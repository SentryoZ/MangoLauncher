//! UI state and the reducers that update it.
//!
//! Deliberately free of egui types so the whole state machine is unit-testable
//! without a window: every [`Event`] maps to a deterministic transition.

use std::collections::VecDeque;
use std::sync::Arc;

use mango_core::config::Config;
use mango_core::instance::{UserInstance, list_user_instances};
use mango_core::java::JavaInstallation;
use mango_core::manifest::{LoaderKind, LoaderSpec, Manifest, ServerEntry};
use mango_core::net::downloader::Progress as DownloadProgress;
use mango_core::server::ServerStatus;

use crate::runtime::Event;

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Landing,
    Instances,
    Modpacks,
    Settings,
    Logs,
}

/// Everything the UI renders.
#[derive(Debug, Default)]
pub struct UiState {
    pub view: View,
    pub manifest: Option<Arc<Manifest>>,
    pub manifest_source: String,
    pub loading_manifest: bool,
    pub selected_server: Option<String>,
    /// Player-created instances, reloaded whenever one is added or removed.
    pub user_instances: Vec<UserInstance>,
    /// Instance selected on the Instances screen.
    pub selected_instance: Option<String>,
    /// Minecraft versions offered by the instance form.
    pub available_versions: Vec<String>,
    /// Loader versions for the form's current loader + game version.
    pub loader_versions: Vec<String>,
    /// Set while `loader_versions` is being fetched.
    pub loading_loader_versions: bool,
    /// Whether the player is creating or editing an instance.
    pub showing_form: bool,
    /// In-progress instance form.
    pub new_instance: InstanceForm,
    /// Signed-in accounts plus the implicit offline profile.
    pub accounts: AccountList,
    pub selected_account: Option<usize>,
    pub java: Vec<JavaInstallation>,
    pub selected_java: Option<usize>,
    pub status: Option<ServerStatus>,
    pub pinging: bool,
    pub progress: Option<ProgressState>,
    pub busy: bool,
    pub logs: LogBuffer,
    pub error: Option<String>,
    pub launch_pid: Option<u32>,
    pub settings: SettingsDraft,
    /// Newer launcher release the feed advertised, if any.
    pub available_update: Option<mango_core::updater::Update>,
    /// Set once the player dismissed the update banner for this session.
    pub update_dismissed: bool,
    /// When the current game launch started, for the Discord elapsed timer.
    pub launch_started_at: Option<u64>,
    /// The presence card last handed to the worker, so an unchanged one is not
    /// resent every frame.
    pub last_presence:
        Option<(mango_core::presence::PresenceConfig, mango_core::presence::Activity)>,
    /// Id of the instance the running game was started from, if any.
    pub running_instance_id: Option<String>,
    /// Java major per Minecraft version, read from metadata.
    pub java_requirements: std::collections::HashMap<String, u32>,
    /// Uuid the account list should select once the config is reloaded.
    pub pending_select_account: Option<String>,
    /// Authorisation URL for a sign-in that is waiting for the browser redirect.
    pub sign_in_url: Option<String>,
    /// Modpack browser.
    pub packs: PackBrowser,
    /// Set when an install finished, so the app rescans the instances directory.
    pub pending_reload_instances: bool,
}

impl UiState {
    pub fn new(config: &Config) -> Self {
        let offline = default_username();
        let accounts = AccountList::from_config(config, &offline);

        // Preselect the account the config remembers, falling back to the offline
        // profile so PLAY is usable before anyone signs in.
        let selected_account = config
            .selected_account
            .as_deref()
            .and_then(|uuid| accounts.index_of(uuid))
            .or_else(|| Some(accounts.offline_index()));

        Self {
            manifest_source: config.settings.launcher.manifest_url.clone().unwrap_or_default(),
            settings: SettingsDraft::from_config(config),
            accounts,
            selected_account,
            ..Default::default()
        }
    }

    /// Rebuild the account list from the config after a sign-in or removal.
    ///
    /// `keep` is the uuid to select when it still exists; otherwise the offline
    /// profile is selected.
    pub fn reload_accounts(&mut self, config: &Config, keep: Option<&str>) {
        let offline = default_username();
        self.accounts = AccountList::from_config(config, &offline);
        self.selected_account = keep
            .and_then(|uuid| self.accounts.index_of(uuid))
            .or_else(|| Some(self.accounts.offline_index()));
    }

    /// Reload user instances from disk.
    pub fn reload_instances(&mut self, instances_dir: &std::path::Path) {
        self.user_instances = list_user_instances(instances_dir);
        let keep = self
            .selected_instance
            .as_deref()
            .filter(|id| self.user_instances.iter().any(|instance| instance.id == *id))
            .map(str::to_owned);
        self.selected_instance = keep.or_else(|| self.user_instances.first().map(|i| i.id.clone()));
    }

    pub fn selected_user_instance(&self) -> Option<&UserInstance> {
        let id = self.selected_instance.as_deref()?;
        self.user_instances.iter().find(|instance| instance.id == id)
    }

    /// The instance the running game belongs to, if it was started from one.
    ///
    /// A game started from the server list has no instance id, so this returns
    /// `None` and the presence falls back to the selected server.
    pub fn running_instance(&self) -> Option<&UserInstance> {
        let id = self.running_instance_id.as_deref()?;
        self.user_instances.iter().find(|instance| instance.id == id)
    }

    /// Ids already in use, for id-collision-free suggestions.
    pub fn existing_instance_ids(&self) -> Vec<String> {
        self.user_instances.iter().map(|instance| instance.id.clone()).collect()
    }

