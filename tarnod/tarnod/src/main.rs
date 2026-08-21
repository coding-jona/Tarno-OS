//! `tarnod` — Tarno OS Root-Daemon.
//!
//! Verantwortlich für IPC (Unix-Socket), API-Key-Vault, Gaming-Mode-Steuerung
//! und (mit Feature `ebpf`) das Behavioral-Security-Subsystem. Architektur:
//! docs/architecture.md.

mod ai;
mod config;
mod gaming;
mod ipc;
mod process_ctl;
mod security;
mod vault;

use std::sync::Arc;

use ai::AiState;
use config::Config;
use gaming::GamingController;
use tarnod_protocol::{Request, Response};
use vault::Vault;

pub struct AppState {
    pub vault: Vault,
    pub gaming: GamingController,
    pub config: Config,
    pub ai: AiState,
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
        Request::AiQuery { text } => {
            let ctx = ai::backend::SystemContext::gather(&state.gaming);
            let answer = state.ai.answer(&text, &ctx);
            Response::ok(serde_json::json!({ "answer": answer }))
        }
        Request::AiStatus => {
            let ctx = ai::backend::SystemContext::gather(&state.gaming);
            Response::ok(serde_json::json!({
                "gaming_mode_active": ctx.gaming_mode_active,
                "isolated_cpus": ctx.isolated_cpus,
                "ebpf_active": ctx.ebpf_active,
                "mem_total_kb": ctx.mem_total_kb,
                "mem_available_kb": ctx.mem_available_kb,
            }))
        }
        Request::AiSuggestions => {
            Response::ok(serde_json::json!({ "suggestions": state.ai.suggestions() }))
        }
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
        ai: AiState::new(),
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
        // Tarno-AI-Tuning-Task: läuft unabhängig vom `ebpf`-Feature, siehe
        // ai/tuning.rs und docs/month3-tarno-layer.md#tarno-ai.
        tokio::spawn(ai::tuning::run(Arc::clone(&state)));
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
            ai: AiState::new(),
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

    #[test]
    fn dispatch_ai_query_returns_grounded_answer() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(
            &state,
            Request::AiQuery {
                text: "ist gaming mode an?".into(),
            },
        );
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"status\":\"ok\""));
        assert!(s.contains("Gaming-Mode"));
    }

    #[test]
    fn dispatch_ai_status_returns_system_context() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::AiStatus);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("gaming_mode_active"));
        assert!(s.contains("ebpf_active"));
    }

    #[test]
    fn dispatch_ai_suggestions_starts_empty() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::AiSuggestions);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"suggestions\":[]"));
    }
}
