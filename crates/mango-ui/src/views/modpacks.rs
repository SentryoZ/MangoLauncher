//! Modpack browser: search Modrinth, pick a version, install.

use eframe::egui;
use egui::{Align, Layout, RichText};

use crate::app::{Action, AppEnv};
use crate::runtime::{Command, RuntimeHandle};
use crate::state::UiState;
use crate::theme;

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut UiState,
    _env: &AppEnv,
    runtime: &RuntimeHandle,
    action: &mut Action,
) {
    if state.packs.open.is_some() {
        pack_detail(ui, state, runtime, action);
        return;
    }

    ui.label(theme::heading("Modpacks"));
    ui.label(theme::muted("Browse and install complete packs from Modrinth."));
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.packs.query)
                .desired_width(320.0)
                .hint_text("search packs"),
        );

        // Enter searches too: the field is the only control in this row a keyboard
        // user reaches first.
        let submitted =
            response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

        if ui.button("Search").clicked() || submitted {
            if state.packs.query.trim().is_empty() {
                state.error = Some("enter something to search for".to_owned());
            } else {
                state.packs.searching = true;
                state.error = None;
                runtime.send(Command::SearchPacks { query: state.packs.query.clone() });
            }
        }

        if state.packs.searching {
            ui.add(egui::Spinner::new().size(14.0));
        }
    });

    ui.add_space(12.0);

    if state.packs.results.is_empty() && !state.packs.searching {
        ui.label(theme::muted(
            "Search for a pack, for example `fabulously optimized` or `adrenaline`.",
        ));
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for pack in state.packs.results.clone() {
            let clicked = egui::Frame::default()
                .fill(theme::BG_PANEL)
                .corner_radius(egui::CornerRadius::same(theme::RADIUS))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&pack.title).strong().size(14.5));
                            ui.label(theme::muted(&pack.description));
                            ui.label(theme::muted(format!(
                                "{} downloads · {}",
                                format_downloads(pack.downloads),
                                pack.game_versions.join(", ")
                            )));
                        });

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.button("Open"))
                            .inner
                            .clicked()
                    })
                    .inner
                })
                // A click anywhere on the row opens it, so the whole card is a
                // target rather than just the button.
                .response
                .interact(egui::Sense::click())
                .clicked();

            if clicked {
                state.packs.open = Some(pack.slug.clone());
                state.packs.target_id = suggest_instance_id(&pack.slug);
                state.packs.versions.clear();
                state.packs.selected_version = None;
                state.packs.loading_versions = true;
                state.error = None;
                runtime.send(Command::LoadPackVersions { project: pack.slug.clone() });
                break;
            }
        }
    });
}

fn pack_detail(
    ui: &mut egui::Ui,
    state: &mut UiState,
    runtime: &RuntimeHandle,
    action: &mut Action,
) {
    let Some(project) = state.packs.open.clone() else { return };

    ui.horizontal(|ui| {
        ui.label(theme::heading(&project));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Back").clicked() {
                *action = Action::ClosePack;
            }
        });
    });
    ui.add_space(8.0);

    if state.packs.loading_versions {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(theme::muted("loading versions…"));
        });
        return;
    }

    if state.packs.versions.is_empty() {
        ui.label(theme::muted("This pack has no installable versions."));
        return;
    }

    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        for version in state.packs.versions.clone() {
            let selected = state.packs.selected_version.as_deref() == Some(version.id.as_str());
            let label = format!(
                "{}  ·  Minecraft {}  ·  {}  ·  {}",
                version.version_number,
                version.game_versions.join(", "),
                version.loaders.join(", "),
                format_size(version.size)
            );

            if ui.selectable_label(selected, RichText::new(label).size(13.0)).clicked() {
                state.packs.selected_version = Some(version.id.clone());
                // A second copy of the same pack must not overwrite the first, so
                // the id carries the version when one already exists.
                state.packs.target_id =
                    suggest_instance_id(&format!("{project}-{}", version.version_number));
            }
        }
    });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    egui::Grid::new("pack-install").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Instance id");
        ui.add(
            egui::TextEdit::singleline(&mut state.packs.target_id)
                .desired_width(260.0)
                .hint_text("folder name"),
        );
        ui.end_row();
    });

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(state.packs.can_install(), egui::Button::new("Install")).clicked() {
            *action = Action::InstallPack;
        }
        if state.packs.installing {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(theme::muted("installing…"));
        }
    });

    if !state.packs.target_id.trim().is_empty()
        && !state.packs.target_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        ui.label(
            RichText::new("The instance id may only contain letters, digits, `-` and `_`.")
                .color(theme::DANGER)
                .small(),
        );
    }

    // The version list is fetched by the click that opened this screen, but a
    // reload after a failed request would otherwise need another click.
    if state.packs.versions.is_empty() && !state.packs.loading_versions {
        ui.add_space(8.0);
        if ui.button("Reload versions").clicked() {
            runtime.send(Command::LoadPackVersions { project: project.clone() });
        }
    }
}

