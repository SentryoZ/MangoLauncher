//! Bridge between the immediate-mode UI and the async launcher core.
//!
//! egui renders on the main thread and must never block, so every long-running
//! operation (network, hashing, launching) runs on a worker thread and reports
//! back through a channel. The UI drains the channel each frame and calls
//! [`egui::Context::request_repaint`] when work completes.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use mango_core::Result;
use mango_core::install::{PlanRoots, install_instance};
use mango_core::instance::UserInstance;
use mango_core::java::{self, JavaInstallation};
use mango_core::manifest::{Manifest, ServerEntry};
use mango_core::mojang::rules::RuleContext;
use mango_core::net::downloader::Progress as DownloadProgress;
use mango_core::net::http;
use mango_core::presence::{Activity, DiscordIpc, PresenceConfig};
use mango_core::server::ServerStatus;

/// Commands the UI sends to the worker.
#[derive(Debug)]
pub enum Command {
    /// Load a manifest and report its servers.
    LoadManifest { source: String, cache_root: PathBuf },
    /// Install a server's files, streaming progress.
    Install { server: Box<ServerEntry>, roots: PlanRoots },
    /// Install a player-created instance.
    InstallInstance { instance: Box<UserInstance>, roots: PlanRoots },
    /// Launch the game for a server.
    Launch {
        server: Box<ServerEntry>,
        roots: PlanRoots,
        username: String,
        /// Uuid of a signed-in account, or `None` for the offline profile.
        account: Option<String>,
        java: PathBuf,
        detached: bool,
    },
    /// Launch a player-created instance.
    LaunchInstance {
        instance: Box<UserInstance>,
        roots: PlanRoots,
        username: String,
        /// Uuid of a signed-in account, or `None` for the offline profile.
        account: Option<String>,
        detached: bool,
    },
    /// Probe every JVM on the machine.
    DiscoverJava,
    /// Ping a server for its player count and MOTD.
    PingServer { address: String },
    /// Fetch the list of installable Minecraft versions.
    LoadVersions,
    /// Fetch the loader versions for a loader and game version.
    LoadLoaderVersions { kind: mango_core::manifest::LoaderKind, minecraft: String },
    /// Ask the release feed whether a newer launcher exists.
    CheckForUpdate { manifest_url: String, allow_prerelease: bool },
    /// Download and apply an update the UI offered.
    ApplyUpdate { update: Box<mango_core::updater::Update>, staging: PathBuf },
    /// Publish (or clear) the Discord presence card.
    SetPresence {
        config: mango_core::presence::PresenceConfig,
        activity: Box<mango_core::presence::Activity>,
    },
    /// Resolve the Java a Minecraft version needs, from its metadata.
    LoadJavaRequirement { minecraft: String },
    /// Open the browser and sign in with a Microsoft account.
    SignIn,
    /// Remove a stored account and its keyring entry.
    SignOut { uuid: String },
    /// Refresh a stored account's token now.
    RefreshAccount { uuid: String },
}

/// Events the worker sends back to the UI.
#[derive(Debug)]
pub enum Event {
    ManifestLoaded {
        manifest: Arc<Manifest>,
    },
    JavaDiscovered {
        installations: Vec<JavaInstallation>,
    },
    ServerStatus {
        address: String,
        status: Box<ServerStatus>,
    },
    /// Download/verify progress for an install or launch.
    Progress(DownloadProgress),
    /// A launch got as far as spawning the JVM.
    Launched {
        pid: Option<u32>,
    },
    /// The game process ended, detected even when it was launched detached.
    GameExited {
        pid: Option<u32>,
    },
    /// Human-readable log line for the activity panel.
    Log(String),
    Failed {
        context: String,
        message: String,
    },
    Finished {
        context: String,
    },
    /// Installable Minecraft versions, newest first.
    VersionsLoaded {
        versions: Vec<String>,
    },
    /// Loader versions for a loader/game-version pair, newest first.
    LoaderVersionsLoaded {
        kind: mango_core::manifest::LoaderKind,
        minecraft: String,
        versions: Vec<String>,
    },
    /// A newer launcher release is available.
    UpdateAvailable {
        update: Box<mango_core::updater::Update>,
    },
    /// The update feed answered and there is nothing to install.
    UpToDate,
    /// Java major a Minecraft version needs, per its metadata.
    JavaRequirementResolved {
        minecraft: String,
        major: u32,
    },
    /// A sign-in finished; the caller reloads `config` and rebuilds the list.
    SignedIn {
        uuid: String,
        username: String,
    },
    /// The browser URL for a sign-in that is now waiting for the redirect.
    SignInUrl {
        url: String,
    },
    /// An account was removed from the config and the keyring.
    Removed {
        uuid: String,
    },
    /// A token refresh finished.
    Refreshed {
        username: String,
    },
}

