//! Instances screen: create, edit, launch, and delete player instances.

use eframe::egui;
use egui::{Align, Layout, RichText};
use mango_core::manifest::{LoaderKind, LoaderSpec};

use crate::app::{Action, AppEnv};
use crate::runtime::{Command, RuntimeHandle};
use crate::state::{InstanceForm, UiState};
use crate::theme;

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut UiState,
    env: &AppEnv,
    runtime: &RuntimeHandle,
    action: &mut Action,
) {
    // The form replaces the list/detail panes entirely: editing is a focused task.
    if state.showing_form {
        form_panel(ui, state, env, runtime, action);
        return;
    }

    egui::SidePanel::left("instance-list")
        .resizable(false)
        .exact_width(210.0)
        .show_inside(ui, |ui| list(ui, state));

    egui::CentralPanel::default().show_inside(ui, |ui| {
        if let Some(instance) = state.selected_user_instance().cloned() {
            details(ui, &instance, state, env, runtime, action);
        } else {
            empty_state(ui, state);
        }
    });
}

fn list(ui: &mut egui::Ui, state: &mut UiState) {
    ui.horizontal(|ui| {
        ui.label(theme::section("INSTANCES"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("+").on_hover_text("New instance").clicked() {
                state.new_instance = InstanceForm::default();
                state.showing_form = true;
            }
        });
    });
    ui.add_space(4.0);

    if state.user_instances.is_empty() {
        ui.label(theme::muted("No instances yet."));
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for instance in &state.user_instances {
            let selected = state.selected_instance.as_deref() == Some(instance.id.as_str());
            let text = if selected {
                RichText::new(&instance.name).color(theme::ACCENT)
            } else {
                RichText::new(&instance.name)
            };
            let response = ui
                .add_sized([ui.available_width(), 30.0], egui::Button::selectable(selected, text));
            if response.clicked() {
                state.selected_instance = Some(instance.id.clone());
            }
            response.on_hover_text(format!(
                "{} · {}",
                instance.minecraft,
                loader_label(&instance.loader)
            ));
        }
    });
}

fn empty_state(ui: &mut egui::Ui, state: &mut UiState) {
    ui.label(theme::heading("My instances"));
    ui.label(theme::muted("Pick any Minecraft version and loader. Create one to start playing."));
    ui.add_space(12.0);

    if ui.button("New instance").clicked() {
        state.new_instance = InstanceForm::default();
        state.showing_form = true;
    }
}

fn details(
    ui: &mut egui::Ui,
    instance: &mango_core::instance::UserInstance,
    state: &mut UiState,
    env: &AppEnv,
    runtime: &RuntimeHandle,
    action: &mut Action,
) {
    ui.horizontal(|ui| {
        ui.label(theme::heading(&instance.name));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Close").clicked() {
                state.selected_instance = None;
            }
        });
    });
    ui.add_space(4.0);

    egui::Frame::default()
        .fill(theme::BG_PANEL)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("instance-facts").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                ui.label(theme::section("MINECRAFT"));
                ui.label(RichText::new(&instance.minecraft).strong());
                ui.end_row();

                ui.label(theme::section("LOADER"));
                ui.label(RichText::new(loader_label(&instance.loader)).strong());
                ui.end_row();

                ui.label(theme::section("JAVA"));
                // The metadata answer is authoritative; the fallback is only for
                // the moment before it arrives.
                let major = state
                    .java_major_for(&instance.minecraft)
                    .map(|major| major.to_string())
                    .unwrap_or_else(|| "…".to_owned());
                ui.label(RichText::new(format!("Java {major}")).strong());
                ui.end_row();

                ui.label(theme::section("ID"));
                ui.label(theme::muted(&instance.id));
                ui.end_row();

                ui.label(theme::section("MODS"));
                ui.label(theme::muted(instance.mods.len().to_string()));
                ui.end_row();
            });
        });

    // The Java requirement comes from the version metadata, which needs a network
    // round-trip, so it is asked for once per distinct version.
    if state.java_major_for(&instance.minecraft).is_none() {
        runtime.send(Command::LoadJavaRequirement { minecraft: instance.minecraft.clone() });
    }

    ui.add_space(14.0);

    let play =
        egui::Button::new(RichText::new("PLAY").size(18.0).strong().color(egui::Color32::BLACK))
            .fill(if state.can_launch_instance() { theme::ACCENT } else { theme::BG_RAISED })
            .min_size(egui::vec2(200.0, 42.0));

    ui.horizontal(|ui| {
        if ui.add_enabled(state.can_launch_instance(), play).clicked() {
            *action = Action::LaunchInstance;
        }
        if ui.add_enabled(!state.busy, egui::Button::new("Install files")).clicked() {
            *action = Action::InstallInstance;
        }
        if ui.add_enabled(!state.busy, egui::Button::new("Edit")).clicked() {
            state.new_instance = InstanceForm::editing(instance);
            state.showing_form = true;
        }
    });

    ui.add_space(10.0);
    if let Some(progress) = &state.progress {
        ui.add(
            egui::ProgressBar::new(progress.fraction)
                .text(RichText::new(&progress.label).small())
                .desired_width(ui.available_width().min(480.0)),
        );
    } else if state.busy {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(theme::muted("working…"));
        });
    }

    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
        if ui.button("Delete").clicked() {
            *action = Action::DeleteInstance;
        }
        ui.label(theme::muted(format!("data dir: {}", env.game_root.display())));
    });
}

