//! Settings screen.

use eframe::egui;
use egui::{Align, Layout, RichText};

use crate::app::{Action, AppEnv};
use crate::runtime::{Command, RuntimeHandle};
use crate::state::{Account, UiState};
use crate::theme;

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut UiState,
    env: &mut AppEnv,
    runtime: &RuntimeHandle,
    action: &mut Action,
) {
    ui.label(theme::heading("Settings"));
    ui.add_space(8.0);

    // The footer claims its space first. Drawing it after the scroll area would
    // paint it on top of the Java list, because the scroll area takes whatever
    // height is left and a `bottom_up` layout shares that space rather than
    // reserving it.
    egui::TopBottomPanel::bottom("settings-footer").show_inside(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                *action = Action::SaveSettings;
            }
            if env.dirty {
                ui.label(theme::muted("unsaved changes"));
            }
        });
    });

    egui::ScrollArea::vertical().show(ui, |ui| {
        section_accounts(ui, state, action);
        ui.add_space(10.0);
        section_minecraft(ui, state);
        ui.add_space(10.0);
        section_java(ui, state, runtime);
        ui.add_space(10.0);
        section_launcher(ui, state, env, runtime);
        ui.add_space(10.0);
        section_mods(ui, state);
        ui.add_space(10.0);
        section_about(ui);
        ui.add_space(10.0);
        section_updates(ui, state, action);
    });
}

fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(theme::BG_PANEL)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::section(title));
            ui.add_space(8.0);
            add(ui)
        })
        .inner
}

fn section_accounts(ui: &mut egui::Ui, state: &mut UiState, action: &mut Action) {
    card(ui, "ACCOUNTS", |ui| {
        if state.accounts.signed_in_count() == 0 {
            ui.label(theme::muted(
                "Sign in with a Microsoft account to join online servers. \
                 Without one, the launcher uses an offline profile.",
            ));
        }

        ui.add_space(6.0);
        for (index, account) in state.accounts.entries.iter().enumerate() {
            let selected = state.selected_account == Some(index);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(selected, RichText::new(account.username()).strong())
                    .clicked()
                {
                    state.selected_account = Some(index);
                }
                ui.label(theme::muted(account.kind_label()));

                if let Account::Microsoft(signed_in) | Account::Yggdrasil(signed_in) = account {
                    // Expiry is worth showing: a stale token is the usual reason a
                    // launch is refused, and refreshing fixes it.
                    match signed_in.expires_at {
                        Some(expires) if expires * 1000 > now_millis() => {
                            ui.label(theme::muted("token valid"));
                        }
                        Some(_) => {
                            ui.label(RichText::new("token expired").color(theme::DANGER));
                        }
                        None => {}
                    }
                }
            });
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Sign in with Microsoft").clicked() {
                *action = Action::SignIn;
            }

            let has_account = state.selected_signed_in().is_some();
            if ui.add_enabled(has_account, egui::Button::new("Refresh")).clicked() {
                *action = Action::RefreshAccount;
            }
            if ui.add_enabled(has_account, egui::Button::new("Sign out")).clicked() {
                *action = Action::SignOut;
            }
        });

        ui.add_space(4.0);
        ui.label(theme::muted(
            "Signing in opens your browser. The launcher never sees your password; \
             tokens are stored in the OS keyring.",
        ));

        // A browser that never opened would otherwise look like a hang: the flow is
        // waiting for a redirect that cannot arrive. The URL is shown so it can be
        // pasted into a browser by hand.
        if let Some(url) = state.sign_in_url.clone() {
            ui.add_space(10.0);
            ui.label(RichText::new("Waiting for the browser…").color(theme::ACCENT));
            ui.label(theme::muted(
                "If no browser opened, copy this address into one. Leave the launcher \
                 open until it says signed in.",
            ));
            ui.add_space(4.0);
            let mut shown = url.as_str();
            ui.add(
                egui::TextEdit::multiline(&mut shown)
                    .desired_width(ui.available_width())
                    .desired_rows(3),
            );
            if ui.button("Copy address").clicked() {
                ui.ctx().copy_text(url.clone());
            }
        }
    });
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn section_minecraft(ui: &mut egui::Ui, state: &mut UiState) {
    card(ui, "MINECRAFT", |ui| {
        egui::Grid::new("mc-grid").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label("Resolution");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut state.settings.res_width).range(320..=7680));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut state.settings.res_height).range(240..=4320));
            });
            ui.end_row();

            ui.label("Fullscreen");
            ui.checkbox(&mut state.settings.fullscreen, "Launch in fullscreen");
            ui.end_row();

            ui.label("Auto connect");
            ui.checkbox(&mut state.settings.auto_connect, "Join the server on launch");
            ui.end_row();

            ui.label("Detached");
            ui.checkbox(
                &mut state.settings.launch_detached,
                "Keep the game running after the launcher closes",
            );
            ui.end_row();
        });
    });
}