/// Handle held by the UI.
#[derive(Clone)]
pub struct RuntimeHandle {
    commands: Sender<Command>,
    events: Receiver<Event>,
    /// Cloned into worker tasks so completed work triggers a repaint.
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl RuntimeHandle {
    /// Spawn the worker thread and build the UI-side handle.
    ///
    /// `data` is where the auth layer keeps `config.json` and reads its client ID;
    /// the worker owns its own copy so a sign-in can persist without touching the
    /// UI's config.
    pub fn spawn(
        data: mango_core::paths::DataPaths,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (command_tx, command_rx) = unbounded::<Command>();
        let (event_tx, event_rx) = unbounded::<Event>();
        let repaint = Arc::new(repaint);

        let worker_events = event_tx.clone();
        let worker_repaint = Arc::clone(&repaint);

        std::thread::Builder::new()
            .name("mango-worker".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("mango-async")
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = worker_events.send(Event::Failed {
                            context: "runtime".to_owned(),
                            message: format!("could not start async runtime: {err}"),
                        });
                        (worker_repaint)();
                        return;
                    }
                };

                let sender = EventSender { events: worker_events, repaint: worker_repaint };
                let mut presence = PresenceHolder::default();
                for command in command_rx {
                    runtime.block_on(run_command(command, &sender, &mut presence, &data));
                }
            })
            .expect("could not spawn the launcher worker thread");

        Self { commands: command_tx, events: event_rx, repaint }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        (self.repaint)();
    }

    /// Non-blocking drain of everything the worker produced.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

/// Keeps the Discord connection alive between commands.
///
/// The socket is deliberately long-lived: reconnecting on every state change
/// would flicker the presence card and cost a handshake each time.
#[derive(Default)]
struct PresenceHolder {
    connection: Option<DiscordIpc>,
    /// Client id the open connection belongs to, so a manifest change reconnects.
    client_id: String,
    /// Last activity sent, so identical updates are not written again.
    last: Option<Activity>,
}

impl PresenceHolder {
    fn publish(
        &mut self,
        config: &PresenceConfig,
        activity: Option<&Activity>,
        sender: &EventSender,
    ) {
        // Disabled: drop the card and the connection.
        if !config.enabled {
            if self.connection.is_some() {
                if let Some(connection) = self.connection.as_mut() {
                    let _ = connection.clear();
                }
                self.connection = None;
                self.client_id.clear();
                self.last = None;
                sender.log("Discord presence disabled");
            }
            return;
        }

        // A manifest change means a different Discord application, so the old
        // handshake is no longer valid.
        if self.connection.is_some() && self.client_id != config.client_id {
            self.connection = None;
            self.last = None;
        }

        if self.connection.is_none() {
            match DiscordIpc::connect(&config.client_id) {
                Ok(connection) => {
                    sender.log("Discord presence connected");
                    self.connection = Some(connection);
                    self.client_id = config.client_id.clone();
                }
                Err(err) => {
                    // Cosmetic: the player does not need an error dialog for this.
                    sender.log(format!("Discord presence unavailable: {err}"));
                    return;
                }
            }
        }

        // Discord rejects commands asynchronously, so a bad client id only shows
        // up on a later reply. Check it before the de-duplication below, or an
        // unchanged activity would skip the check forever.
        if let Some(connection) = self.connection.as_mut()
            && let Some(problem) = connection.take_error()
        {
            sender.log(format!("Discord rejected the presence: {problem}"));
            self.last = None;
            return;
        }

        if self.last.as_ref() == activity {
            return;
        }

        if let Some(connection) = self.connection.as_mut() {
            match connection.set_activity(activity) {
                Ok(()) => self.last = activity.cloned(),
                Err(err) => {
                    sender.log(format!("Discord presence stopped: {err}"));
                    self.connection = None;
                    self.last = None;
                }
            }
        }
    }
}

