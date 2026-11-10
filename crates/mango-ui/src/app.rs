//! Application shell: window, navigation, and the action handlers that turn UI
//! input into worker commands.

use std::path::PathBuf;

use eframe::egui;
use egui::{Align, Layout, RichText};
use mango_core::config::Config;
use mango_core::paths::DataPaths;

use crate::runtime::{Command, RuntimeHandle, roots_for};
use crate::state::{UiState, View};
use crate::theme;
use crate::views;

use mango_core::presence::PresenceConfig;

/// Paths and configuration the UI needs to act on.
pub struct AppEnv {
    pub data: DataPaths,
    pub game_root: PathBuf,
    pub config: Config,
    pub dirty: bool,
}

pub struct MangoApp {
    pub state: UiState,
    pub runtime: RuntimeHandle,
    pub env: AppEnv,
    /// Server whose status has already been requested, to avoid re-pinging.
    last_pinged: Option<String>,
}

impl MangoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, env: AppEnv) -> Self {
        theme::apply(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let ctx = cc.egui_ctx.clone();
        let runtime = RuntimeHandle::spawn(move || ctx.request_repaint());

        let state = UiState::new(&env.config);

        // Discover Java and load the configured manifest without blocking the
        // first frame.
        runtime.send(Command::DiscoverJava);
        runtime.send(Command::LoadVersions);
        // An update check is optional and must never delay the first frame, so it
        // runs on the worker like everything else.
        runtime.send(Command::CheckForUpdate {
            manifest_url: mango_core::updater::default_manifest_url(),
            allow_prerelease: env.config.settings.launcher.allow_prerelease,
        });
        if !state.manifest_source.trim().is_empty() {
            runtime.send(Command::LoadManifest {
                source: state.manifest_source.clone(),
                cache_root: env.data.root().to_path_buf(),
            });
        }

        let mut app = Self { state, runtime, env, last_pinged: None };
        app.state.reload_instances(&app.env.game_root.join("instances"));
        app
    }

    fn save_settings(&mut self) {
        self.state.settings.clamp_resolution();
        self.state.settings.apply_to(&mut self.env.config);
        if let Err(err) = self.env.config.save(&self.env.data) {
            self.state.error = Some(format!("could not save settings: {err}"));
        } else {
            self.state.logs.push("settings saved".to_owned());
            self.env.dirty = false;
        }
    }

    fn load_manifest(&mut self) {
        let source = self.state.manifest_source.trim().to_owned();
        if source.is_empty() {
            self.state.error = Some("enter a manifest path or URL".to_owned());
            return;
        }
        self.state.loading_manifest = true;
        self.state.error = None;
        self.runtime.send(Command::LoadManifest {
            source: source.clone(),
            cache_root: self.env.data.root().to_path_buf(),
        });

        // Persist the manifest source so the next start uses it.
        self.state.settings.manifest_url = source;
        self.state.settings.apply_to(&mut self.env.config);
        if let Err(err) = self.env.config.save(&self.env.data) {
            self.state.logs.push(format!("could not save config: {err}"));
        }
    }

    fn launch(&mut self) {
        let Some(server) = self.state.selected_server_entry().cloned() else {
            self.state.error = Some("select a server first".to_owned());
            return;
        };
        let Some(username) = self.state.selected_username().map(str::to_owned) else {
            self.state.error = Some("select an account first".to_owned());
            return;
        };
        let Some(java) = self.state.selected_java_install().map(|java| java.executable.clone())
        else {
            self.state.error =
                Some("no Java runtime found; install one with `mango java install 17`".to_owned());
            return;
        };

        let roots = roots_for(&self.env.game_root, &server.id);
        self.state.busy = true;
        self.state.error = None;
        self.state.launch_pid = None;
        // A server launch replaces any instance the presence was naming.
        self.state.running_instance_id = None;

        self.runtime
            .send(Command::Install { server: Box::new(server.clone()), roots: roots.clone() });
        self.runtime.send(Command::Launch {
            server: Box::new(server),
            roots,
            username,
            java,
            detached: self.state.settings.launch_detached,
        });
    }

    /// Create the instance described by the form and persist it.
    fn create_instance(&mut self) {
        let dir = self.env.game_root.join("instances");
        let existing = self.state.existing_instance_ids();
        let instance = match self.state.new_instance.build(&existing) {
            Ok(instance) => instance,
            Err(err) => {
                self.state.error = Some(err);
                return;
            }
        };

        match instance.save(&dir) {
            Ok(_) => {
                self.state.logs.push(format!("created instance {}", instance.id));
                self.state.new_instance = Default::default();
                self.state.showing_form = false;
                self.state.reload_instances(&dir);
                self.state.selected_instance = Some(instance.id);
                self.state.error = None;
            }
            Err(err) => self.state.error = Some(format!("could not save the instance: {err}")),
        }
    }

