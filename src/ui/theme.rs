//! Dark, minimal visual theme.

use egui::{Color32, Stroke, TextStyle, Vec2};

pub const BG: Color32 = Color32::from_rgb(10, 14, 19);
pub const PANEL: Color32 = Color32::from_rgb(16, 22, 30);
pub const CARD: Color32 = Color32::from_rgb(21, 28, 38);
pub const BORDER: Color32 = Color32::from_rgb(37, 47, 61);
pub const TEXT: Color32 = Color32::from_rgb(226, 234, 242);
pub const MUTED: Color32 = Color32::from_rgb(139, 152, 167);
pub const ACCENT: Color32 = Color32::from_rgb(102, 192, 244);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(27, 47, 64);
pub const GOOD: Color32 = Color32::from_rgb(121, 214, 143);
pub const WARN: Color32 = Color32::from_rgb(226, 183, 90);
pub const BAD: Color32 = Color32::from_rgb(224, 108, 117);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(8, 12, 17);
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    visuals.widgets.noninteractive.bg_fill = CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(26, 35, 46);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(35, 46, 60);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.widgets.active.bg_fill = ACCENT_DIM;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.widgets.open.bg_fill = CARD;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, BORDER);

    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.text_styles.insert(
        TextStyle::Body,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    ctx.set_style_of(egui::Theme::Dark, style);
}
