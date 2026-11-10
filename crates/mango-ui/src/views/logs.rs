//! Scrollable activity log.

use eframe::egui;
use egui::{Align, Layout, RichText};

use crate::state::UiState;
use crate::theme;

pub fn draw(ui: &mut egui::Ui, state: &mut UiState) {
    ui.horizontal(|ui| {
        ui.label(theme::heading("Logs"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Clear").clicked() {
                state.logs.clear();
            }
            ui.label(theme::muted(format!("{} lines", state.logs.len())));
        });
    });

    ui.add_space(6.0);

    egui::Frame::default()
        .fill(theme::BG_PANEL)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            let height = ui.available_height() - 8.0;
            egui::ScrollArea::vertical().stick_to_bottom(true).max_height(height).show(ui, |ui| {
                if state.logs.is_empty() {
                    ui.label(theme::muted("Nothing yet."));
                    return;
                }
                for line in state.logs.lines() {
                    let color =
                        if line.starts_with("ERROR") { theme::DANGER } else { theme::TEXT_MUTED };
                    ui.label(RichText::new(line).monospace().size(12.0).color(color));
                }
            });
        });
}
