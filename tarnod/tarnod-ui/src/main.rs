//! `tarnod-ui` — natives GUI für `tarnod` (egui/eframe). Verbindet sich auf
//! denselben Unix-Socket wie `tarnoctl`, zeigt Gaming-Mode- und
//! Security-Status und erlaubt einfache Steuerung. Siehe
//! docs/architecture.md.
//!
//! Env-Variablen:
//!   TARNOD_SOCKET          Pfad zum tarnod-Socket (Default: /run/tarnod/tarnod.sock)
//!   TARNOD_UI_SCREENSHOT   wenn gesetzt: nach dem ersten Render einen
//!                          Screenshot an diesen Pfad schreiben und beenden
//!                          (Verifikations-/Testhilfe, kein Teil des
//!                          normalen Betriebs)

mod app;
mod client;

use std::path::PathBuf;

use eframe::egui;
use tarno_ui_theme as theme;

fn socket_path() -> PathBuf {
    std::env::var("TARNOD_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/tarnod/tarnod.sock"))
}

fn main() -> eframe::Result<()> {
    let screenshot_path = std::env::var("TARNOD_UI_SCREENSHOT").ok().map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([880.0, 560.0])
            .with_min_inner_size([680.0, 440.0])
            .with_title("Tarno OS — tarnod"),
        ..Default::default()
    };

    eframe::run_native(
        "tarnod-ui",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app::TarnodApp::new(socket_path(), screenshot_path)))
        }),
    )
}