#[derive(Clone)]
struct EventSender {
    events: Sender<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl EventSender {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        // The UI is idle waiting for input; wake it so the event is drawn.
        (self.repaint)();
    }

    fn log(&self, message: impl Into<String>) {
        self.send(Event::Log(message.into()));
    }

    fn failed(&self, context: impl Into<String>, err: impl std::fmt::Display) {
        self.send(Event::Failed { context: context.into(), message: err.to_string() });
    }
}

async fn run_command(
    command: Command,
    sender: &EventSender,
    presence: &mut PresenceHolder,
    data: &mango_core::paths::DataPaths,
) {
    match command {
        Command::LoadManifest { source, cache_root } => {
            let result = load_manifest(&source, &cache_root).await;
            match result {
                Ok(manifest) => {
                    sender.log(format!(
                        "manifest loaded: {} ({} servers)",
                        manifest.name,
                        manifest.servers.len()
                    ));
                    sender.send(Event::ManifestLoaded { manifest: Arc::new(manifest) });
                }
                Err(err) => sender.failed("manifest", err),
            }
        }
        Command::Install { server, roots } => {
            sender.log(format!("installing {}", server.name));
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("install", err),
            };

            let progress = ProgressBridge { sender };
            let outcome = install_instance(
                &client,
                &server.to_instance_spec(),
                &roots,
                &RuleContext::new(),
                None,
                &mango_core::install::InstallOptions::default(),
                &|event| progress.on(event),
            )
            .await;

            match outcome {
                Ok(outcome) => {
                    sender.log(format!(
                        "installed {} files ({} downloaded, {} mods)",
                        outcome.plan.files.len(),
                        outcome.downloaded,
                        outcome.mods.resolved.len()
                    ));
                    for failure in &outcome.mods.failures {
                        sender.log(format!("mod {} skipped: {}", failure.id, failure.reason));
                    }
                    sender.send(Event::Finished { context: "install".to_owned() });
                }
                Err(err) => sender.failed("install", err),
            }
        }
        Command::Launch { server, roots, username, account, java, detached } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("launch", err),
            };

            let java = match select_java(&client, &server, java, sender).await {
                Ok(java) => java,
                Err(err) => return sender.failed("launch", err),
            };
            sender.log(format!("launching {} with {}", server.name, java.display()));

            let (profile, agents) =
                match resolve_account(&client, data, account.as_deref(), username, sender).await {
                    Ok(resolved) => resolved,
                    Err(err) => return sender.failed("launch", err),
                };

            let spec = mango_core::launch::LaunchSpec {
                minecraft_version: server.minecraft.clone(),
                version_name: server.id.clone(),
                game_directory: roots.instance_dir.clone(),
                java_executable: java,
                account: profile,
                loader: server.loader.clone(),
                detached,
                resolution: Some((1280, 720)),
                fullscreen: false,
                jvm_options: Vec::new(),
                java_agents: agents,
                auto_connect: server
                    .auto_connect
                    .then(|| server.host_port())
                    .map(|(host, port)| format!("{host}:{port}")),
            };

            let progress = ProgressBridge { sender };
            let process = mango_core::launch::service::launch(
                &client,
                &spec,
                &roots,
                &RuleContext::new(),
                &std::env::temp_dir(),
            )
            .await;
            let _ = progress;

            match process {
                Ok(process) => {
                    let pid = process.pid();
                    sender.log(format!("game started (pid {pid:?})"));
                    sender.send(Event::Launched { pid });
                    if !detached {
                        let exit = process.wait().await;
                        match exit {
                            Ok(code) => sender.log(format!("game exited with {code:?}")),
                            Err(err) => sender.failed("launch", err),
                        }
                        sender.send(Event::Finished { context: "launch".to_owned() });
                    } else {
                        // Detached: the handle is dropped, so the exit has to be
                        // detected another way or the presence would claim the
                        // game is running forever.
                        watch_detached_exit(pid, sender);
                    }
                }
                Err(err) => sender.failed("launch", err),
            }
        }
        Command::DiscoverJava => {
            let installations = java::discover::discover();
            sender.log(format!("found {} Java runtime(s)", installations.len()));
            sender.send(Event::JavaDiscovered { installations });
        }
        Command::InstallInstance { instance, roots } => {
            sender.log(format!("installing {}", instance.name));
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("install", err),
            };

            let progress = ProgressBridge { sender };
            let outcome = install_instance(
                &client,
                &instance.to_spec(),
                &roots,
                &RuleContext::new(),
                None,
                &mango_core::install::InstallOptions::default(),
                &|event| progress.on(event),
            )
            .await;

            match outcome {
                Ok(outcome) => {
                    sender.log(format!(
                        "installed {} files ({} downloaded, {} mods)",
                        outcome.plan.files.len(),
                        outcome.downloaded,
                        outcome.mods.resolved.len()
                    ));
                    for failure in &outcome.mods.failures {
                        sender.log(format!("mod {} skipped: {}", failure.id, failure.reason));
                    }
                    sender.send(Event::Finished { context: "install".to_owned() });
                }
                Err(err) => sender.failed("install", err),
            }
        }
        Command::LaunchInstance { instance, roots, username, account, detached } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("launch", err),
            };

            let java = read_java(&client, &instance, sender).await;
            let java = match java {
                Ok(java) => java,
                Err(err) => return sender.failed("launch", err),
            };
            sender.log(format!("launching {} with {}", instance.name, java.display()));

            let (profile, agents) =
                match resolve_account(&client, data, account.as_deref(), username.clone(), sender)
                    .await
                {
                    Ok(resolved) => resolved,
                    Err(err) => return sender.failed("launch", err),
                };

            crate::runtime::launch_user_instance(
                &client, &instance, &roots, profile, agents, java, detached, sender,
            )
            .await;
        }
        Command::LoadVersions => match load_versions().await {
            Ok(versions) => {
                sender.log(format!("{} Minecraft versions available", versions.len()));
                sender.send(Event::VersionsLoaded { versions });
            }
            Err(err) => sender.failed("versions", err),
        },
        Command::LoadLoaderVersions { kind, minecraft } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("loader versions", err),
            };

            match mango_core::loaders::available_versions(&client, kind, &minecraft).await {
                Ok(versions) => {
                    sender.send(Event::LoaderVersionsLoaded { kind, minecraft, versions })
                }
                Err(err) => {
                    // A loader with no builds for a release is a normal answer,
                    // not a failure worth interrupting the player for.
                    sender.log(format!("no {} builds for {minecraft}: {err}", kind.as_str()));
                    sender.send(Event::LoaderVersionsLoaded {
                        kind,
                        minecraft,
                        versions: Vec::new(),
                    });
                }
            }
        }
        Command::PingServer { address } => {
            match mango_core::server::ping(&address, std::time::Duration::from_secs(5)).await {
                Ok(status) => {
                    sender.send(Event::ServerStatus { address, status: Box::new(status) })
                }
                Err(err) => sender.log(format!("status for {address}: {err}")),
            }
        }
        Command::CheckForUpdate { manifest_url, allow_prerelease } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("update", err),
            };

            match mango_core::updater::check(&client, &manifest_url, allow_prerelease).await {
                Ok(Some(update)) => {
                    sender.log(format!("MangoLauncher {} is available", update.version));
                    sender.send(Event::UpdateAvailable { update: Box::new(update) });
                }
                Ok(None) => sender.send(Event::UpToDate),
                Err(err) => {
                    // A release feed that is down or misconfigured must not stop
                    // anyone from playing, so this is a log line, not an error.
                    sender.log(format!("could not check for updates: {err}"));
                    sender.send(Event::UpToDate);
                }
            }
        }
        Command::ApplyUpdate { update, staging } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("update", err),
            };

            match mango_core::updater::download(&client, &update, &staging).await {
                Ok(downloaded) => {
                    sender.log(format!("downloaded {}", downloaded.display()));
                    match std::env::current_exe() {
                        Ok(current) => {
                            match mango_core::updater::apply(&update, &downloaded, &current) {
                                Ok(true) => sender.log(format!(
                                    "MangoLauncher {} is ready; restart to use it",
                                    update.version
                                )),
                                Ok(false) => sender.log("the download was opened"),
                                Err(err) => sender.failed("update", err),
                            }
                        }
                        Err(err) => sender.failed(
                            "update",
                            mango_core::MangoError::Other(format!(
                                "cannot locate this executable: {err}"
                            )),
                        ),
                    }
                }
                Err(err) => sender.failed("update", err),
            }
        }
        Command::SetPresence { config, activity } => {
            presence.publish(&config, Some(&activity), sender);
        }
        Command::LoadJavaRequirement { minecraft } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("java", err),
            };

            // Ask the metadata, not the version id: `26.3` needs Java 25 while the
            // id itself says nothing about a runtime.
            let spec = mango_core::launch::LaunchSpec {
                minecraft_version: minecraft.clone(),
                version_name: minecraft.clone(),
                game_directory: PathBuf::new(),
                java_executable: PathBuf::new(),
                account: mango_core::launch::LaunchAccount::offline("Player"),
                loader: mango_core::manifest::LoaderSpec::default(),
                detached: true,
                resolution: None,
                fullscreen: false,
                jvm_options: Vec::new(),
                java_agents: Vec::new(),
                auto_connect: None,
            };

            match mango_core::launch::service::java_requirement(&client, &spec).await {
                Ok(requirement) => sender.send(Event::JavaRequirementResolved {
                    minecraft,
                    major: requirement.suggested,
                }),
                Err(err) => {
                    // A version without metadata still has to show something, so
                    // this is a log line rather than a failure dialog.
                    sender.log(format!("could not read the Java version for {minecraft}: {err}"));
                }
            }
        }
        Command::SignIn => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("sign-in", err),
            };

            // The browser is opened by the auth layer; the worker only reports the
            // outcome. Config is re-read here so a sign-in started from a stale UI
            // does not resurrect removed accounts.
            let mut config = match mango_core::config::Config::load(data) {
                Ok(config) => config,
                Err(err) => return sender.failed("sign-in", err),
            };

            sender.log("opening the browser to sign in…");
            match mango_core::auth::sign_in_with_hint(&client, data, &mut config, |url| {
                // Sent before the browser opens, so the URL is visible even when the
                // launch fails or the player's browser never appears.
                sender.send(Event::SignInUrl { url: url.to_owned() });
            })
            .await
            {
                Ok(meta) => {
                    sender.send(Event::SignedIn {
                        uuid: meta.uuid.clone(),
                        username: meta.username.clone(),
                    });
                    sender.log(format!("signed in as {}", meta.username));
                }
                Err(err) => {
                    // Sign-in failures are the ones a player can act on (game not
                    // owned, app not approved, browser closed), so the message is
                    // shown rather than only logged.
                    sender.failed("sign-in", mango_core::auth::describe_error(&err));
                }
            }
        }
        Command::SignOut { uuid } => {
            let mut config = match mango_core::config::Config::load(data) {
                Ok(config) => config,
                Err(err) => return sender.failed("sign-out", err),
            };

            match mango_core::auth::remove_account(data, &mut config, &uuid) {
                Ok(()) => {
                    sender.send(Event::Removed { uuid });
                    sender.log("account removed");
                }
                Err(err) => sender.failed("sign-out", err),
            }
        }
        Command::RefreshAccount { uuid } => {
            let client = match http::build_client() {
                Ok(client) => client,
                Err(err) => return sender.failed("refresh", err),
            };

            let mut config = match mango_core::config::Config::load(data) {
                Ok(config) => config,
                Err(err) => return sender.failed("refresh", err),
            };

            match mango_core::auth::ensure_fresh(&client, data, &mut config, &uuid).await {
                Ok(_) => {
                    let username = config
                        .accounts
                        .get(&uuid)
                        .map(|meta| meta.username.clone())
                        .unwrap_or_default();
                    sender.send(Event::Refreshed { username: username.clone() });
                    sender.log(format!("refreshed the token for {username}"));
                }
                Err(err) => sender.failed("refresh", mango_core::auth::describe_error(&err)),
            }
        }
    }
}

