//! `tarno-installer` — natives GUI zum Schreiben eines Tarno-OS-USB-Boot-
//! Images auf einen Wechseldatenträger. Siehe docs/architecture.md und
//! docs/month1-foundation.md#usb-boot-image-anforderung-installationauslieferung-per-usb-stick.
//!
//! Sicherheitsprinzip: nur Geräte, die der Kernel als "removable" markiert
//! (siehe `devices.rs`), werden überhaupt zur Auswahl angeboten; das
//! Root-Gerät ist zusätzlich explizit ausgeschlossen. Vor dem eigentlichen
//! Schreiben muss der Nutzer eine explizite Bestätigung anhaken (siehe
//! `app.rs`). Dieses Werkzeug läuft auf dem Rechner, der den Stick
//! erstellt (nicht auf Tarno OS selbst) — vergleichbar mit Raspberry Pi
//! Imager / Rufus / balenaEtcher.
//!
//! Env-Variablen:
//!   TARNO_INSTALLER_SCREENSHOT   wenn gesetzt: nach dem ersten Render
//!                                 einen Screenshot an diesen Pfad
//!                                 schreiben und beenden (Verifikations-/
//!                                 Testhilfe, kein Teil des normalen
//!                                 Betriebs)

mod app;
mod devices;
mod flasher;

use std::path::PathBuf;

use eframe::egui;
use tarno_ui_theme as theme;

fn main() -> eframe::Result<()> {
    let screenshot_path = std::env::var("TARNO_INSTALLER_SCREENSHOT").ok().map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([600.0, 480.0])
            .with_title("Tarno OS — Installer"),
        ..Default::default()
    };

    eframe::run_native(
        "tarno-installer",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app::InstallerApp::new(screenshot_path)))
        }),
    )
}
