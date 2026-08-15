//! `tarno-desktop` — Tarno OS Desktop-Modus.
//!
//! Eigener, minimaler Wayland-Compositor (smithay) mit direkt eingebauter
//! Taskleiste (kein separater Panel-Client über wlr-layer-shell — spart
//! einen kompletten zweiten Prozess samt eigenem Event-Loop/GL-Context und
//! die IPC-Overhead zwischen Compositor und Panel). Siehe
//! docs/architecture.md und docs/month2-gaming-tuning.md für die
//! Dual-Mode-Begründung (Gaming bleibt `cage`, das hier ist nur für den
//! Nicht-Gaming-Betrieb).
//!
//! Entwicklungs-/Test-Backend: `winit` (läuft genestet in einem
//! bestehenden X11/Wayland-Fenster, funktioniert unter Xvfb — siehe
//! `docs/month-desktop.md`). Ein echter DRM/KMS-Backend-Pfad für
//! Bare-Metal-Boot auf dem M6700 ist als Stage-2-Arbeit vorgesehen, siehe
//! Scope-Hinweis dort.

mod clock;
mod state;
mod tarnod_client;
mod taskbar;
mod text;

fn main() {
    env_logger_init();
    state::run();
}

fn env_logger_init() {
    // Kein extra Logging-Crate-Dependency: minimales Format direkt auf stderr.
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "warn");
    }
}