    /// Whether the Instances screen has something to launch.
    pub fn can_launch_instance(&self) -> bool {
        !self.busy && self.selected_user_instance().is_some() && self.selected_username().is_some()
    }

    pub fn selected_server_entry(&self) -> Option<&ServerEntry> {
        let manifest = self.manifest.as_ref()?;
        let id = self.selected_server.as_deref()?;
        manifest.server(id)
    }

    pub fn selected_username(&self) -> Option<&str> {
        self.accounts.selected(self.selected_account).map(Account::username)
    }

    /// The selected account when it is a real signed-in one.
    ///
    /// `None` for the offline profile, which is what launch uses to decide whether
    /// to fetch a token.
    pub fn selected_signed_in(&self) -> Option<&SignedInAccount> {
        match self.accounts.selected(self.selected_account) {
            Some(Account::Microsoft(account)) => Some(account),
            _ => None,
        }
    }

    pub fn selected_java_install(&self) -> Option<&JavaInstallation> {
        self.selected_java.and_then(|index| self.java.get(index))
    }

    /// Apply one worker event.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::ManifestLoaded { manifest } => {
                self.loading_manifest = false;
                self.error = None;
                self.set_manifest(manifest);
            }
            Event::JavaDiscovered { installations } => {
                // Keep the current pick when it still exists.
                let previous = self.selected_java_install().map(|java| java.executable.clone());
                self.java = installations;
                self.selected_java = previous
                    .and_then(|path| self.java.iter().position(|java| java.executable == path))
                    .or_else(|| {
                        self.java
                            .iter()
                            .position(|java| java.major().is_some_and(|major| major >= 17))
                            .or(if self.java.is_empty() { None } else { Some(0) })
                    });
            }
            Event::ServerStatus { address, status } => {
                // Drop a reply for a server the user already switched away from.
                if let Some(server) = self.selected_server_entry()
                    && server.address == address
                {
                    self.status = Some(*status);
                }
                self.pinging = false;
            }
            Event::Progress(progress) => {
                self.progress = Some(ProgressState::from_progress(&progress));
                if matches!(progress, DownloadProgress::Finished { .. }) {
                    self.progress = None;
                }
            }
            Event::Launched { pid } => {
                self.launch_pid = pid;
                self.busy = false;
                self.progress = None;
                self.error = None;
            }
            Event::GameExited { pid } => {
                // Ignore a late report for a launch the player already replaced.
                if pid.is_none() || pid == self.launch_pid {
                    self.launch_pid = None;
                    self.launch_started_at = None;
                    self.running_instance_id = None;
                }
            }
            Event::Log(line) => self.logs.push(line),
            Event::Failed { context, message } => {
                self.busy = false;
                self.loading_manifest = false;
                self.pinging = false;
                self.progress = None;
                // A failed sign-in or install leaves no work in flight, so the
                // spinner and its banner must not stay on screen.
                if context == "sign-in" {
                    self.sign_in_url = None;
                }
                if context == "modpack" || context == "modpacks" {
                    self.packs.installing = false;
                    self.packs.searching = false;
                    self.packs.loading_versions = false;
                }
                self.error = Some(format!("{context}: {message}"));
                self.logs.push(format!("ERROR ({context}): {message}"));
            }
            Event::Finished { context } => {
                self.busy = false;
                self.progress = None;
                self.logs.push(format!("{context} finished"));
            }
            Event::VersionsLoaded { versions } => {
                // Keep the player's pick when it still exists, otherwise default
                // to the newest release.
                let keep = if versions.contains(&self.new_instance.minecraft) {
                    Some(self.new_instance.minecraft.clone())
                } else {
                    None
                };
                self.new_instance.minecraft =
                    keep.or_else(|| versions.first().cloned()).unwrap_or_default();
                self.available_versions = versions;
            }
            Event::LoaderVersionsLoaded { kind, minecraft, versions } => {
                self.loading_loader_versions = false;
                // A late reply for a loader/version the player already changed
                // would put the wrong list in the dropdown.
                if kind != self.new_instance.loader || minecraft != self.new_instance.minecraft {
                    return;
                }
                // Keep a pinned version only when it is still offered.
                if !self.new_instance.loader_version.is_empty()
                    && !versions.contains(&self.new_instance.loader_version)
                {
                    self.new_instance.loader_version.clear();
                }
                self.loader_versions = versions;
            }
            Event::UpdateAvailable { update } => {
                self.logs.push(format!("MangoLauncher {} is available", update.version));
                self.available_update = Some(*update);
            }
            Event::UpToDate => {}
            Event::JavaRequirementResolved { minecraft, major } => {
                self.java_requirements.insert(minecraft, major);
            }
            Event::SignedIn { uuid, username } => {
                // The list itself is rebuilt by the app, which owns the config.
                self.logs.push(format!("signed in as {username}"));
                self.pending_select_account = Some(uuid);
                self.sign_in_url = None;
                self.error = None;
            }
            Event::SignInUrl { url } => {
                // Kept visible until it completes, so a browser that never opened is
                // recoverable by copying the URL.
                self.sign_in_url = Some(url);
            }
            Event::Removed { uuid } => {
                self.logs.push(format!("removed account {uuid}"));
                self.pending_select_account = None;
            }
            Event::Refreshed { username, .. } => {
                self.logs.push(format!("refreshed the token for {username}"));
                self.error = None;
            }
            Event::PacksFound { packs } => {
                self.packs.searching = false;
                self.packs.results = packs;
                self.error = None;
            }
            Event::PackVersionsLoaded { project, versions } => {
                self.packs.loading_versions = false;

                // Ignore a reply for a pack the player already navigated away from.
                if self.packs.open.as_deref() != Some(project.as_str()) {
                    return;
                }

                // Default to what a player would pick, so Install works without
                // reading the list.
                let preferred = mango_core::mods::modrinth_pack::preferred_version(&versions, None);
                self.packs.selected_version = preferred.map(|version| version.id.clone());
                self.packs.versions = versions;
            }
            Event::PackInstalled { instance_id } => {
                self.packs.installing = false;
                self.packs.open = None;
                self.logs.push(format!("installed modpack as {instance_id}"));
                self.pending_reload_instances = true;
            }
        }
    }

    /// Java major a version needs, when the metadata reply has arrived.
    ///
    /// Returns `None` rather than a guess: showing the wrong runtime is what
    /// produced `Unsupported class file major version` in the first place.
    pub fn java_major_for(&self, minecraft: &str) -> Option<u32> {
        self.java_requirements.get(minecraft).copied()
    }

    /// Replace the manifest, keeping the selection when it is still valid.
    pub fn set_manifest(&mut self, manifest: Arc<Manifest>) {
        let keep = self
            .selected_server
            .as_deref()
            .filter(|id| manifest.server(id).is_some())
            .map(str::to_owned);

        self.selected_server = keep.or_else(|| manifest.default_server().map(|s| s.id.clone()));
        self.manifest = Some(manifest);
        self.status = None;
    }

    pub fn select_server(&mut self, id: impl Into<String>) {
        self.selected_server = Some(id.into());
        self.status = None;
    }

    /// Whether PLAY should be enabled.
    pub fn can_launch(&self) -> bool {
        !self.busy
            && self.manifest.is_some()
            && self.selected_server_entry().is_some()
            && self.selected_username().is_some()
    }
}

