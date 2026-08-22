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
    /// Tarno-AI-Phase-3 (siehe docs/month3-tarno-layer.md#tarno-ai):
    /// beschränkter Log der jüngsten `ExecEvent`s, den
    /// `security::ebpf_loader::run` (Feature `ebpf`) befüllt und den
    /// `AiQuery`/`AiStatus` nur lesend konsumieren. Unabhängig vom `ebpf`-
    /// Feature immer vorhanden — ohne das Feature bleibt er schlicht immer
    /// leer, siehe `security::events`.
    pub security_events: security::events::SecurityEventLog,
}

/// Verarbeitet eine einzelne Anfrage gegen den aktuellen Daemon-Zustand.
/// Reine Funktion (kein IO-Framework-Bezug, außer dem für `AiQuery` nötigen
/// `.await` seit Tarno-AI-Phase-2) — leicht testbar, siehe unten. `async`
/// seit Phase 2, weil `AiState::answer` bei aktivem Mistral-Backend einen
/// Netzwerk-Request macht; `ipc.rs`s `handle_client` ist bereits `async`,
/// daher passt ein `.await` hier ohne neues Concurrency-Modell (siehe
/// docs/month3-tarno-layer.md#ipc-design).
pub async fn dispatch(state: &AppState, req: Request) -> Response {
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
            let ctx = ai::backend::SystemContext::gather(&state.gaming, &state.security_events);
            let answer = state.ai.answer(&text, &ctx).await;
            Response::ok(serde_json::json!({ "answer": answer }))
        }
        Request::AiStatus => {
            let ctx = ai::backend::SystemContext::gather(&state.gaming, &state.security_events);
            let mistral_configured = state.ai.mistral_configured();
            // Ohne konfigurierten Mistral-Key läuft Tarno AI im reinen
            // Heuristik-Modus — das soll für `tarnoctl ai status` sofort
            // erkennbar sein, inklusive eines Zeigers auf die Setup-Doku,
            // statt den Nutzer raten zu lassen, warum Antworten "einfach"
            // bleiben. Siehe docs/month3-tarno-layer.md#mistral-api-key-einrichten.
            let mode_note = if mistral_configured {
                "Tarno AI läuft im Mistral-Modus (Phase 2) mit Heuristik-Fallback bei Netzwerk-/API-Fehlern.".to_string()
            } else {
                format!(
                    "Kein {} in der Vault konfiguriert — Tarno AI läuft im \
                     Heuristik-Modus (Phase 1). Siehe \
                     docs/month3-tarno-layer.md#mistral-api-key-einrichten \
                     für die Einrichtung.",
                    ai::MISTRAL_API_KEY_NAME
                )
            };
            Response::ok(serde_json::json!({
                "gaming_mode_active": ctx.gaming_mode_active,
                "isolated_cpus": ctx.isolated_cpus,
                "ebpf_active": ctx.ebpf_active,
                "mem_total_kb": ctx.mem_total_kb,
                "mem_available_kb": ctx.mem_available_kb,
                "mistral_configured": mistral_configured,
                "mode_note": mode_note,
                // Tarno-AI-Phase-3: siehe docs/month3-tarno-layer.md#tarno-ai.
                "recent_security_stops": ctx.recent_security_stops,
                "last_stopped_comm": ctx.last_stopped_comm,
                "last_stopped_filename": ctx.last_stopped_filename,
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
    // `AiState::from_vault` wählt Mistral+Fallback (Phase 2) vs. reine
    // Heuristik (Phase 1) je nachdem, ob MISTRAL_API_KEY in der Vault
    // steht — siehe docs/month3-tarno-layer.md#mistral-api-key-einrichten.
    let ai = AiState::from_vault(&vault);
    let state = Arc::new(AppState {
        vault,
        gaming,
        config,
        ai,
        security_events: security::events::SecurityEventLog::new(),
    });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        #[cfg(feature = "ebpf")]
        {
            let policy = security::ebpf_loader::Policy::from_env();
            let ebpf_state = Arc::clone(&state);
            tokio::spawn(async move {
                if let Err(e) = security::ebpf_loader::run(policy, ebpf_state).await {
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
        // `AiState::from_vault` statt `AiState::new()`, damit Tests, die
        // gezielt einen MISTRAL_API_KEY in der Vault setzen (siehe unten),
        // auch tatsächlich das erwartete Backend/den erwarteten
        // AiStatus-Modus sehen.
        let ai = AiState::from_vault(&vault);
        AppState {
            vault,
            gaming: GamingController::new(true),
            config: Config {
                socket_dir: PathBuf::from("/tmp/tarnod-test"),
                socket_path: PathBuf::from("/tmp/tarnod-test/tarnod.sock"),
                vault_path: PathBuf::from("/nonexistent"),
                dry_run: true,
            },
            ai,
            security_events: security::events::SecurityEventLog::new(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_ping_returns_pong() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::Ping).await;
        assert!(matches!(resp, Response::Ok { .. }));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("pong"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_get_api_key_found() {
        let state = state_with_vault(Vault::parse("SECRET=hunter2\n"));
        let resp = dispatch(
            &state,
            Request::GetApiKey {
                name: "SECRET".into(),
            },
        )
        .await;
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("hunter2"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_get_api_key_missing_returns_error() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(
            &state,
            Request::GetApiKey {
                name: "MISSING".into(),
            },
        )
        .await;
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_security_status_reflects_feature_flag() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::SecurityStatus).await;
        let s = serde_json::to_string(&resp).unwrap();
        // Ohne "ebpf"-Feature (Default-Build) muss dies false sein.
        assert!(s.contains("\"ebpf_active\":false") || cfg!(feature = "ebpf"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_ai_query_returns_grounded_answer() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(
            &state,
            Request::AiQuery {
                text: "ist gaming mode an?".into(),
            },
        )
        .await;
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"status\":\"ok\""));
        assert!(s.contains("Gaming-Mode"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_ai_status_returns_system_context() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::AiStatus).await;
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("gaming_mode_active"));
        assert!(s.contains("ebpf_active"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_ai_status_reports_heuristic_mode_without_mistral_key() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::AiStatus).await;
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"mistral_configured\":false"));
        assert!(s.contains("mistral-api-key-einrichten"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_ai_status_reports_mistral_mode_when_key_configured() {
        let state = state_with_vault(Vault::parse("MISTRAL_API_KEY=test-key-123\n"));
        let resp = dispatch(&state, Request::AiStatus).await;
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"mistral_configured\":true"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_ai_suggestions_starts_empty() {
        let state = state_with_vault(Vault::default());
        let resp = dispatch(&state, Request::AiSuggestions).await;
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"suggestions\":[]"));
    }
}
