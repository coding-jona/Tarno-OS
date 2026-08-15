//! Geteiltes visuelles Theme für alle Tarno-OS-GUIs: dunkel, Cyan-Akzent,
//! "Glass-lite" — an die Optik der TARNO-Windows-App (WinUI 3, "Liquid
//! Glass", `#0BC7FF`-Akzent, BuildMC-Surface-Palette) angelehnt, damit die
//! Marke über alle Tarno-Werkzeuge hinweg konsistent wirkt.
//!
//! Bewusst **abgespeckt**: TARNO nutzt für den Glass-Look echte
//! Compositor-Materialien (Mica/Acrylic, GPU-Backdrop-Blur). Das käme uns
//! hier teuer zu stehen — dieses Projekt ist auf niedrigen Ressourcen-
//! Verbrauch getrimmt (siehe ROADMAP.md), und egui/eframe ist
//! Immediate-Mode ohne Compositor-Blur-Pipeline. Der "Glass"-Eindruck kommt
//! deshalb ausschließlich aus billigen Mitteln: leicht transparente
//! Flächen + eine dünne helle Linie an der Karten-Oberkante (`glass_card`)
//! statt echtem Blur-hinter-dem-Panel — ein zusätzlicher gefüllter
//! Rechteck-Draw-Call pro Karte, keine zusätzliche Rendering-Pass.

use egui::{Color32, CornerRadius, InnerResponse, Margin, Stroke};

// BuildMC-Surface-Palette (aus der TARNO-Windows-App übernommen).
pub const BG_APP: Color32 = Color32::from_rgb(0x0a, 0x0a, 0x0f); // Surface0
pub const BG_PANEL: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x25); // Surface2 (Karten)
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x12, 0x12, 0x1a); // Surface1 (Inputs)
pub const BG_WIDGET_HOVER: Color32 = Color32::from_rgb(0x22, 0x22, 0x2f); // Surface3
pub const BG_WIDGET_ACTIVE: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x38); // Surface4
pub const BORDER: Color32 = Color32::from_rgb(0x22, 0x22, 0x2f); // Surface3

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xee, 0xee, 0xee);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x88, 0x90, 0xa4);

/// Cyan-500 — TARNOs Marken-Akzentfarbe (`#0BC7FF`), 1:1 übernommen.
pub const ACCENT: Color32 = Color32::from_rgb(0x0b, 0xc7, 0xff);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x33, 0xd1, 0xff); // Cyan-400
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x0f, 0x2e, 0x38); // dunkel getönt, für aktive Flächen
pub const SUCCESS: Color32 = Color32::from_rgb(0x2d, 0xd4, 0xbf); // Turquoise-500
pub const WARNING: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b); // Orange-500
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x44, 0x44); // Red-500
pub const DANGER_DIM: Color32 = Color32::from_rgb(0x3a, 0x14, 0x14);

/// Halbtransparentes Weiß für den Glass-Highlight-Strich (billiger Ersatz
/// für echten Backdrop-Blur, siehe Modul-Kommentar).
const GLASS_HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(0xff, 0xff, 0xff, 18);

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
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.2, ACCENT_HOVER);

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

/// Innenabstand aller Glass-Karten — geteilt zwischen `glass_frame` (setzt
/// ihn als `inner_margin`) und `paint_glass_highlight` (muss wissen, wie
/// weit der Karten-Inhalt vom tatsächlichen Kartenrand eingerückt ist, um
/// die Highlight-Linie richtig direkt unter dem Rand zu platzieren).
const CARD_INNER_MARGIN: f32 = 16.0;

fn card_outer_margin(ui: &egui::Ui) -> Margin {
    Margin {
        left: 0,
        right: 0,
        top: 0,
        bottom: ui.spacing().item_spacing.y as i8,
    }
}

/// Zeichnet die "Glass"-Karte: Panel mit Border + abgerundeten Ecken, plus
/// ein dünner heller Strich an der Innen-Oberkante — der einzige
/// Glass-Kostenfaktor ist dieser eine zusätzliche gefüllte Rechteck-Call,
/// kein Blur/Compositor-Effekt. Siehe Modul-Kommentar.
pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> InnerResponse<R> {
    glass_frame(BG_PANEL, Stroke::new(1.0, BORDER))
        .outer_margin(card_outer_margin(ui))
        .show(ui, |ui| {
            paint_glass_highlight(ui);
            add_contents(ui)
        })
}

/// Wie `card`, aber mit rotem Warn-Rand — für gefährliche/destruktive
/// Aktionen (z.B. die Bestätigung vor dem Überschreiben eines
/// Datenträgers in `tarno-installer`).
pub fn danger_card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> InnerResponse<R> {
    glass_frame(DANGER_DIM, Stroke::new(1.4, DANGER))
        .outer_margin(card_outer_margin(ui))
        .show(ui, |ui| {
            paint_glass_highlight(ui);
            add_contents(ui)
        })
}

fn glass_frame(fill: Color32, stroke: Stroke) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(CARD_INNER_MARGIN as i8))
}

/// Dünner, halbtransparenter Strich knapp innerhalb der Karten-Oberkante —
/// simuliert das Licht, das eine echte Glaskante einfangen würde, ohne
/// tatsächlich etwas zu weichzeichnen. Muss aus der Content-Closure heraus
/// aufgerufen werden (bevor Inhalt hinzugefügt wird), damit `ui.max_rect()`
/// noch die volle Innenfläche der Karte liefert.
fn paint_glass_highlight(ui: &mut egui::Ui) {
    let content_rect = ui.max_rect();
    let card_top = content_rect.top() - CARD_INNER_MARGIN;
    let inset = 10.0;
    if content_rect.width() <= inset * 2.0 {
        return;
    }
    let line = egui::Rect::from_min_max(
        egui::pos2(content_rect.left() + inset, card_top + 1.0),
        egui::pos2(content_rect.right() - inset, card_top + 1.7),
    );
    ui.painter().rect_filled(line, 0.0, GLASS_HIGHLIGHT);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reine Farbwert-Sanity-Checks: die Markenfarbe muss exakt TARNOs
    /// Cyan-500 sein, nicht versehentlich verwässert werden.
    #[test]
    fn accent_matches_tarno_brand_cyan() {
        assert_eq!(ACCENT, Color32::from_rgb(0x0b, 0xc7, 0xff));
    }

    #[test]
    fn success_matches_tarno_turquoise() {
        assert_eq!(SUCCESS, Color32::from_rgb(0x2d, 0xd4, 0xbf));
    }
}
