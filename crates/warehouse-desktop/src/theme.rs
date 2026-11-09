//! Colors and widget metrics shared with the HTML demo page.

use egui::{Color32, CornerRadius, Stroke, Vec2};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x1a);
pub const CARD: Color32 = Color32::from_rgb(0x1c, 0x20, 0x30);
pub const LINE: Color32 = Color32::from_rgb(0x2c, 0x33, 0x48);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xeb, 0xf4);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xb8);
pub const ACCENT: Color32 = Color32::from_rgb(0x7a, 0xa2, 0xff);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x0d, 0x12, 0x20);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x8b, 0x8b);
pub const DANGER_BG: Color32 = Color32::from_rgb(0x3a, 0x22, 0x28);
pub const OK: Color32 = Color32::from_rgb(0x8e, 0xe0, 0xb0);
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0xb4, 0x5a);
pub const INPUT: Color32 = Color32::from_rgb(0x10, 0x13, 0x1b);
pub const BUBBLE_USER: Color32 = Color32::from_rgb(0x24, 0x33, 0x56);
pub const BUBBLE_ASSISTANT: Color32 = Color32::from_rgb(0x16, 0x1a, 0x26);
pub const PENDING_BG: Color32 = Color32::from_rgb(0x24, 0x18, 0x1c);
pub const PENDING_LINE: Color32 = Color32::from_rgb(0x5a, 0x30, 0x38);
pub const BUTTON: Color32 = Color32::from_rgb(0x24, 0x2a, 0x3c);
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(0x2e, 0x36, 0x4e);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(TEXT);
        visuals.weak_text_color = Some(MUTED);
        visuals.panel_fill = BG;
        visuals.window_fill = CARD;
        visuals.extreme_bg_color = INPUT;
        visuals.faint_bg_color = CARD;
        visuals.hyperlink_color = ACCENT;
        visuals.warn_fg_color = WARN;
        visuals.error_fg_color = DANGER;
        visuals.window_stroke = Stroke::new(1.0, LINE);
        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.menu_corner_radius = CornerRadius::same(8);
        visuals.widgets.noninteractive.bg_fill = CARD;
        visuals.widgets.noninteractive.weak_bg_fill = CARD;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
        visuals.widgets.inactive.weak_bg_fill = BUTTON;
        visuals.widgets.inactive.bg_fill = BUTTON;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
        visuals.widgets.hovered.weak_bg_fill = BUTTON_HOVER;
        visuals.widgets.hovered.bg_fill = BUTTON_HOVER;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
        visuals.widgets.active.weak_bg_fill = ACCENT;
        visuals.widgets.active.bg_fill = ACCENT;
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, ON_ACCENT);
        visuals.widgets.active.corner_radius = CornerRadius::same(8);
        visuals.widgets.open.weak_bg_fill = BUTTON;
        visuals.widgets.open.bg_fill = CARD;
        visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.open.corner_radius = CornerRadius::same(8);
        visuals.selection.bg_fill = Color32::from_rgb(0x3a, 0x52, 0x86);
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.striped = true;
        style.visuals = visuals;
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
    });
}

pub fn accent_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(text).color(ON_ACCENT)).fill(ACCENT)
}

pub fn danger_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(text).color(DANGER)).fill(DANGER_BG)
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(12)
        .inner_margin(12)
}
