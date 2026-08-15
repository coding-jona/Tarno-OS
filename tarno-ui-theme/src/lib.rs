//! Geteiltes visuelles Theme für alle Tarno-OS-GUIs: dunkel, ruhig,
//! "Stealth-Tech" — passend zum Namen "Tarno" (von "tarnen"). Ein
//! Farbsatz, konsequent auf Panels/Buttons/Status-Badges angewendet statt
//! egui-Defaults zu benutzen, damit `tarnod-ui` und `tarno-installer`
//! (und künftige Tarno-OS-Tools) optisch aus einem Guss wirken.

use egui::{Color32, CornerRadius, Margin, Stroke};

pub const BG_APP: Color32 = Color32::from_rgb(0x0d, 0x0f, 0x13);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x15, 0x18, 0x1f);
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x1c, 0x20, 0x29);
pub const BG_WIDGET_HOVER: Color32 = Color32::from_rgb(0x24, 0x29, 0x35);
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x3a);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xe8, 0xea, 0xed);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa1);

pub const ACCENT: Color32 = Color32::from_rgb(0x2d, 0xd4, 0xbf); // teal/mint
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x1b, 0x4b, 0x47);
pub const SUCCESS: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
pub const WARNING: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const DANGER: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub const DANGER_DIM: Color32 = Color32::from_rgb(0x4a, 0x1f, 0x1f);

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.panel_fill = BG_APP;
    visuals.window_fill = BG_PANEL;
    visuals.faint_bg_color = BG_WIDGET;
    visuals.extreme_bg_color = BG_APP;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);

    visuals.widgets.inactive.bg_fill = BG_WIDGET;
    visuals.widgets.inactive.weak_bg_fill = BG_WIDGET;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);

    visuals.widgets.hovered.bg_fill = BG_WIDGET_HOVER;
    visuals.widgets.hovered.weak_bg_fill = BG_WIDGET_HOVER;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.2, ACCENT);

    visuals.widgets.active.bg_fill = ACCENT_DIM;
    visuals.widgets.active.weak_bg_fill = ACCENT_DIM;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_stroke = Stroke::new(1.2, ACCENT);

    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        w.corner_radius = CornerRadius::same(8);
    }

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.window_margin = Margin::same(16);
    ctx.set_style(style);
}

/// Karten-artiges Panel (Frame mit Border + abgerundeten Ecken), das
/// konsistente Innenabstände für alle Sections der App liefert.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .outer_margin(Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: (ui.spacing().item_spacing.y) as i8,
        })
}

/// Wie `card`, aber mit rotem Warn-Rand — für gefährliche/destruktive
/// Aktionen (z.B. die Bestätigung vor dem Überschreiben eines Datenträgers
/// in `tarno-installer`).
pub fn danger_card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(DANGER_DIM)
        .stroke(Stroke::new(1.4, DANGER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .outer_margin(Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: (ui.spacing().item_spacing.y) as i8,
        })
}