struct ProgressBridge<'a> {
    sender: &'a EventSender,
}

impl ProgressBridge<'_> {
    fn on(&self, event: DownloadProgress) {
        self.sender.send(Event::Progress(event));
    }
}

async fn load_manifest(source: &str, cache_root: &std::path::Path) -> Result<Manifest> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = http::build_client()?;
        let cache = mango_core::manifest::cache_path_for(cache_root, source);
        let fetched = mango_core::manifest::fetch(&client, source, &cache, true).await?;
        Ok(fetched.manifest)
    } else {
        Ok(mango_core::manifest::load_local(std::path::Path::new(source))?.manifest)
    }
}

/// Pick (or download) the JVM a user instance needs.
async fn read_java(
    client: &mango_core::net::http::Client,
    instance: &UserInstance,
    sender: &EventSender,
) -> Result<PathBuf> {
    // Build a spec first: the requirement comes from the version metadata, which
    // is the only reliable source (`26.3` needs Java 25, 1.20.1 needs 17).
    let spec = mango_core::launch::LaunchSpec {
        minecraft_version: instance.minecraft.clone(),
        version_name: instance.id.clone(),
        game_directory: PathBuf::new(),
        java_executable: PathBuf::new(),
        account: mango_core::launch::LaunchAccount::offline("Player"),
        loader: instance.loader.clone(),
        detached: true,
        resolution: None,
        fullscreen: false,
        jvm_options: Vec::new(),
        java_agents: Vec::new(),
        auto_connect: None,
    };

    let requirement = mango_core::launch::service::java_requirement(client, &spec).await?;
    let installations = java::discover::discover();

    sender.log(format!(
        "{} needs Java {} (found: {})",
        instance.minecraft,
        requirement.suggested,
        if installations.is_empty() {
            "none".to_owned()
        } else {
            installations.iter().map(|java| java.version.clone()).collect::<Vec<_>>().join(", ")
        }
    ));

    let java_dir = std::env::temp_dir().join("mango-cache").join("java");
    mango_core::launch::service::ensure_java(client, &requirement, &installations, None, &java_dir)
        .await
}

