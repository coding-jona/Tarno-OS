//! Proaktiver Tuning-Task für Tarno AI (Phase 1, siehe
//! docs/month3-tarno-layer.md#tarno-ai). Rein regelbasiert, kein LLM:
//! periodisch `SystemContext` neu sammeln, gegen ein paar einfache Regeln
//! prüfen, bei Treffer eine Vorschlags-Zeile in `AiState::suggestions`
//! pushen. Struktur (eigener `tokio::spawn`'ter Endlos-Loop neben dem
//! IPC-Server) ist analog zum eBPF-RingBuf-Poller in `security/ebpf_loader.rs`
//! bzw. dessen Spawn in `main.rs`.

use std::sync::Arc;
use std::time::Duration;

use super::backend::SystemContext;
use crate::AppState;

/// Poll-Intervall. Bewusst lang gewählt: dies ist ein Hintergrund-Task in
/// einem Daemon, kein interaktiver Prozess — RAM-/Gaming-Mode-Zustand
/// ändert sich nicht so schnell, dass ein kürzeres Intervall sinnvolle
/// zusätzliche Vorschläge liefern würde, aber es würde unnötig CPU-Zeit
/// kosten (siehe `docs/month3-tarno-layer.md`, IPC-Design-Begründung für
/// dieselbe "kein unnötiges Polling"-Haltung).
const TUNING_INTERVAL: Duration = Duration::from_secs(30);

/// Schwelle: unter diesem Prozentsatz verfügbarem RAM gilt der Speicher
/// als "knapp" im Sinne eines Tuning-Vorschlags.
const LOW_MEM_AVAILABLE_PERCENT: u64 = 15;

/// Läuft, bis der Prozess beendet wird — wird ungebunden per `tokio::spawn`
/// neben `ipc::serve` gestartet (siehe `main.rs`).
pub async fn run(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(TUNING_INTERVAL);
    loop {
        interval.tick().await;
        let ctx = SystemContext::gather(&state.gaming);
        if let Some(suggestion) = evaluate(&ctx) {
            state.ai.push_suggestion(suggestion);
        }
    }
}

/// Reine Regel-Auswertung, getrennt von der Loop, damit sie ohne Timer/IO
/// testbar ist.
fn evaluate(ctx: &SystemContext) -> Option<String> {
    let (total, available) = match (ctx.mem_total_kb, ctx.mem_available_kb) {
        (Some(t), Some(a)) if t > 0 => (t, a),
        _ => return None,
    };
    let available_percent = available.saturating_mul(100) / total;
    if available_percent < LOW_MEM_AVAILABLE_PERCENT && !ctx.gaming_mode_active {
        return Some(format!(
            "RAM knapp ({available_percent}% verfügbar), aber Gaming-Mode ist aus \
             — falls gerade eine Last läuft, könnte Gaming-Mode (Governor \
             'performance', THP 'madvise') helfen."
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(total_kb: u64, available_kb: u64, gaming_mode_active: bool) -> SystemContext {
        SystemContext {
            gaming_mode_active,
            isolated_cpus: None,
            ebpf_active: false,
            mem_total_kb: Some(total_kb),
            mem_available_kb: Some(available_kb),
        }
    }

    #[test]
    fn suggests_gaming_mode_when_ram_low_and_gaming_off() {
        let c = ctx(1_000_000, 100_000, false); // 10% verfügbar
        assert!(evaluate(&c).is_some());
    }

    #[test]
    fn no_suggestion_when_ram_ok() {
        let c = ctx(1_000_000, 500_000, false); // 50% verfügbar
        assert!(evaluate(&c).is_none());
    }

    #[test]
    fn no_suggestion_when_gaming_mode_already_active() {
        let c = ctx(1_000_000, 50_000, true); // 5% verfügbar, aber schon an
        assert!(evaluate(&c).is_none());
    }

    #[test]
    fn no_suggestion_without_meminfo() {
        let c = SystemContext {
            gaming_mode_active: false,
            isolated_cpus: None,
            ebpf_active: false,
            mem_total_kb: None,
            mem_available_kb: None,
        };
        assert!(evaluate(&c).is_none());
    }
}
