//! Visual identity: palette, spacing and widget styling.
//!
//! The colours follow the same dark, warm-neutral direction as Helios so the
//! two launchers feel related, but the fonts are OFL replacements — Helios ships
//! Avenir, which is licensed and cannot be redistributed.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// Background behind the landing screen.
pub const BG_DEEP: Color32 = Color32::from_rgb(0x17, 0x16, 0x14);
/// Panels and cards.
pub const BG_PANEL: Color32 = Color32::from_rgb(0x1f, 0x1e, 0x1b);
/// Raised surface (inputs, list rows).
pub const BG_RAISED: Color32 = Color32::from_rgb(0x2a, 0x28, 0x24);
/// Primary accent, used for the PLAY button and selection.
pub const ACCENT: Color32 = Color32::from_rgb(0xe8, 0x9c, 0x2e);
/// Muted accent for informational banners, dark enough to keep text readable.
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x3a, 0x2e, 0x1c);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xee, 0xec, 0xe7);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9a, 0x96, 0x8d);
pub const DANGER: Color32 = Color32::from_rgb(0xd9, 0x53, 0x4f);
pub const SUCCESS: Color32 = Color32::from_rgb(0x5f, 0xb8, 0x6a);

/// Standard corner radius for cards and buttons.
pub const RADIUS: u8 = 6;

/// Install the launcher's visual style into a context.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();

    visuals.panel_fill = BG_DEEP;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = BG_RAISED;
    visuals.faint_bg_color = BG_RAISED;
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x33, 0x30, 0x2b));

    let radius = CornerRadius::same(RADIUS);
    visuals.widgets.noninteractive.corner_radius = radius;
    visuals.widgets.inactive.corner_radius = radius;
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.corner_radius = radius;

    visuals.widgets.inactive.bg_fill = BG_RAISED;
    visuals.widgets.inactive.weak_bg_fill = BG_RAISED;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x35, 0x33, 0x2d);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x35, 0x33, 0x2d);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x3d, 0x3a, 0x33);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x3d, 0x3a, 0x33);

    visuals.selection.bg_fill = ACCENT.linear_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 28.0;
    ctx.set_style(style);
}

/// Heading text.
pub fn heading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(22.0_f32).strong().color(TEXT_PRIMARY)
}

/// Section label.
pub fn section(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(13.0_f32).strong().color(TEXT_MUTED)
}

/// Secondary/muted body text.
pub fn muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(TEXT_MUTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_is_warm_and_distinct_from_background() {
        assert!(ACCENT.r() > ACCENT.b(), "accent should be warm");
        assert_ne!(ACCENT, BG_DEEP);
    }

    #[test]
    fn text_is_light_on_dark_background() {
        let luminance = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        assert!(luminance(TEXT_PRIMARY) > luminance(BG_DEEP));
        assert!(luminance(TEXT_MUTED) > luminance(BG_DEEP));
    }

    #[test]
    fn applying_theme_sets_panel_fill() {
        let ctx = egui::Context::default();
        apply(&ctx);
        assert_eq!(ctx.style().visuals.panel_fill, BG_DEEP);
    }
}