/// The instance editor, used for both creating and editing.
///
/// One form keeps the two flows identical; the only difference is that `editing`
/// names the instance being changed and the id is therefore not recomputed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceForm {
    /// Instance being edited; `None` when creating a new one.
    pub editing: Option<String>,
    pub name: String,
    pub minecraft: String,
    pub loader: LoaderKind,
    /// Loader version; empty means "newest compatible with the game version".
    pub loader_version: String,
}

impl Default for InstanceForm {
    fn default() -> Self {
        Self {
            editing: None,
            name: String::new(),
            // Filled from `available_versions` once the list arrives.
            minecraft: String::new(),
            loader: LoaderKind::Vanilla,
            loader_version: String::new(),
        }
    }
}

impl InstanceForm {
    /// Pre-fill the form from an existing instance.
    pub fn editing(instance: &UserInstance) -> Self {
        Self {
            editing: Some(instance.id.clone()),
            name: instance.name.clone(),
            minecraft: instance.minecraft.clone(),
            loader: instance.loader.kind,
            loader_version: instance.loader.version.clone().unwrap_or_default(),
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// A name is optional — it falls back to the id — so only the game version
    /// is required.
    pub fn is_valid(&self) -> bool {
        !self.minecraft.trim().is_empty()
    }

    /// The loader spec this form describes.
    pub fn loader_spec(&self) -> LoaderSpec {
        LoaderSpec {
            kind: self.loader,
            // Vanilla has no loader version, and an empty box means "latest".
            version: if self.loader == LoaderKind::Vanilla || self.loader_version.trim().is_empty()
            {
                None
            } else {
                Some(self.loader_version.trim().to_owned())
            },
        }
    }

    /// Build a new instance, or explain why it cannot be built.
    pub fn build(&self, existing_ids: &[String]) -> Result<UserInstance, String> {
        if !self.is_valid() {
            return Err("pick a Minecraft version".to_owned());
        }
        let id = mango_core::instance::suggest_id(&self.name, existing_ids);
        UserInstance::new(&id, &self.name, &self.minecraft, self.loader_spec())
            .map_err(|err| err.to_string())
    }

    /// Apply this form to an existing instance.
    pub fn apply_to(&self, instance: &mut UserInstance) -> Result<(), String> {
        if !self.is_valid() {
            return Err("pick a Minecraft version".to_owned());
        }
        instance
            .update(mango_core::instance::InstanceEdit {
                name: self.name.clone(),
                minecraft: self.minecraft.clone(),
                loader: self.loader_spec(),
                jvm_args: instance.jvm_args.clone(),
            })
            .map_err(|err| err.to_string())
    }
}

/// A single-line progress summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressState {
    pub label: String,
    pub done: usize,
    pub total: usize,
    pub fraction: f32,
}

impl ProgressState {
    pub fn from_progress(progress: &DownloadProgress) -> Self {
        match progress {
            DownloadProgress::Verifying { done, total } => Self {
                label: format!("Verifying files {done}/{total}"),
                done: *done,
                total: *total,
                fraction: ratio(*done as f32, *total as f32),
            },
            DownloadProgress::Downloading { done, total, bytes_done, bytes_total, current } => {
                let name = current.file_name().and_then(|name| name.to_str()).unwrap_or("file");
                Self {
                    label: format!("Downloading {name} ({done}/{total})"),
                    done: *done,
                    total: *total,
                    // Byte progress is smoother than file counts for big jars.
                    fraction: if *bytes_total > 0 {
                        ratio(*bytes_done as f32, *bytes_total as f32)
                    } else {
                        ratio(*done as f32, *total as f32)
                    },
                }
            }
            DownloadProgress::Finished { downloaded, skipped } => Self {
                label: format!("Done — {downloaded} downloaded, {skipped} already present"),
                done: *downloaded + *skipped,
                total: *downloaded + *skipped,
                fraction: 1.0,
            },
        }
    }
}

fn ratio(numerator: f32, denominator: f32) -> f32 {
    if denominator <= 0.0 { 0.0 } else { (numerator / denominator).clamp(0.0, 1.0) }
}

/// Bounded log buffer: old lines are dropped so a chatty game cannot grow
/// memory without limit.
#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    capacity: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self { lines: VecDeque::new(), capacity: 2000 }
    }
}

