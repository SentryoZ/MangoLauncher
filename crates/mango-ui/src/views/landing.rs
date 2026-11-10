//! Landing screen: the selected server, its status, and the PLAY button.

use eframe::egui;
use egui::{Align, Layout, RichText};

use crate::app::{Action, AppEnv};
use crate::state::UiState;
use crate::theme;

pub fn draw(ui: &mut egui::Ui, state: &mut UiState, env: &AppEnv, action: &mut Action) {
    if state.manifest.is_none() {
        draw_empty(ui, state, action);
        return;
    }

    let Some(server) = state.selected_server_entry().cloned() else {
        ui.label(theme::heading("No server selected"));
        return;
    };

    ui.label(theme::heading(&server.name));
    if let Some(description) = &server.description {
        ui.label(theme::muted(description));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(theme::muted(format!("{} · Minecraft {}", server.address, server.minecraft)));
        ui.label(theme::muted(format!("· {}", server.loader.kind.as_str())));
    });

    ui.add_space(14.0);
    status_badge(ui, state, &server.address);

    ui.add_space(18.0);
    egui::Frame::default()
        .fill(theme::BG_PANEL)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            ui.horizontal(|ui| {
                ui.label(theme::section("ACCOUNT"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let name = state.selected_username().unwrap_or("none").to_owned();
                    ui.label(RichText::new(name).strong());
                    ui.label(theme::muted("offline profile"));
                });
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(theme::section("JAVA"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match state.selected_java_install() {
                        Some(java) => {
                            ui.label(RichText::new(&java.version).strong());
                            ui.label(theme::muted(&java.vendor));
                        }
                        None => {
                            ui.label(RichText::new("not found").color(theme::DANGER));
                        }
                    }
                });
            });
        });

    ui.add_space(18.0);

    let button =
        egui::Button::new(RichText::new("PLAY").size(20.0).strong().color(egui::Color32::BLACK))
            .fill(if state.can_launch() { theme::ACCENT } else { theme::BG_RAISED })
            .min_size(egui::vec2(ui.available_width().min(320.0), 48.0));

    if ui.add_enabled(state.can_launch(), button).clicked() {
        *action = Action::Launch;
    }

    if let Some(progress) = &state.progress {
        ui.add_space(10.0);
        ui.add(
            egui::ProgressBar::new(progress.fraction)
                .text(RichText::new(&progress.label).small())
                .desired_width(ui.available_width().min(520.0)),
        );
    } else if state.busy {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(theme::muted("working…"));
        });
    }

    if let Some(pid) = state.launch_pid {
        ui.add_space(8.0);
        ui.label(theme::muted(format!("game running (pid {pid})")));
    }

    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
        ui.label(theme::muted(format!("data dir: {}", env.game_root.display())));
    });
}

fn draw_empty(ui: &mut egui::Ui, state: &mut UiState, action: &mut Action) {
    ui.label(theme::heading("MangoLauncher"));
    ui.label(theme::muted(
        "Load a manifest to get started. A manifest lists the servers this launcher supports.",
    ));
    ui.add_space(12.0);

    ui.label(theme::section("MANIFEST"));
    ui.add(
        egui::TextEdit::singleline(&mut state.manifest_source)
            .desired_width(ui.available_width().min(520.0))
            .hint_text("https://example.com/manifest.json or a local path"),
    );

    ui.add_space(6.0);
    if ui.button("Load manifest").clicked() {
        *action = Action::LoadManifest;
    }

    if state.loading_manifest {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(theme::muted("loading…"));
        });
    }
}

fn status_badge(ui: &mut egui::Ui, state: &UiState, address: &str) {
    ui.horizontal(|ui| {
        if state.pinging {
            ui.add(egui::Spinner::new().size(10.0));
            ui.label(theme::muted("checking status…"));
            return;
        }

        match &state.status {
            Some(status) => {
                // A reply means the server answered, so it is online; the player
                // count is informational. A full server is still reachable.
                let color = theme::SUCCESS;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.label(RichText::new("Online").color(color).small());

                ui.label(theme::muted(format!(
                    "{}/{} players · {} · {} ms",
                    status.players_online,
                    status.players_max,
                    status.version_name,
                    status.latency_ms
                )));
                if !status.motd.is_empty() {
                    ui.label(theme::muted(format!("“{}”", status.motd)));
                }
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, theme::TEXT_MUTED);
                ui.label(theme::muted(format!("status unknown · {address}")));
            }
        }
    });
}