    /// Apply the form's changes to the instance being edited.
    fn save_instance(&mut self) {
        let Some(id) = self.state.new_instance.editing.clone() else { return };
        let dir = self.env.game_root.join("instances");

        // Re-read from disk so concurrent edits are not silently overwritten.
        let mut instance = match mango_core::instance::UserInstance::load(&dir.join(&id)) {
            Ok(Some(instance)) => instance,
            Ok(None) => {
                self.state.error = Some(format!("instance `{id}` no longer exists"));
                return;
            }
            Err(err) => {
                self.state.error = Some(format!("could not read the instance: {err}"));
                return;
            }
        };

        if let Err(err) = self.state.new_instance.apply_to(&mut instance) {
            self.state.error = Some(err);
            return;
        }

        match instance.save(&dir) {
            Ok(_) => {
                self.state.logs.push(format!("updated instance {}", instance.id));
                self.state.new_instance = Default::default();
                self.state.showing_form = false;
                self.state.reload_instances(&dir);
                self.state.selected_instance = Some(instance.id);
                self.state.error = None;
            }
            Err(err) => self.state.error = Some(format!("could not save the instance: {err}")),
        }
    }

    /// Delete the selected instance after an explicit confirmation dialog.
    fn delete_instance(&mut self) {
        let Some(instance) = self.state.selected_user_instance().cloned() else { return };

        // Deleting removes worlds, so ask first.
        let confirmed = rfd::MessageDialog::new()
            .set_title("Delete instance")
            .set_description(format!(
                "Delete “{}” and its worlds? This cannot be undone.",
                instance.name
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();

        if confirmed != rfd::MessageDialogResult::Yes {
            return;
        }

        let dir = self.env.game_root.join("instances");
        match mango_core::instance::UserInstance::delete(&dir, &instance.id) {
            Ok(()) => {
                self.state.logs.push(format!("deleted instance {}", instance.id));
                self.state.selected_instance = None;
                self.state.reload_instances(&dir);
                self.state.error = None;
            }
            Err(err) => self.state.error = Some(format!("could not delete the instance: {err}")),
        }
    }

    fn install_instance(&mut self) {
        let Some(instance) = self.state.selected_user_instance().cloned() else { return };
        let roots = roots_for(&self.env.game_root, &instance.id);

        self.state.busy = true;
        self.state.error = None;
        self.state.launch_pid = None;
        self.runtime.send(Command::InstallInstance { instance: Box::new(instance), roots });
    }

    fn launch_instance(&mut self) {
        let Some(instance) = self.state.selected_user_instance().cloned() else { return };
        let roots = roots_for(&self.env.game_root, &instance.id);

        self.state.busy = true;
        self.state.error = None;
        self.state.launch_pid = None;

        self.runtime.send(Command::InstallInstance {
            instance: Box::new(instance.clone()),
            roots: roots.clone(),
        });
        self.runtime.send(Command::LaunchInstance {
            instance: Box::new(instance.clone()),
            roots,
            username: self.state.selected_username().unwrap_or("Player").to_owned(),
            detached: self.state.settings.launch_detached,
        });
        // Remembered so the presence card can name the instance; it is not in the
        // manifest, so the sidebar selection cannot supply it.
        self.state.running_instance_id = Some(instance.id);
    }

    fn maybe_ping(&mut self) {
        let Some(server) = self.state.selected_server_entry() else { return };
        if self.state.pinging || self.last_pinged.as_deref() == Some(server.address.as_str()) {
            return;
        }

        let address = server.address.clone();
        self.last_pinged = Some(address.clone());
        self.state.pinging = true;
        self.runtime.send(Command::PingServer { address });
    }

    /// Push the current state to Discord.
    ///
    /// Returns without sending when nothing changed: every `send` wakes the UI for
    /// a repaint, so an unconditional send here would keep the event loop spinning.
    fn maybe_update_presence(&mut self) {
        let config = PresenceConfig::from_parts(
            self.env.config.settings.launcher.enable_discord_rpc,
            self.state.manifest.as_deref(),
        );

        // Discord shows "elapsed" from this timestamp, so it is taken when the
        // game starts, not when the launcher opens.
        let started_at = if self.state.launch_pid.is_some() {
            Some(*self.state.launch_started_at.get_or_insert_with(mango_core::presence::now_millis))
        } else {
            self.state.launch_started_at = None;
            None
        };

        // A player-created instance is not in the manifest, so the target is built
        // from whichever view the game was started from.
        let target = self.presence_target();
        let activity = mango_core::presence::activity_for(
            self.state.manifest.as_deref(),
            target.as_ref(),
            started_at.is_some(),
            started_at,
        );

        let current = (config, activity);
        if self.state.last_presence.as_ref() == Some(&current) {
            return;
        }
        self.state.last_presence = Some(current.clone());

        self.runtime
            .send(Command::SetPresence { config: current.0, activity: Box::new(current.1) });
    }

    /// What to name on the presence card.
    ///
    /// A running instance wins over the sidebar selection, because that is what the
    /// player is actually doing.
    fn presence_target(&self) -> Option<mango_core::presence::PresenceTarget> {
        if let Some(instance) = self.state.running_instance() {
            return Some(mango_core::presence::PresenceTarget::new(
                &instance.name,
                &instance.minecraft,
            ));
        }

        self.state.selected_server_entry().map(mango_core::presence::PresenceTarget::from_server)
    }

    /// Download and apply the update the feed offered.
    fn apply_update(&mut self) {
        let Some(update) = self.state.available_update.clone() else { return };

        // The staging directory is outside the game tree so a partial download is
        // never treated as game content.
        self.runtime.send(Command::ApplyUpdate {
            update: Box::new(update),
            staging: self.env.data.root().join("update"),
        });
    }

    fn check_for_update(&mut self) {
        // An explicit check overrides an earlier dismissal, otherwise pressing the
        // button would appear to do nothing.
        self.state.update_dismissed = false;
        self.runtime.send(Command::CheckForUpdate {
            manifest_url: mango_core::updater::default_manifest_url(),
            allow_prerelease: self.env.config.settings.launcher.allow_prerelease,
        });
    }

    /// Banner advertising a newer launcher release.
    fn update_banner(&self, ui: &mut egui::Ui, action: &mut Action) {
        let Some(update) = self.state.available_update.as_ref() else { return };

        egui::Frame::default()
            .fill(theme::ACCENT_DIM)
            .inner_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("MangoLauncher {} is available", update.version))
                            .strong(),
                    );
                    ui.label(theme::muted(format!("you have {}", mango_core::VERSION)));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Later").clicked() {
                            *action = Action::DismissUpdate;
                        }
                        if ui.button("Update now").clicked() {
                            *action = Action::ApplyUpdate;
                        }
                    });
                });

                if let Some(notes) =
                    update.notes.as_deref().filter(|notes| !notes.trim().is_empty())
                {
                    ui.label(theme::muted(notes.lines().next().unwrap_or_default()));
                }
            });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("MANGO").size(18.0).strong().color(theme::ACCENT));
            ui.label(theme::muted(format!("v{}", mango_core::VERSION)));
            ui.add_space(16.0);

            for (view, label) in [
                (View::Landing, "Play"),
                (View::Instances, "Instances"),
                (View::Settings, "Settings"),
                (View::Logs, "Logs"),
            ] {
                let selected = self.state.view == view;
                if ui.selectable_label(selected, label).clicked() {
                    self.state.view = view;
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(server) = self.state.selected_server_entry() {
                    ui.label(theme::muted(&server.address));
                }
            });
        });
    }
}