impl LogBuffer {
    /// Smaller buffer, used by tests to exercise eviction.
    #[cfg(test)]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { lines: VecDeque::new(), capacity: capacity.max(1) }
    }

    pub fn push(&mut self, line: String) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Last line, shown in a compact status area.
    pub fn last(&self) -> Option<&str> {
        self.lines.back().map(String::as_str)
    }
}

/// Editable copy of the persisted settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDraft {
    pub res_width: u32,
    pub res_height: u32,
    pub fullscreen: bool,
    pub auto_connect: bool,
    pub launch_detached: bool,
    pub ram_mb: u32,
    pub java_path: String,
    pub jvm_options: String,
    pub manifest_url: String,
    pub enable_discord_rpc: bool,
}

impl Default for SettingsDraft {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl SettingsDraft {
    pub fn from_config(config: &Config) -> Self {
        Self {
            res_width: config.settings.game.res_width,
            res_height: config.settings.game.res_height,
            fullscreen: config.settings.game.fullscreen,
            auto_connect: config.settings.game.auto_connect,
            launch_detached: config.settings.game.launch_detached,
            ram_mb: 2048,
            java_path: String::new(),
            jvm_options: String::new(),
            manifest_url: config.settings.launcher.manifest_url.clone().unwrap_or_default(),
            enable_discord_rpc: config.settings.launcher.enable_discord_rpc,
        }
    }

    /// Write the draft back into the persisted configuration.
    pub fn apply_to(&self, config: &mut Config) {
        config.settings.game.res_width = self.res_width;
        config.settings.game.res_height = self.res_height;
        config.settings.game.fullscreen = self.fullscreen;
        config.settings.game.auto_connect = self.auto_connect;
        config.settings.game.launch_detached = self.launch_detached;
        config.settings.launcher.manifest_url = if self.manifest_url.trim().is_empty() {
            None
        } else {
            Some(self.manifest_url.trim().to_owned())
        };
        config.settings.launcher.enable_discord_rpc = self.enable_discord_rpc;
    }

    /// Keep the resolution inside a sane range.
    pub fn clamp_resolution(&mut self) {
        self.res_width = self.res_width.clamp(320, 7680);
        self.res_height = self.res_height.clamp(240, 4320);
    }
}

/// Name used for the default offline profile.
pub fn default_username() -> String {
    "Player".to_owned()
}

/// Modpack browser state.
///
/// Kept small: the catalogue is remote, so this holds only the search box, the
/// last results, and which pack is open.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PackBrowser {
    pub query: String,
    pub results: Vec<mango_core::mods::modrinth_pack::PackSummary>,
    pub searching: bool,
    /// Slug of the pack whose versions are open.
    pub open: Option<String>,
    pub versions: Vec<mango_core::mods::modrinth_pack::PackVersion>,
    pub loading_versions: bool,
    /// Version chosen in the open pack, ready to install.
    pub selected_version: Option<String>,
    /// Instance id an install will use, editable so a second copy of the same pack
    /// does not overwrite the first.
    pub target_id: String,
    pub installing: bool,
}

impl PackBrowser {
    /// Version the install button would use.
    pub fn chosen_version(&self) -> Option<&mango_core::mods::modrinth_pack::PackVersion> {
        let id = self.selected_version.as_deref()?;
        self.versions.iter().find(|version| version.id == id)
    }

    /// Whether Install can be pressed.
    pub fn can_install(&self) -> bool {
        !self.installing
            && self.open.is_some()
            && self.chosen_version().is_some()
            && is_safe_instance_id(&self.target_id)
    }
}

/// Whether an id can be used as an instance directory name.
///
/// Mirrors the rule the instance layer enforces, so the Install button is disabled
/// before a request is made rather than failing afterwards.
fn is_safe_instance_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

/// A signed-in account, as the UI needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedInAccount {
    /// Key in `config.accounts`, needed to refresh and to remove it.
    pub uuid: String,
    pub username: String,
    /// Unix seconds when the Minecraft token expires, for a "needs refresh" hint.
    pub expires_at: Option<i64>,
    pub kind: mango_core::config::AccountKind,
}

/// The offline profile, always available so the launcher works with no sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineProfile {
    pub username: String,
}

/// One row in the account picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Account {
    Microsoft(SignedInAccount),
    Yggdrasil(SignedInAccount),
    Offline(OfflineProfile),
}

impl Account {
    pub fn username(&self) -> &str {
        match self {
            Self::Microsoft(account) | Self::Yggdrasil(account) => &account.username,
            Self::Offline(profile) => &profile.username,
        }
    }

    /// Short label shown next to the name.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Microsoft(_) => "Microsoft",
            Self::Yggdrasil(_) => "Yggdrasil",
            Self::Offline(_) => "offline profile",
        }
    }

    /// Whether this account can join an online server.
    pub fn is_online(&self) -> bool {
        !matches!(self, Self::Offline(_))
    }
}

/// Signed-in accounts followed by the offline profile.
///
/// The offline profile is kept last and cannot be removed, so there is always a
/// valid selection even if every stored account is deleted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountList {
    pub entries: Vec<Account>,
}

