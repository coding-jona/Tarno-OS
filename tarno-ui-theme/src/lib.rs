//! Geteiltes visuelles Theme für alle Tarno-OS-GUIs: an Windows 11 / Fluent
//! Design 2 angelehnt (echte neutrale Mica-Grautöne, kleine Control-Radien,
//! zurückhaltender Akzenteinsatz), mit beibehaltenem Cyan-Akzent (`#0BC7FF`,
//! aus der `tarno`-Windows-App übernommen — bewusste Ausnahme, kein
//! Windows-11-Systemblau).
//!
//! Bewusst **abgespeckt**: echtes Mica/Acrylic ist Compositor-Backdrop-Blur
//! (GPU-Kosten). Dieses Projekt ist auf niedrigen Ressourcenverbrauch
//! getrimmt (siehe ROADMAP.md), und egui/eframe ist Immediate-Mode ohne
//! Compositor-Blur-Pipeline. Der "Glass"-Eindruck kommt deshalb aus
//! billigen Mitteln: ein Karten-Rand als dünner, halbtransparenter Strich
//! (`card`/`danger_card`) statt echtem Blur-hinter-dem-Panel — ein
//! zusätzlicher gefüllter Rechteck-Draw-Call pro Karte, keine zusätzliche
//! Rendering-Pass. Aus dem gleichen Grund: kein Segoe UI (Windows-System-
//! font, nicht frei redistributierbar) — egui bleibt beim eingebauten Font.
//!
//! Farbwerte sind Fluent-2-*inspiriert*, keine exakt aus Microsofts
//! Design-Tokens kopierten Werte (die sind nicht offen dokumentiert) —
//! Ziel ist ein sofort als "Windows 11" erkennbarer neutraler Grauton statt
//! der vorherigen blaustichigen Palette, nicht Pixel-Identität.

use egui::{Color32, CornerRadius, InnerResponse, Margin, Stroke};

// Neutrale Mica-Grauskala (Fluent 2, dunkel) — bewusst blaustichfrei, im
// Gegensatz zur vorherigen BuildMC-Palette.
pub const BG_APP: Color32 = Color32::from_rgb(0x20, 0x20, 0x20); // Mica-Basis
pub const BG_PANEL: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x2c); // Karten/Layer
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x33, 0x33, 0x33); // Control-Ruhezustand
pub const BG_WIDGET_HOVER: Color32 = Color32::from_rgb(0x3d, 0x3d, 0x3d);
pub const BG_WIDGET_ACTIVE: Color32 = Color32::from_rgb(0x26, 0x26, 0x26); // gedrückt: dunkler, nicht heller
/// Dünner, halbtransparenter weißer Strich statt flachem Grau — Fluents
/// "Card Stroke" auf Mica ist eine leichte Aufhellung, kein harter Rand.
pub const BORDER: Color32 = Color32::from_rgba_premultiplied(0xff, 0xff, 0xff, 20);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xf3, 0xf3, 0xf3);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0xb3, 0xb3, 0xb3);

/// Cyan-500 (`#0BC7FF`) — bewusst beibehaltene Ausnahme von reinem
/// Windows-11-Look, siehe Modul-Kommentar.
pub const ACCENT: Color32 = Color32::from_rgb(0x0b, 0xc7, 0xff);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x33, 0xd1, 0xff);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x16, 0x31, 0x38);
pub const SUCCESS: Color32 = Color32::from_rgb(0x6c, 0xcb, 0x5f); // Fluent-Grün
pub const WARNING: Color32 = Color32::from_rgb(0xff, 0xb9, 0x00); // Fluent-Amber
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x6a, 0x5f); // Fluent-Rot (Dark-Theme-Kontrast)
pub const DANGER_DIM: Color32 = Color32::from_rgb(0x3d, 0x1f, 0x1c);

/// Fluent `ControlCornerRadius` — kleine Controls (Buttons, Inputs).
const CONTROL_RADIUS: u8 = 4;
/// Fluent `OverlayCornerRadius` — größere Flächen (Karten, Dialoge).
const SURFACE_RADIUS: u8 = 8;

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

    visuals.widgets.active.bg_fill = BG_WIDGET_ACTIVE;
    visuals.widgets.active.weak_bg_fill = BG_WIDGET_ACTIVE;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_stroke = Stroke::new(1.2, ACCENT);

    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        w.corner_radius = CornerRadius::same(CONTROL_RADIUS);
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
        .corner_radius(CornerRadius::same(SURFACE_RADIUS))
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

/// Windows-11-artiger Auswahl-Indikator für Navigationselemente: ein
/// schmaler, abgerundeter Akzent-Balken am linken Rand des ausgewählten
/// Eintrags (WinUI3 `NavigationView`-Muster) — statt einer vollflächigen
/// akzentgefärbten Fläche.
pub fn paint_nav_selection_bar(ui: &egui::Ui, item_rect: egui::Rect) {
    let bar = egui::Rect::from_min_max(
        egui::pos2(item_rect.left(), item_rect.top() + 6.0),
        egui::pos2(item_rect.left() + 3.0, item_rect.bottom() - 6.0),
    );
    ui.painter()
        .rect_filled(bar, CornerRadius::same(2), ACCENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reine Farbwert-Sanity-Checks: die Akzentfarbe muss exakt TARNOs
    /// Cyan-500 bleiben — bewusst beibehaltene Ausnahme vom sonst
    /// Windows-11-Fluent-Look, siehe Modul-Kommentar.
    #[test]
    fn accent_stays_tarno_brand_cyan() {
        assert_eq!(ACCENT, Color32::from_rgb(0x0b, 0xc7, 0xff));
    }

    #[test]
    fn background_is_neutral_not_blue_tinted() {
        // BG_APP muss ein echtes Neutralgrau sein (R=G=B) statt der
        // vorherigen blaustichigen BuildMC-Palette (R<G<B).
        assert_eq!(BG_APP.r(), BG_APP.g());
        assert_eq!(BG_APP.g(), BG_APP.b());
    }
}