impl eframe::App for MangoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for event in self.runtime.poll() {
            self.state.apply(event);
        }
        self.maybe_ping();
        self.maybe_update_presence();

        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| self.top_bar(ui));

        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(14, 6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(error) = &self.state.error {
                        ui.label(RichText::new(error).color(theme::DANGER));
                    } else if let Some(line) = self.state.logs.last() {
                        ui.label(theme::muted(line));
                    } else {
                        ui.label(theme::muted("ready"));
                    }
                });
            });

        // Views report intent instead of calling back into `self`, so the
        // borrow of `state`/`env` ends before an action mutates the app.
        let mut action = Action::None;

        if self.state.available_update.is_some() && !self.state.update_dismissed {
            egui::TopBottomPanel::top("update").show(ctx, |ui| self.update_banner(ui, &mut action));
        }

        egui::SidePanel::left("servers")
            .resizable(false)
            .exact_width(240.0)
            .frame(egui::Frame::default().inner_margin(egui::Margin::same(12)))
            .show(ctx, |ui| views::sidebar::draw(ui, &mut self.state, &self.runtime, &self.env));

        egui::CentralPanel::default()
            .frame(egui::Frame::default().inner_margin(egui::Margin::same(18)))
            .show(ctx, |ui| match self.state.view {
                View::Landing => views::landing::draw(ui, &mut self.state, &self.env, &mut action),
                View::Instances => views::instances::draw(
                    ui,
                    &mut self.state,
                    &self.env,
                    &self.runtime,
                    &mut action,
                ),
                View::Settings => views::settings::draw(
                    ui,
                    &mut self.state,
                    &mut self.env,
                    &self.runtime,
                    &mut action,
                ),
                View::Logs => views::logs::draw(ui, &mut self.state),
            });

        match action {
            Action::None => {}
            Action::Launch => self.launch(),
            Action::LoadManifest => self.load_manifest(),
            Action::SaveSettings => self.save_settings(),
            Action::CreateInstance => self.create_instance(),
            Action::SaveInstance => self.save_instance(),
            Action::DeleteInstance => self.delete_instance(),
            Action::InstallInstance => self.install_instance(),
            Action::LaunchInstance => self.launch_instance(),
            Action::ApplyUpdate => self.apply_update(),
            Action::DismissUpdate => self.state.update_dismissed = true,
            Action::CheckForUpdate => self.check_for_update(),
        }
    }
}

/// What the user asked for this frame, applied after rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    None,
    Launch,
    LoadManifest,
    SaveSettings,
    CreateInstance,
    SaveInstance,
    DeleteInstance,
    InstallInstance,
    LaunchInstance,
    ApplyUpdate,
    DismissUpdate,
    CheckForUpdate,
}