impl AccountList {
    /// Build from the persisted config, in stable uuid order.
    ///
    /// The offline profile is appended last, using `offline` as its name.
    pub fn from_config(config: &Config, offline: &str) -> Self {
        let mut entries: Vec<Account> = config
            .accounts
            .values()
            .map(|meta| {
                let account = SignedInAccount {
                    uuid: meta.uuid.clone(),
                    username: meta.username.clone(),
                    expires_at: meta.expires_at,
                    kind: meta.kind,
                };
                match meta.kind {
                    mango_core::config::AccountKind::Microsoft => Account::Microsoft(account),
                    _ => Account::Yggdrasil(account),
                }
            })
            .collect();

        // Selecting the stored account last used is the least surprising default.
        entries.sort_by(|a, b| a.username().cmp(b.username()));

        entries.push(Account::Offline(OfflineProfile { username: offline.to_owned() }));
        Self { entries }
    }

    pub fn get(&self, index: usize) -> Option<&Account> {
        self.entries.get(index)
    }

    pub fn selected(&self, index: Option<usize>) -> Option<&Account> {
        index.and_then(|index| self.get(index))
    }

    /// Index of the offline profile, which always exists.
    pub fn offline_index(&self) -> usize {
        self.entries.len().saturating_sub(1)
    }

    /// Index of the account with this uuid, if still present.
    pub fn index_of(&self, uuid: &str) -> Option<usize> {
        self.entries.iter().position(|account| match account {
            Account::Microsoft(account) | Account::Yggdrasil(account) => account.uuid == uuid,
            Account::Offline(_) => false,
        })
    }

    /// How many real accounts are stored.
    pub fn signed_in_count(&self) -> usize {
        self.entries.iter().filter(|account| account.is_online()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn manifest() -> Arc<Manifest> {
        Arc::new(
            serde_json::from_str(
                r#"{"schemaVersion":1,"name":"Test","servers":[
                    {"id":"a","name":"A","address":"a.example.com","minecraft":"1.20.1"},
                    {"id":"b","name":"B","address":"b.example.com","minecraft":"1.20.1","mainServer":true}
                ]}"#,
            )
            .unwrap(),
        )
    }

    fn state() -> UiState {
        let mut state = UiState::new(&Config::default());
        state.loading_manifest = true;
        state
    }

    #[test]
    fn loading_a_manifest_selects_the_main_server() {
        let mut state = state();
        state.apply(Event::ManifestLoaded { manifest: manifest() });
        assert_eq!(state.selected_server.as_deref(), Some("b"));
        assert!(!state.loading_manifest);
        assert_eq!(state.logs.len(), 0);
    }

