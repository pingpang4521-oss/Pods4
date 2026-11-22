//! One UI-inspired palette and the small custom widgets the window is built from.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};

// True-black canvas, soft charcoal cards, Samsung blue accent.
pub const BG_MAIN: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
pub const BG_CARD: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x1E);
pub const BG_INSET: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2E);
pub const DIVIDER: Color32 = Color32::from_rgb(0x38, 0x38, 0x3A);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0xA0, 0xA0, 0xA5);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x70, 0x70, 0x75);
pub const ACCENT_BLUE: Color32 = Color32::from_rgb(0x38, 0x87, 0xF5);
pub const ACCENT_GREEN_HI: Color32 = Color32::from_rgb(0x4C, 0xD9, 0x64);
pub const ACCENT_AMBER: Color32 = Color32::from_rgb(0xFF, 0xB3, 0x40);
pub const ACCENT_RED: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);

pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_MAIN;
    visuals.window_fill = BG_MAIN;
    visuals.widgets.noninteractive.bg_fill = BG_CARD;
    visuals.widgets.inactive.bg_fill = BG_INSET;
    visuals.widgets.inactive.weak_bg_fill = BG_INSET;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x3A, 0x3A, 0x3C);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3A, 0x3A, 0x3C);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x48, 0x48, 0x4A);
    visuals.selection.bg_fill = ACCENT_BLUE;
    visuals.slider_trailing_fill = true;
    ctx.set_visuals(visuals);
}

pub fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    card_filled(ui, BG_CARD, add_contents);
}

pub fn card_filled(ui: &mut egui::Ui, fill: Color32, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(24))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
    ui.add_space(12.0);
}

pub fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(13.5).strong().color(ACCENT_BLUE));
    ui.add_space(10.0);
}

pub fn divider(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, DIVIDER);
    ui.add_space(10.0);
}

pub fn battery_badge(ui: &mut egui::Ui, title: &str, pct: Option<u8>, sub: &str) {
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(12.0).color(TEXT_MUTED));
        let value = pct.map_or("—".to_string(), |p| format!("{p}%"));
        ui.label(RichText::new(value).size(24.0).strong().color(TEXT_PRIMARY));

        // Thin battery bar
        let w = (ui.available_width() - 16.0).clamp(20.0, 90.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 5.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(3), BG_INSET);
        if let Some(p) = pct {
            let fill_w = rect.width() * (p.min(100) as f32 / 100.0);
            let color = match p {
                0..=15 => ACCENT_RED,
                16..=30 => ACCENT_AMBER,
                _ => ACCENT_GREEN_HI,
            };
            let fill = egui::Rect::from_min_size(rect.min, Vec2::new(fill_w, rect.height()));
            ui.painter().rect_filled(fill, CornerRadius::same(3), color);
        }
        ui.add_space(2.0);
        ui.label(RichText::new(sub).size(11.0).color(TEXT_DIM));
    });
}

/// Rounded One UI style choice button (filled blue when selected).
pub fn pill_button(ui: &mut egui::Ui, label: &str, active: bool, height: f32, size: f32) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .size(size)
            .strong()
            .color(if active { Color32::WHITE } else { TEXT_MUTED }),
    )
    .fill(if active { ACCENT_BLUE } else { BG_INSET })
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same((height / 2.0) as u8))
    .min_size(Vec2::new(ui.available_width(), height));
    ui.add(btn)
}

/// Compact text button with blue label (Reconnect, Open, Install...).
pub fn action_button(ui: &mut egui::Ui, label: &str, fill: Color32, enabled: bool) -> egui::Response {
    let btn = egui::Button::new(RichText::new(label).size(12.5).strong().color(ACCENT_BLUE))
        .fill(fill)
        .corner_radius(CornerRadius::same(16))
        .min_size(Vec2::new(0.0, 32.0));
    ui.add_enabled(enabled, btn)
}

/// Filled primary button (white on blue).
pub fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let btn = egui::Button::new(RichText::new(label).size(12.5).strong().color(Color32::WHITE))
        .fill(ACCENT_BLUE)
        .corner_radius(CornerRadius::same(16))
        .min_size(Vec2::new(0.0, 32.0));
    ui.add_enabled(enabled, btn)
}

pub fn slider_header(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.5).color(TEXT_PRIMARY));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.5).strong().color(ACCENT_BLUE));
        });
    });
}

pub fn full_width_slider(ui: &mut egui::Ui, val: &mut u8, range: std::ops::RangeInclusive<u8>) -> egui::Response {
    ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(80.0);
    ui.add(egui::Slider::new(val, range).show_value(false).trailing_fill(true))
}

/// Title + subtitle on the left, content (buttons) laid out right-to-left on the right.
pub fn labeled_row(ui: &mut egui::Ui, title: &str, subtitle: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 120.0).max(120.0));
            ui.label(RichText::new(title).size(14.0).color(TEXT_PRIMARY));
            ui.label(RichText::new(subtitle).size(11.5).color(TEXT_MUTED));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

pub fn toggle_row(ui: &mut egui::Ui, title: &str, subtitle: &str, on: &mut bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 60.0).max(120.0));
            ui.label(RichText::new(title).size(14.0).color(TEXT_PRIMARY));
            ui.label(RichText::new(subtitle).size(11.5).color(TEXT_MUTED));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_switch(ui, on).clicked() {
                clicked = true;
            }
        });
    });
    clicked
}

/// One UI style switch: blue track when on, grey when off, white thumb.
pub fn pill_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired = Vec2::new(46.0, 26.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let off_bg = Color32::from_rgb(0x58, 0x58, 0x5C);
    let bg = Color32::from_rgb(
        (off_bg.r() as f32 + (ACCENT_BLUE.r() as f32 - off_bg.r() as f32) * t) as u8,
        (off_bg.g() as f32 + (ACCENT_BLUE.g() as f32 - off_bg.g() as f32) * t) as u8,
        (off_bg.b() as f32 + (ACCENT_BLUE.b() as f32 - off_bg.b() as f32) * t) as u8,
    );
    let r = rect.height() * 0.5;
    ui.painter().rect_filled(rect, CornerRadius::same(r as u8), bg);
    let cx = rect.left() + r + (rect.width() - 2.0 * r) * t;
    ui.painter().circle_filled(egui::pos2(cx, rect.center().y), r - 3.0, Color32::WHITE);
    resp
}