/// A folder-safe id from a pack name.
fn suggest_instance_id(name: &str) -> String {
    let mut id: String =
        name.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() { character.to_ascii_lowercase() } else { '-' }
            })
            .collect();

    // Collapse runs of separators, and trim them from the ends: `--a--` is valid
    // but ugly, and a leading `-` looks like a flag.
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    id.trim_matches('-').to_owned()
}

fn format_downloads(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PackBrowser;

    #[test]
    fn an_id_is_lowercased_and_separated() {
        assert_eq!(suggest_instance_id("Fabulously Optimized"), "fabulously-optimized");
        assert_eq!(suggest_instance_id("Adrenaline 1.20.1"), "adrenaline-1-20-1");
    }

    #[test]
    fn separators_are_collapsed_and_trimmed() {
        // `--` bloat and leading/trailing `-` both look like mistakes.
        assert_eq!(suggest_instance_id("  a -- b  "), "a-b");
        assert_eq!(suggest_instance_id("!!!pack!!!"), "pack");
    }

    #[test]
    fn a_name_with_only_symbols_yields_an_empty_id() {
        // The Install button stays disabled, so this never becomes a directory name.
        assert_eq!(suggest_instance_id("!!!"), "");
    }

    #[test]
    fn an_id_stays_within_the_charset_instances_allow() {
        let id = suggest_instance_id("Some Pack / v2.0 (beta)");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{id}");
    }

    #[test]
    fn download_counts_are_shortened() {
        assert_eq!(format_downloads(999), "999");
        assert_eq!(format_downloads(1500), "1.5k");
        assert_eq!(format_downloads(2_500_000), "2.5M");
    }

    #[test]
    fn sizes_are_rendered_in_the_units_a_player_reads() {
        assert_eq!(format_size(61_666), "60 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn install_needs_a_version_and_a_safe_id() {
        let mut browser = PackBrowser::default();
        assert!(!browser.can_install(), "nothing selected");

        browser.open = Some("pack".to_owned());
        assert!(!browser.can_install(), "no version yet");

        browser.versions.push(mango_core::mods::modrinth_pack::PackVersion {
            id: "v1".to_owned(),
            version_number: "1.0".to_owned(),
            game_versions: vec!["1.20.1".to_owned()],
            loaders: vec!["fabric".to_owned()],
            url: "https://cdn/v1.mrpack".to_owned(),
            file_name: "v1.mrpack".to_owned(),
            size: 1000,
            version_type: "release".to_owned(),
        });
        browser.selected_version = Some("v1".to_owned());
        assert!(!browser.can_install(), "no id yet");

        browser.target_id = "my copy".to_owned();
        assert!(!browser.can_install(), "a space is not a valid id");

        browser.target_id = "my-copy".to_owned();
        assert!(browser.can_install());

        browser.installing = true;
        assert!(!browser.can_install(), "an install in flight blocks a second");
    }
}
