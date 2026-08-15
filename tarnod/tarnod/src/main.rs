//! `tarnod` — Tarno OS Root-Daemon.
//!
//! Verantwortlich für IPC (Unix-Socket), API-Key-Vault, Gaming-Mode-Steuerung
//! und (mit Feature `ebpf`) das Behavioral-Security-Subsystem. Architektur:
//! docs/architecture.md.

mod config;
mod gaming;
mod ipc;
mod process_ctl;
mod security;
mod vault;

use std::sync::Arc;

use config::Config;
use gaming::GamingController;
use tarnod_protocol::{Request, Response};
use vault::Vault;

pub struct AppState {
    pub vault: Vault,
    pub gaming: GamingController,
    pub config: Config,
}

/// Verarbeitet eine einzelne Anfrage gegen den aktuellen Daemon-Zustand.
/// Reine Funktion (kein IO-Framework-Bezug) — leicht testbar, siehe unten.
pub fn dispatch(state: &AppState, req: Request) -> Response {
    match req {
        Request::Ping => Response::ok(serde_json::json!("pong")),
        Request::GetGamingMode => match state.gaming.isolated_cpus() {
            Ok(isolated) => Response::ok(serde_json::json!({ "isolated_cpus": isolated })),
            Err(e) => Response::err(e.to_string()),
        },
        Request::SetGamingMode { enabled } => {
            let governor = if enabled { "performance" } else { "powersave" };
            match state.gaming.set_governor(governor) {
                Ok(()) => Response::ok(serde_json::json!({ "gaming_mode": enabled })),
                Err(e) => Response::err(e.to_string()),
            }
        }
        Request::GetApiKey { name } => match state.vault.get(&name) {
            Some(value) => Response::ok(serde_json::json!({ "name": name, "value": value })),
            None => Response::err(format!("unknown key: {name}")),
        },
        Request::SecurityStatus => {
            Response::ok(serde_json::json!({ "ebpf_active": cfg!(feature = "ebpf") }))
        }
        Request::ResumeProcess { pid } => match process_ctl::resume(pid) {
            Ok(()) => Response::ok(serde_json::json!({ "resumed": pid })),
            Err(e) => Response::err(e.to_string()),
        },
    }
}

fn main() -> anyhow::Result<()> {
    let config = Config::load_default();

    let vault = match Vault::load_from_file(&config.vault_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "tarnod: warnung: konnte Vault nicht aus {:?} laden ({e}) — starte mit leerem Vault",
                config.vault_path
            );
            Vault::default()
        }
    };

    let gaming = GamingController::new(config.dry_run);
    let state = Arc::new(AppState {
        vault,
        gaming,
        config,
    });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        #[cfg(feature = "ebpf")]
        {
            let policy = security::ebpf_loader::Policy::from_env();
            tokio::spawn(async move {
                if let Err(e) = security::ebpf_loader::run(policy).await {
                    eprintln!("tarnod: eBPF-Security-Subsystem beendet mit Fehler: {e}");
                }
            });
        }
        ipc::serve(state).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn state_with_vault(vault: Vault) -> AppState {
        AppState {
            vault,
            gaming: GamingController::new(true),
            config: Config {
                socket_dir: PathBuf::from("/tmp/tarnod-test"),
                socket_path: PathBuf::from("/tmp/tarnod-test/tarnod.sock"),
                vault_path: PathBuf::from("/nonexistent"),
                dry_run: true,
            },
        }
    }

    #[test]
    fn dispatch_ping_returns_pong() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::Ping);
        assert!(matches!(resp, Response::Ok { .. }));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("pong"));
    }

    #[test]
    fn dispatch_get_api_key_found() {
        let state = state_with_vault(Vault::parse("SECRET=hunter2\n"));
        let resp = dispatch(
            &state,
            Request::GetApiKey {
                name: "SECRET".into(),
            },
        );
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("hunter2"));
    }

    #[test]
    fn dispatch_get_api_key_missing_returns_error() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(
            &state,
            Request::GetApiKey {
                name: "MISSING".into(),
            },
        );
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn dispatch_security_status_reflects_feature_flag() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::SecurityStatus);
        let s = serde_json::to_string(&resp).unwrap();
        // Ohne "ebpf"-Feature (Default-Build) muss dies false sein.
        assert!(s.contains("\"ebpf_active\":false") || cfg!(feature = "ebpf"));
    }
}