/// The create/edit form. Which one is in progress is decided by
/// [`InstanceForm::editing`].
fn form_panel(
    ui: &mut egui::Ui,
    state: &mut UiState,
    env: &AppEnv,
    runtime: &RuntimeHandle,
    action: &mut Action,
) {
    let editing = state.new_instance.is_editing();

    ui.horizontal(|ui| {
        ui.label(theme::heading(if editing { "Edit instance" } else { "New instance" }));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Cancel").clicked() {
                state.showing_form = false;
                state.new_instance = InstanceForm::default();
            }
        });
    });
    ui.label(theme::muted(if editing {
        "Changing the version or loader reinstalls on next launch. Worlds and configs are kept."
    } else {
        "Pick any Minecraft version and loader. Files download on first launch."
    }));
    ui.add_space(12.0);

    // Set when a dropdown changed, so the loader list is refetched once instead of
    // on every frame.
    let mut needs_loader_list = false;

    egui::Frame::default()
        .fill(theme::BG_PANEL)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(560.0));

            egui::Grid::new("instance-form").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                ui.label("Name");
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_instance.name)
                        .desired_width(320.0)
                        .hint_text("optional — defaults to the id"),
                );
                ui.end_row();

                ui.label("Minecraft");
                if version_picker(ui, state) {
                    needs_loader_list = true;
                }
                ui.end_row();

                ui.label("Loader");
                ui.horizontal(|ui| {
                    for kind in [
                        LoaderKind::Vanilla,
                        LoaderKind::Fabric,
                        LoaderKind::Quilt,
                        LoaderKind::Forge,
                        LoaderKind::Neoforge,
                    ] {
                        let selected = state.new_instance.loader == kind;
                        if ui.selectable_label(selected, loader_title(kind)).clicked() && !selected
                        {
                            state.new_instance.loader = kind;
                            // A version pinned for the previous loader is meaningless.
                            state.new_instance.loader_version.clear();
                            needs_loader_list = true;
                        }
                    }
                });
                ui.end_row();

                ui.label("Loader version");
                loader_version_picker(ui, state);
                ui.end_row();
            });
        });

    if needs_loader_list {
        request_loader_list(state, runtime);
    }

    ui.add_space(12.0);

    let enabled = state.new_instance.is_valid() && !state.busy;
    ui.horizontal(|ui| {
        let label = if editing { "Save changes" } else { "Create instance" };
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            *action = if editing { Action::SaveInstance } else { Action::CreateInstance };
        }
        ui.label(theme::muted(format!("data dir: {}", env.game_root.display())));
    });

    if state.available_versions.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(theme::muted("loading Minecraft versions…"));
        });
    }
}

/// Ask the worker for the loader versions matching the form's current choices.
fn request_loader_list(state: &mut UiState, runtime: &RuntimeHandle) {
    if state.new_instance.loader == LoaderKind::Vanilla {
        state.loader_versions.clear();
        state.loading_loader_versions = false;
        return;
    }
    if state.new_instance.minecraft.trim().is_empty() {
        state.loader_versions.clear();
        return;
    }

    state.loading_loader_versions = true;
    runtime.send(Command::LoadLoaderVersions {
        kind: state.new_instance.loader,
        minecraft: state.new_instance.minecraft.clone(),
    });
}

/// Returns true when the selection changed and the loader list must be refetched.
fn version_picker(ui: &mut egui::Ui, state: &mut UiState) -> bool {
    if state.available_versions.is_empty() {
        ui.label(theme::muted("loading…"));
        return false;
    }

    let previous = state.new_instance.minecraft.clone();
    egui::ComboBox::from_id_salt("mc-version")
        .selected_text(if previous.is_empty() { "choose".to_owned() } else { previous.clone() })
        .width(200.0)
        .show_ui(ui, |ui| {
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for version in &state.available_versions {
                    ui.selectable_value(
                        &mut state.new_instance.minecraft,
                        version.clone(),
                        version,
                    );
                }
            });
        });

    if state.new_instance.minecraft == previous {
        return false;
    }
    // A loader version pinned for the previous release is meaningless.
    state.new_instance.loader_version.clear();
    true
}

fn loader_version_picker(ui: &mut egui::Ui, state: &mut UiState) {
    if state.new_instance.loader == LoaderKind::Vanilla {
        ui.label(theme::muted("vanilla has no loader version"));
        return;
    }
    if state.loading_loader_versions {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(10.0));
            ui.label(theme::muted("loading versions…"));
        });
        return;
    }
    if state.loader_versions.is_empty() {
        ui.label(theme::muted("no builds listed — leave empty to use the newest compatible"));
        return;
    }

    let selected = if state.new_instance.loader_version.is_empty() {
        "newest".to_owned()
    } else {
        state.new_instance.loader_version.clone()
    };

    egui::ComboBox::from_id_salt("loader-version").selected_text(selected).width(200.0).show_ui(
        ui,
        |ui| {
            ui.selectable_value(&mut state.new_instance.loader_version, String::new(), "newest");
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for version in &state.loader_versions {
                    ui.selectable_value(
                        &mut state.new_instance.loader_version,
                        version.clone(),
                        version,
                    );
                }
            });
        },
    );
}

fn loader_title(kind: LoaderKind) -> &'static str {
    match kind {
        LoaderKind::Vanilla => "Vanilla",
        LoaderKind::Fabric => "Fabric",
        LoaderKind::Quilt => "Quilt",
        LoaderKind::Forge => "Forge",
        LoaderKind::Neoforge => "NeoForge",
    }
}

fn loader_label(loader: &LoaderSpec) -> String {
    match loader.version.as_deref() {
        Some(version) => format!("{} {version}", loader_title(loader.kind)),
        None => loader_title(loader.kind).to_owned(),
    }
}
