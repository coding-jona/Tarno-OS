//! Laufzeit-Konfiguration von `tarnod`. Über Umgebungsvariablen steuerbar,
//! damit sich der Daemon sowohl im echten OpenRC-Betrieb
//! (`board/tarno-m6700/rootfs-overlay/etc/init.d/S60tarnod`) als auch für
//! lokale Tests/Sandboxes ohne echtes `/run`/`/etc` konfigurieren lässt.

use std::path::PathBuf;

pub struct Config {
    pub socket_dir: PathBuf,
    pub socket_path: PathBuf,
    pub vault_path: PathBuf,
    pub dry_run: bool,
}

impl Config {
    pub fn load_default() -> Self {
        let socket_dir = std::env::var("TARNOD_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/run/tarnod"));
        let socket_path = socket_dir.join("tarnod.sock");
        let vault_path = std::env::var("TARNOD_VAULT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/tarnod/secrets.conf"));
        let dry_run = std::env::var("TARNOD_DRY_RUN")
            .map(|v| v == "1")
            .unwrap_or(false);
        Self {
            socket_dir,
            socket_path,
            vault_path,
            dry_run,
        }
    }
}