/// Build the account a launch runs as, plus any JVM arguments it needs.
///
/// A signed-in account gets its real Minecraft token, refreshed if needed. The
/// offline profile is the fallback, which is what a player with no account, or a
/// LAN game, uses.
async fn resolve_account(
    client: &mango_core::net::http::Client,
    data: &mango_core::paths::DataPaths,
    uuid: Option<&str>,
    username: String,
    sender: &EventSender,
) -> Result<(mango_core::launch::LaunchAccount, Vec<String>)> {
    let Some(uuid) = uuid else {
        sender.log(format!("launching as the offline profile {username}"));
        return Ok((mango_core::launch::LaunchAccount::offline(&username), Vec::new()));
    };

    // Re-read the config: the token may have expired since the UI last looked, and
    // `launch_profile` refreshes and persists it.
    let mut config = mango_core::config::Config::load(data)?;
    match mango_core::auth::launch_profile(client, data, &mut config, uuid).await {
        Ok((account, agents)) => {
            let kind = if agents.is_empty() { "online" } else { "online (authlib agent)" };
            sender.log(format!("launching as {} ({kind})", account.username));
            Ok((account, agents))
        }
        Err(err) => {
            // Falling back to offline would let the player into single player but
            // fail the moment they join a server, with a confusing message. Report
            // it instead.
            Err(mango_core::MangoError::Other(format!(
                "could not sign this launch in: {}",
                mango_core::auth::describe_error(&err)
            )))
        }
    }
}