    #[test]
    fn switching_manifests_keeps_a_valid_selection() {
        let mut state = state();
        state.apply(Event::ManifestLoaded { manifest: manifest() });
        state.select_server("a");

        let same = manifest();
        state.set_manifest(same);
        assert_eq!(state.selected_server.as_deref(), Some("a"));

        let other: Arc<Manifest> = Arc::new(
            serde_json::from_str(
                r#"{"schemaVersion":1,"name":"Other","servers":[
                    {"id":"c","name":"C","address":"c","minecraft":"1.20.1"}]}"#,
            )
            .unwrap(),
        );
        state.set_manifest(other);
        assert_eq!(state.selected_server.as_deref(), Some("c"));
    }

    #[test]
    fn progress_reports_file_and_byte_fractions() {
        let verifying =
            ProgressState::from_progress(&DownloadProgress::Verifying { done: 5, total: 10 });
        assert_eq!(verifying.fraction, 0.5);

        let downloading = ProgressState::from_progress(&DownloadProgress::Downloading {
            done: 1,
            total: 4,
            bytes_done: 250,
            bytes_total: 1000,
            current: PathBuf::from("/x/lwjgl.jar"),
        });
        assert_eq!(downloading.fraction, 0.25);
        assert!(downloading.label.contains("lwjgl.jar"));
    }

    #[test]
    fn progress_fraction_is_safe_with_zero_totals() {
        let zero = ProgressState::from_progress(&DownloadProgress::Downloading {
            done: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: 0,
            current: PathBuf::from("x"),
        });
        assert_eq!(zero.fraction, 0.0);
    }

    #[test]
    fn finished_progress_clears_the_bar() {
        let mut state = state();
        state.apply(Event::Progress(DownloadProgress::Verifying { done: 1, total: 2 }));
        assert!(state.progress.is_some());

        state.apply(Event::Progress(DownloadProgress::Finished { downloaded: 1, skipped: 2 }));
        assert!(state.progress.is_none());
    }

    #[test]
    fn errors_clear_busy_flags_and_are_logged() {
        let mut state = state();
        state.busy = true;
        state.loading_manifest = true;
        state.apply(Event::Failed { context: "install".to_owned(), message: "boom".to_owned() });

        assert!(!state.busy);
        assert!(!state.loading_manifest);
        assert!(state.error.as_deref().unwrap().contains("install: boom"));
        assert!(state.logs.last().unwrap().contains("boom"));
    }

    #[test]
    fn log_buffer_is_bounded() {
        let mut buffer = LogBuffer::with_capacity(3);
        for index in 0..5 {
            buffer.push(format!("line {index}"));
        }
        assert_eq!(buffer.len(), 3);
        let lines: Vec<_> = buffer.lines().collect();
        assert_eq!(lines, vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn stale_status_replies_are_ignored() {
        let mut state = state();
        state.apply(Event::ManifestLoaded { manifest: manifest() });
        state.select_server("a");

        let status = ServerStatus {
            version_name: "1.20.1".to_owned(),
            protocol: 763,
            players_online: 3,
            players_max: 20,
            motd: "hi".to_owned(),
            favicon: None,
            latency_ms: 10,
        };

        // Reply for the server the user just left.
        state.apply(Event::ServerStatus {
            address: "b.example.com".to_owned(),
            status: Box::new(status.clone()),
        });
        assert!(state.status.is_none());

        state.apply(Event::ServerStatus {
            address: "a.example.com".to_owned(),
            status: Box::new(status),
        });
        assert_eq!(state.status.unwrap().players_online, 3);
    }

    #[test]
    fn launch_requires_a_manifest_and_account() {
        let mut state = state();
        assert!(!state.can_launch());

        state.apply(Event::ManifestLoaded { manifest: manifest() });
        assert!(state.can_launch());

        state.busy = true;
        assert!(!state.can_launch());
        state.busy = false;

        // The offline profile is always present, so a launch is always possible
        // without signing in; only an empty selection blocks it.
        state.selected_account = None;
        assert!(!state.can_launch());
    }

    #[test]
    fn java_discovery_prefers_java_17_or_newer() {
        let mut state = state();
        let make = |version: &str, name: &str| JavaInstallation {
            executable: PathBuf::from(name),
            version: version.to_owned(),
            vendor: "test".to_owned(),
            arch: "amd64".to_owned(),
        };

        state.apply(Event::JavaDiscovered {
            installations: vec![make("1.8.0_402", "java8"), make("17.0.9", "java17")],
        });
        assert_eq!(state.selected_java_install().unwrap().executable, PathBuf::from("java17"));

        // An empty list must not panic and must clear the selection.
        state.apply(Event::JavaDiscovered { installations: Vec::new() });
        assert!(state.selected_java.is_none());
    }

    #[test]
    fn settings_round_trip_through_config() {
        let mut config = Config::default();
        config.settings.launcher.manifest_url = Some("https://example.com/m.json".to_owned());

        let mut draft = SettingsDraft::from_config(&config);
        assert_eq!(draft.manifest_url, "https://example.com/m.json");

        draft.res_width = 2560;
        draft.manifest_url = "  ".to_owned();
        draft.apply_to(&mut config);

        assert_eq!(config.settings.game.res_width, 2560);
        assert_eq!(config.settings.launcher.manifest_url, None);
    }

    #[test]
    fn resolution_is_clamped_to_sane_bounds() {
        let mut draft = SettingsDraft::from_config(&Config::default());
        draft.res_width = 10;
        draft.res_height = 100_000;
        draft.clamp_resolution();
        assert_eq!(draft.res_width, 320);
        assert_eq!(draft.res_height, 4320);
    }

    #[test]
    fn launched_event_clears_busy_state() {
        let mut state = state();
        state.busy = true;
        state.progress =
            Some(ProgressState { label: "x".to_owned(), done: 1, total: 2, fraction: 0.5 });

        state.apply(Event::Launched { pid: Some(4242) });
        assert_eq!(state.launch_pid, Some(4242));
        assert!(!state.busy);
        assert!(state.progress.is_none());
        assert!(state.error.is_none());
    }

    #[test]
    fn reload_selects_the_first_instance() {
        let dir = std::env::temp_dir().join(format!("mango-ui-inst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let loader = LoaderSpec::default();
        UserInstance::new("alpha", "Alpha", "1.20.1", loader.clone()).unwrap().save(&dir).unwrap();
        UserInstance::new("beta", "Beta", "1.12.2", loader).unwrap().save(&dir).unwrap();

        let mut state = UiState::new(&Config::default());
        state.reload_instances(&dir);

        assert_eq!(state.user_instances.len(), 2);
        // Sorted by name, so "Alpha" wins.
        assert_eq!(state.selected_instance.as_deref(), Some("alpha"));
        assert_eq!(state.selected_user_instance().unwrap().name, "Alpha");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reload_keeps_a_valid_selection_and_drops_a_stale_one() {
        let dir = std::env::temp_dir().join(format!("mango-ui-inst2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let loader = LoaderSpec::default();
        UserInstance::new("alpha", "Alpha", "1.20.1", loader.clone()).unwrap().save(&dir).unwrap();
        UserInstance::new("beta", "Beta", "1.12.2", loader).unwrap().save(&dir).unwrap();

        let mut state = UiState::new(&Config::default());
        state.reload_instances(&dir);
        state.selected_instance = Some("beta".to_owned());
        state.reload_instances(&dir);
        assert_eq!(state.selected_instance.as_deref(), Some("beta"));

        // Removing the selected instance falls back to another one.
        UserInstance::delete(&dir, "beta").unwrap();
        state.reload_instances(&dir);
        assert_eq!(state.selected_instance.as_deref(), Some("alpha"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn form_requires_a_version_but_not_a_name() {
        let mut form = InstanceForm::default();
        // Nothing chosen yet.
        assert!(!form.is_valid());
        assert!(form.build(&[]).is_err());

        form.minecraft = "1.20.1".to_owned();
        assert!(form.is_valid(), "a name is optional");

        // An unnamed instance keeps the suggested id as its name.
        let built = form.build(&[]).unwrap();
        assert_eq!(built.id, "instance");
        assert_eq!(built.name, "instance");
        assert_eq!(built.loader.kind, LoaderKind::Vanilla);
        assert_eq!(built.loader.version, None);
    }

    #[test]
    fn form_blank_loader_version_means_unpinned() {
        let form = InstanceForm {
            name: "Fabric Pack".to_owned(),
            minecraft: "1.20.1".to_owned(),
            loader: LoaderKind::Fabric,
            loader_version: "   ".to_owned(),
            editing: None,
        };
        assert_eq!(form.build(&[]).unwrap().loader.version, None);

        let pinned = InstanceForm { loader_version: "0.16.9".to_owned(), ..form };
        assert_eq!(pinned.build(&[]).unwrap().loader.version.as_deref(), Some("0.16.9"));
    }

    #[test]
    fn vanilla_form_ignores_a_stale_loader_version() {
        // Switching back to vanilla must not leave a loader version behind.
        let form = InstanceForm {
            name: "a".to_owned(),
            minecraft: "1.20.1".to_owned(),
            loader: LoaderKind::Vanilla,
            loader_version: "0.16.9".to_owned(),
            editing: None,
        };
        assert_eq!(form.loader_spec().version, None);
        assert!(form.build(&[]).is_ok());
    }

    #[test]
    fn form_avoids_id_collisions() {
        let form = InstanceForm {
            name: "Mine".to_owned(),
            minecraft: "1.20.1".to_owned(),
            ..Default::default()
        };
        let built = form.build(&["mine".to_owned(), "mine-2".to_owned()]).unwrap();
        assert_eq!(built.id, "mine-3");
    }

    #[test]
    fn editing_form_is_prefilled_and_applies_changes() {
        let mut instance = UserInstance::new(
            "p",
            "Pack",
            "1.20.1",
            LoaderSpec { kind: LoaderKind::Fabric, version: Some("0.16.9".to_owned()) },
        )
        .unwrap();

        let mut form = InstanceForm::editing(&instance);
        assert!(form.is_editing());
        assert_eq!(form.editing.as_deref(), Some("p"));
        assert_eq!(form.name, "Pack");
        assert_eq!(form.loader, LoaderKind::Fabric);
        assert_eq!(form.loader_version, "0.16.9");

        form.name = "Renamed".to_owned();
        form.minecraft = "1.21.1".to_owned();
        form.loader = LoaderKind::Neoforge;
        form.loader_version = "21.1.92".to_owned();

        form.apply_to(&mut instance).unwrap();
        assert_eq!(instance.name, "Renamed");
        assert_eq!(instance.minecraft, "1.21.1");
        assert_eq!(instance.loader.kind, LoaderKind::Neoforge);
        assert_eq!(instance.loader.version.as_deref(), Some("21.1.92"));
        // The id is identity and must not change.
        assert_eq!(instance.id, "p");
    }

    #[test]
    fn an_edit_without_a_name_keeps_the_id_as_name() {
        let mut instance =
            UserInstance::new("keep", "Original", "1.20.1", LoaderSpec::default()).unwrap();
        let form = InstanceForm { name: "  ".to_owned(), ..InstanceForm::editing(&instance) };

        form.apply_to(&mut instance).unwrap();
        assert_eq!(instance.name, "keep");
    }

    #[test]
    fn loader_versions_are_ignored_when_stale() {
        let mut state = UiState::new(&Config::default());
        state.new_instance.loader = LoaderKind::Fabric;
        state.new_instance.minecraft = "1.20.1".to_owned();

        state.apply(Event::LoaderVersionsLoaded {
            kind: LoaderKind::Forge,
            minecraft: "1.20.1".to_owned(),
            versions: vec!["47.4.29".to_owned()],
        });
        assert!(state.loader_versions.is_empty(), "wrong loader must not fill the list");

        state.apply(Event::LoaderVersionsLoaded {
            kind: LoaderKind::Fabric,
            minecraft: "1.20.1".to_owned(),
            versions: vec!["0.16.9".to_owned()],
        });
        assert_eq!(state.loader_versions, vec!["0.16.9".to_owned()]);
    }

    #[test]
    fn a_pinned_loader_version_is_cleared_when_no_longer_offered() {
        let mut state = UiState::new(&Config::default());
        state.new_instance.loader = LoaderKind::Fabric;
        state.new_instance.minecraft = "1.21.1".to_owned();
        state.new_instance.loader_version = "0.16.9".to_owned();

        state.apply(Event::LoaderVersionsLoaded {
            kind: LoaderKind::Fabric,
            minecraft: "1.21.1".to_owned(),
            versions: vec!["0.17.0".to_owned()],
        });
        assert_eq!(state.new_instance.loader_version, "", "stale pin must be dropped");

        state.apply(Event::LoaderVersionsLoaded {
            kind: LoaderKind::Fabric,
            minecraft: "1.21.1".to_owned(),
            versions: vec!["0.17.0".to_owned()],
        });
        assert_eq!(state.loader_versions, vec!["0.17.0".to_owned()]);
    }

    #[test]
    fn version_list_defaults_to_the_newest_release() {
        let mut state = UiState::new(&Config::default());
        state.apply(Event::VersionsLoaded {
            versions: vec!["1.21.4".to_owned(), "1.20.1".to_owned()],
        });
        assert_eq!(state.new_instance.minecraft, "1.21.4");
        assert_eq!(state.available_versions.len(), 2);

        // A pick that still exists survives a refresh.
        state.new_instance.minecraft = "1.20.1".to_owned();
        state.apply(Event::VersionsLoaded {
            versions: vec!["1.21.4".to_owned(), "1.20.1".to_owned()],
        });
        assert_eq!(state.new_instance.minecraft, "1.20.1");
    }

    #[test]
    fn launching_an_instance_requires_a_selection() {
        let mut state = UiState::new(&Config::default());
        assert!(!state.can_launch_instance());

        state
            .user_instances
            .push(UserInstance::new("a", "A", "1.20.1", LoaderSpec::default()).unwrap());
        state.selected_instance = Some("a".to_owned());
        assert!(state.can_launch_instance());

        state.busy = true;
        assert!(!state.can_launch_instance());
    }

    #[test]
    fn the_discord_switch_survives_a_settings_round_trip() {
        let mut config = Config::default();
        assert!(config.settings.launcher.enable_discord_rpc, "on by default");

        let mut draft = SettingsDraft::from_config(&config);
        assert!(draft.enable_discord_rpc);

        draft.enable_discord_rpc = false;
        draft.apply_to(&mut config);
        assert!(!config.settings.launcher.enable_discord_rpc);

        let reloaded = SettingsDraft::from_config(&config);
        assert!(!reloaded.enable_discord_rpc, "the choice must survive a restart");
    }

    #[test]
    fn a_java_requirement_is_not_guessed_before_metadata_arrives() {
        // Launching from the server list used to guess the requirement from the
        // version name, which gets `26.3` wrong (21 instead of 25) and crashes the
        // game with `Unsupported class file major version`.
        let mut state = UiState::new(&Config::default());
        assert_eq!(state.java_major_for("26.3"), None, "nothing is known yet");

        state.apply(Event::JavaRequirementResolved { minecraft: "26.3".to_owned(), major: 25 });
        assert_eq!(state.java_major_for("26.3"), Some(25));

        // Other versions keep their own answer.
        assert_eq!(state.java_major_for("1.20.1"), None);
    }

    #[test]
    fn the_offline_profile_is_always_the_last_entry() {
        // PLAY must be usable with no sign-in, so there is never an empty list.
        let list = AccountList::from_config(&Config::default(), "Player");
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.offline_index(), 0);
        assert_eq!(list.selected(Some(0)).map(Account::username), Some("Player"));
        assert!(!list.selected(Some(0)).unwrap().is_online());
    }

    #[test]
    fn a_stored_account_is_listed_before_the_offline_profile() {
        let mut config = Config::default();
        config.accounts.insert(
            "uuid-1".to_owned(),
            mango_core::config::AccountMeta {
                kind: mango_core::config::AccountKind::Microsoft,
                uuid: "uuid-1".to_owned(),
                username: "Steve".to_owned(),
                expires_at: Some(1_700_000_000),
                yggdrasil_base: None,
            },
        );

        let list = AccountList::from_config(&config, "Player");
        assert_eq!(list.entries.len(), 2);
        assert_eq!(list.selected(Some(0)).map(Account::username), Some("Steve"));
        assert!(list.selected(Some(0)).unwrap().is_online());
        assert_eq!(list.offline_index(), 1);
        assert_eq!(list.signed_in_count(), 1);
        assert_eq!(list.index_of("uuid-1"), Some(0));
    }

    #[test]
    fn the_remembered_account_is_preselected() {
        let mut config = Config::default();
        for (uuid, name) in [("u1", "Alex"), ("u2", "Steve")] {
            config.accounts.insert(
                uuid.to_owned(),
                mango_core::config::AccountMeta {
                    kind: mango_core::config::AccountKind::Microsoft,
                    uuid: uuid.to_owned(),
                    username: name.to_owned(),
                    expires_at: None,
                    yggdrasil_base: None,
                },
            );
        }
        config.selected_account = Some("u2".to_owned());

        let state = UiState::new(&config);
        assert_eq!(state.selected_username(), Some("Steve"));
        assert!(state.selected_signed_in().is_some());
    }

    #[test]
    fn with_no_remembered_account_the_offline_profile_is_selected() {
        let state = UiState::new(&Config::default());
        assert_eq!(state.selected_username(), Some(default_username().as_str()));
        assert!(state.selected_signed_in().is_none(), "offline is not a sign-in");
    }

    #[test]
    fn removing_the_selected_account_falls_back_to_offline() {
        let mut config = Config::default();
        config.accounts.insert(
            "u1".to_owned(),
            mango_core::config::AccountMeta {
                kind: mango_core::config::AccountKind::Microsoft,
                uuid: "u1".to_owned(),
                username: "Steve".to_owned(),
                expires_at: None,
                yggdrasil_base: None,
            },
        );
        config.selected_account = Some("u1".to_owned());

        let mut state = UiState::new(&config);
        assert_eq!(state.selected_username(), Some("Steve"));

        // The app reloads after the config loses the account; nothing points at it.
        let empty = Config::default();
        state.reload_accounts(&empty, None);
        assert_eq!(state.selected_username(), Some(default_username().as_str()));
        assert!(state.selected_signed_in().is_none());
    }

    fn sample_update(version: &str) -> mango_core::updater::Update {
        mango_core::updater::Update {
            version: mango_core::updater::parse_version(version).unwrap(),
            notes: Some("Faster installs\nFixed a crash".to_owned()),
            asset: mango_core::updater::PlatformAsset {
                url: "https://example.com/MangoLauncher-setup.exe".to_owned(),
                sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    .to_owned(),
                size: Some(1024),
                kind: mango_core::updater::AssetKind::Installer,
            },
        }
    }

    #[test]
    fn an_available_update_is_recorded_and_logged() {
        let mut state = UiState::new(&Config::default());
        assert!(state.available_update.is_none());

        state.apply(Event::UpdateAvailable { update: Box::new(sample_update("0.2.0")) });

        let update = state.available_update.as_ref().expect("the update should be kept");
        assert_eq!(update.version.to_string(), "0.2.0");
        assert!(
            state.logs.last().is_some_and(|line| line.contains("0.2.0")),
            "the player should see the version in the activity log"
        );
    }

    #[test]
    fn an_up_to_date_answer_leaves_no_banner() {
        let mut state = UiState::new(&Config::default());
        state.apply(Event::UpToDate);
        assert!(state.available_update.is_none());
        assert!(!state.update_dismissed);
    }
}