fn section_java(ui: &mut egui::Ui, state: &mut UiState, runtime: &RuntimeHandle) {
    card(ui, "JAVA", |ui| {
        ui.horizontal(|ui| {
            ui.label("Installed runtimes");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Rescan").clicked() {
                    runtime.send(Command::DiscoverJava);
                }
            });
        });

        ui.add_space(6.0);
        if state.java.is_empty() {
            ui.label(theme::muted("No Java found. Install one with `mango java install 17`."));
        } else {
            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                for (index, java) in state.java.iter().enumerate() {
                    let selected = state.selected_java == Some(index);
                    let label = format!(
                        "{}  ·  {}  ·  {}",
                        java.version,
                        java.vendor,
                        java.executable.display()
                    );
                    if ui.selectable_label(selected, RichText::new(label).size(12.5)).clicked() {
                        state.selected_java = Some(index);
                    }
                }
            });
        }

        ui.add_space(8.0);
        egui::Grid::new("java-grid").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label("Memory");
            ui.add(
                egui::Slider::new(&mut state.settings.ram_mb, 512..=16384)
                    .step_by(512.0)
                    .suffix(" MB"),
            );
            ui.end_row();

            ui.label("JVM options");
            ui.add(
                egui::TextEdit::singleline(&mut state.settings.jvm_options)
                    .desired_width(360.0)
                    .hint_text("-XX:+UseG1GC"),
            );
            ui.end_row();
        });
    });
}

fn section_launcher(
    ui: &mut egui::Ui,
    state: &mut UiState,
    env: &mut AppEnv,
    runtime: &RuntimeHandle,
) {
    card(ui, "LAUNCHER", |ui| {
        ui.label("Manifest");
        ui.add(
            egui::TextEdit::singleline(&mut state.manifest_source)
                .desired_width(ui.available_width().min(460.0))
                .hint_text("https://example.com/manifest.json"),
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Load").clicked() {
                state.loading_manifest = true;
                state.error = None;
                runtime.send(Command::LoadManifest {
                    source: state.manifest_source.clone(),
                    cache_root: env.data.root().to_path_buf(),
                });
            }
            if ui.button("Browse…").clicked()
                && let Some(path) =
                    rfd::FileDialog::new().add_filter("manifest", &["json"]).pick_file()
            {
                state.manifest_source = path.to_string_lossy().to_string();
                env.dirty = true;
            }
        });

        ui.add_space(10.0);
        ui.label("Game directory");
        ui.horizontal(|ui| {
            ui.label(theme::muted(env.game_root.display().to_string()));
            if ui.button("Change…").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                env.config.settings.launcher.data_directory =
                    Some(path.to_string_lossy().to_string());
                env.game_root = path;
                env.dirty = true;
            }
        });

        ui.add_space(10.0);
        ui.checkbox(&mut state.settings.enable_discord_rpc, "Show what I am playing on Discord");
        ui.label(theme::muted(
            "Needs the manifest to publish a Discord client id. Has no effect when Discord is closed.",
        ));
    });
}

fn section_mods(ui: &mut egui::Ui, state: &mut UiState) {
    card(ui, "MODS", |ui| {
        let Some(server) = state.selected_server_entry().cloned() else {
            ui.label(theme::muted("Select a server to see its mods."));
            return;
        };

        if server.mods.is_empty() {
            ui.label(theme::muted("This server declares no additional mods."));
            return;
        }

        ui.label(theme::muted(
            "Mods are resolved from Modrinth or CurseForge when the server is installed.",
        ));
        ui.add_space(6.0);

        for entry in &server.mods {
            ui.horizontal(|ui| {
                let source = match entry.source {
                    mango_core::manifest::ModSource::Modrinth => "Modrinth",
                    mango_core::manifest::ModSource::Curseforge => "CurseForge",
                };
                ui.label(RichText::new(&entry.id).strong());
                ui.label(theme::muted(format!(
                    "{} · {} · {}",
                    source,
                    entry.version.as_deref().unwrap_or("latest"),
                    if entry.required { "required" } else { "optional" }
                )));
            });
        }
    });
}

fn section_about(ui: &mut egui::Ui) {
    card(ui, "ABOUT", |ui| {
        ui.label(format!("MangoLauncher {}", mango_core::VERSION));
        ui.label(theme::muted(
            "A Rust Minecraft launcher. Behaviour inspired by Helios Launcher; no code shared.",
        ));
        ui.label(theme::muted(
            "Game files are downloaded from Mojang's official servers and never redistributed.",
        ));
    });
}

fn section_updates(ui: &mut egui::Ui, state: &UiState, action: &mut Action) {
    card(ui, "UPDATES", |ui| match state.available_update.as_ref() {
        Some(update) => {
            ui.label(RichText::new(format!("Version {} is available", update.version)).strong());
            if let Some(notes) = update.notes.as_deref().filter(|notes| !notes.trim().is_empty()) {
                ui.add_space(4.0);
                for line in notes.lines().take(6) {
                    ui.label(theme::muted(line));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Download and install").clicked() {
                    *action = Action::ApplyUpdate;
                }
                if ui.button("Later").clicked() {
                    *action = Action::DismissUpdate;
                }
            });
        }
        None => {
            ui.label(theme::muted(format!(
                "You are running MangoLauncher {}.",
                mango_core::VERSION
            )));
            ui.add_space(6.0);
            if ui.button("Check for updates").clicked() {
                *action = Action::CheckForUpdate;
            }
        }
    });
}