/// Spawn a user instance's game process.
#[allow(clippy::too_many_arguments)]
async fn launch_user_instance(
    client: &mango_core::net::http::Client,
    instance: &UserInstance,
    roots: &PlanRoots,
    account: mango_core::launch::LaunchAccount,
    java_agents: Vec<String>,
    java: PathBuf,
    detached: bool,
    sender: &EventSender,
) {
    let spec = mango_core::launch::LaunchSpec {
        minecraft_version: instance.minecraft.clone(),
        version_name: instance.id.clone(),
        game_directory: roots.instance_dir.clone(),
        java_executable: java,
        account,
        loader: instance.loader.clone(),
        detached,
        resolution: Some((1280, 720)),
        fullscreen: false,
        jvm_options: instance.jvm_args.clone(),
        java_agents,
        auto_connect: None,
    };

    if let Err(err) = mango_core::launch::service::validate_spec(&spec) {
        return sender.failed("launch", err);
    }

    match mango_core::launch::service::launch(
        client,
        &spec,
        roots,
        &RuleContext::new(),
        &std::env::temp_dir(),
    )
    .await
    {
        Ok(process) => {
            let pid = process.pid();
            sender.log(format!("game started (pid {pid:?})"));
            sender.send(Event::Launched { pid });
            if !detached {
                match process.wait().await {
                    Ok(code) => sender.log(format!("game exited with {code:?}")),
                    Err(err) => sender.failed("launch", err),
                }
                sender.send(Event::Finished { context: "launch".to_owned() });
            } else {
                watch_detached_exit(pid, sender);
            }
        }
        Err(err) => sender.failed("launch", err),
    }
}

