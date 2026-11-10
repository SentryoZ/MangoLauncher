//! Server list sidebar.

use eframe::egui;
use egui::{Align, Layout, RichText};

use crate::app::AppEnv;
use crate::runtime::{Command, RuntimeHandle};
use crate::state::UiState;
use crate::theme;

pub fn draw(ui: &mut egui::Ui, state: &mut UiState, runtime: &RuntimeHandle, env: &AppEnv) {
    ui.label(theme::section("SERVERS"));
    ui.add_space(4.0);

    let Some(manifest) = state.manifest.clone() else {
        ui.label(theme::muted("No manifest loaded."));
        ui.add_space(8.0);
        ui.label(theme::muted("Set one in Settings, or load the bundled example."));
        if ui.button("Load example manifest").clicked() {
            let source = env.data.root().join("example-manifest.json");
            let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/manifest.json");
            if std::fs::copy(&example, &source).is_ok() {
                state.manifest_source = source.to_string_lossy().to_string();
                state.loading_manifest = true;
                state.error = None;
                runtime.send(Command::LoadManifest {
                    source: source.to_string_lossy().to_string(),
                    cache_root: env.data.root().to_path_buf(),
                });
            } else if let Err(err) = std::fs::create_dir_all(env.data.root()) {
                state.error = Some(format!("could not prepare the data directory: {err}"));
            }
        }
        return;
    };

    egui::ScrollArea::vertical().max_height(ui.available_height() - 60.0).show(ui, |ui| {
        for server in &manifest.servers {
            let selected = state.selected_server.as_deref() == Some(server.id.as_str());
            let text = RichText::new(&server.name).size(14.0);
            let text = if selected { text.color(theme::ACCENT) } else { text };

            let response = ui
                .add_sized([ui.available_width(), 34.0], egui::Button::selectable(selected, text));
            if response.clicked() {
                state.select_server(server.id.clone());
            }
            response.on_hover_text(format!("{} · {}", server.address, server.minecraft));
        }
    });

    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
        if state.loading_manifest {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(theme::muted("loading manifest…"));
        } else {
            ui.label(theme::muted(format!("{} servers", manifest.servers.len())));
        }
    });
}