/// Report when a detached game closes.
///
/// The launcher has no handle to the process after a detached launch, so the pid
/// is polled on a background thread. Polling stops as soon as the process is gone
/// or the event channel is closed (the launcher exited).
fn watch_detached_exit(pid: Option<u32>, sender: &EventSender) {
    let Some(pid) = pid else { return };

    let sender = sender.clone();
    std::thread::Builder::new()
        .name("mango-game-watch".to_owned())
        .spawn(move || {
            // One second is frequent enough for a presence timer and cheap enough
            // to leave running for a long session.
            mango_core::launch::process::wait_for_exit(pid, std::time::Duration::from_secs(1));
            sender.send(Event::GameExited { pid: Some(pid) });
            sender.log("game exited");
        })
        .ok();
}

/// Fetch release version ids, newest first.
async fn load_versions() -> Result<Vec<String>> {
    let client = http::build_client()?;
    let manifest: mango_core::mojang::VersionManifest =
        http::get_json(&client, mango_core::install::service::VERSION_MANIFEST_URL).await?;
    Ok(manifest
        .versions
        .iter()
        .filter(|version| version.kind == "release")
        .map(|version| version.id.clone())
        .collect())
}

/// Build plan roots for a server.
pub fn roots_for(game_root: &std::path::Path, server_id: &str) -> PlanRoots {
    PlanRoots::new(game_root.join("common"), game_root.join("instances").join(server_id))
}

/// Pick (or download) the JVM this instance needs.
///
/// Delegates to the same selector the CLI uses, so both front ends agree on which
/// Java a given Minecraft version requires.
async fn select_java(
    client: &mango_core::net::http::Client,
    server: &ServerEntry,
    preferred: PathBuf,
    sender: &EventSender,
) -> Result<PathBuf> {
    use mango_core::java::JavaChoice;

    // Read the requirement the same way the launch will: from the version
    // metadata, which is the only reliable source. Guessing from the version id
    // gets `26.3` wrong (it needs Java 25 but the id carries no hint), and the
    // wrong runtime surfaces as `Unsupported class file major version`.
    let probe = mango_core::launch::LaunchSpec {
        minecraft_version: server.minecraft.clone(),
        version_name: server.id.clone(),
        game_directory: PathBuf::new(),
        java_executable: PathBuf::new(),
        account: mango_core::launch::LaunchAccount::offline("Player"),
        loader: server.loader.clone(),
        detached: true,
        resolution: None,
        fullscreen: false,
        jvm_options: Vec::new(),
        java_agents: Vec::new(),
        auto_connect: None,
    };

    let requirement = mango_core::launch::service::java_requirement(client, &probe).await?;
    let installations = java::discover::discover();

    sender.log(format!(
        "{} needs Java {} (found: {})",
        server.minecraft,
        requirement.suggested,
        if installations.is_empty() {
            "none".to_owned()
        } else {
            installations.iter().map(|java| java.version.clone()).collect::<Vec<_>>().join(", ")
        }
    ));

    // The runtime picked in Settings acts as a pin.
    let pinned = preferred.to_str();

    match java::choose_java(&installations, &requirement, pinned) {
        JavaChoice::Installed(java) => Ok(java.executable),
        JavaChoice::NeedsDownload { major, reason } => {
            sender.log(format!("{reason}; downloading Java {major}"));
            let java_dir = std::env::temp_dir().join("mango-cache").join("java");
            let download = java::resolve_jdk(client, major).await?;
            java::install_jdk(client, &java_dir, &download, major).await
        }
    }
}
