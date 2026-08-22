//! Phase-1-`AiBackend`: mustererkennt eine kleine Menge bekannter
//! Fragenformen (deutsch/englisch) und antwortet templated, aber anhand
//! von echtem, live gelesenem System-Zustand (`SystemContext`) — kein LLM,
//! keine Halluzination möglich, weil nichts generiert wird. Details/Scope:
//! docs/month3-tarno-layer.md#tarno-ai.

use async_trait::async_trait;

use super::backend::{AiBackend, SystemContext};

pub struct HeuristicBackend;

/// Formatiert einen kB-Wert menschenlesbar in MiB (ganzzahlig, reicht für
/// eine Chat-Antwort — keine Nachkommastellen-Präzision nötig).
fn kb_to_mib(kb: u64) -> u64 {
    kb / 1024
}

fn answer_gaming_mode(ctx: &SystemContext) -> String {
    let status = if ctx.gaming_mode_active {
        "an (Governor: performance)"
    } else {
        "aus (Governor: nicht performance)"
    };
    match &ctx.isolated_cpus {
        Some(isolated) => format!(
            "Gaming-Mode ist {status}. Isolierte CPUs (isolcpus): {isolated}."
        ),
        None => format!(
            "Gaming-Mode ist {status}. Keine isolierten CPUs aktiv (kein \
             isolcpus-Boot-Parameter, oder nicht verfügbar auf diesem System)."
        ),
    }
}

fn answer_memory(ctx: &SystemContext) -> String {
    match (ctx.mem_total_kb, ctx.mem_available_kb) {
        (Some(total), Some(available)) if total > 0 => {
            let used_percent = 100u64.saturating_sub(available.saturating_mul(100) / total);
            format!(
                "RAM: {} MiB von {} MiB belegt ({used_percent}% genutzt, {} MiB verfügbar). \
                 Details zu einzelnen Prozessen liefert Tarno AI in Phase 1 noch nicht — \
                 dafür `ps`/`top` auf dem System selbst nutzen.",
                kb_to_mib(total.saturating_sub(available)),
                kb_to_mib(total),
                kb_to_mib(available)
            )
        }
        _ => "RAM-Status aktuell nicht lesbar (/proc/meminfo nicht verfügbar).".to_string(),
    }
}

fn answer_security(ctx: &SystemContext) -> String {
    if ctx.ebpf_active {
        "Behavioral-Security (eBPF) ist aktiv — verdächtige Prozesse werden per \
         Tracepoint erkannt und mit SIGSTOP angehalten (siehe `tarnoctl resume <pid>`)."
            .to_string()
    } else {
        "Behavioral-Security (eBPF) ist in diesem Build NICHT aktiv (Cargo-Feature \
         `ebpf` nicht eingeschaltet)."
            .to_string()
    }
}

/// Tarno-AI-Phase-3 (siehe docs/month3-tarno-layer.md#tarno-ai): beantwortet
/// "was wurde zuletzt geblockt"/"what was blocked" bzw. "warum wurde X
/// angehalten"/"why was X stopped" anhand von `SystemContext::recent_
/// security_stops`/`last_stopped_*` (befüllt aus `security::events::
/// SecurityEventLog`, additiv zur Tracepoint/Policy-Engine, siehe
/// `security::ebpf_loader`). Ohne Events (Feature `ebpf` inaktiv, oder
/// bisher nichts passiert) gibt es eine ehrliche "nichts Auffälliges"-
/// Antwort statt einer erfundenen — beide Fälle sind aus `SystemContext`
/// heraus nicht unterscheidbar (0 Stops bleibt 0 Stops), das wird hier
/// bewusst nicht verschleiert.
fn answer_security_events(ctx: &SystemContext) -> String {
    if ctx.recent_security_stops == 0 {
        if ctx.ebpf_active {
            "Nichts Auffälliges bisher — im aktuellen Event-Fenster wurde kein Prozess von der \
             Behavioral-Security gestoppt."
                .to_string()
        } else {
            "Nichts Auffälliges bisher — Behavioral-Security (eBPF) ist in diesem Build nicht \
             aktiv, es gibt daher keine Events zum Nachschauen."
                .to_string()
        }
    } else {
        let comm = ctx.last_stopped_comm.as_deref().unwrap_or("unbekannt");
        let filename = ctx.last_stopped_filename.as_deref().unwrap_or("unbekannter Pfad");
        format!(
            "{} Prozess(e) wurden zuletzt von der Behavioral-Security gestoppt (SIGSTOP). \
             Zuletzt betroffen: \"{comm}\" ({filename}) — die konfigurierte Deny-Liste hat \
             gegriffen. Fortsetzen (nach Prüfung) mit `tarnoctl resume <pid>`.",
            ctx.recent_security_stops
        )
    }
}

fn fallback(question: &str) -> String {
    format!(
        "Das kann ich (Phase 1, heuristisch, kein LLM) noch nicht beantworten: \"{question}\". \
         Bekannte Fragen: Gaming-Mode-Status, RAM-Status, Security-Status, zuletzt geblockte \
         Prozesse."
    )
}

#[async_trait]
impl AiBackend for HeuristicBackend {
    async fn answer(&self, question: &str, ctx: &SystemContext) -> String {
        let q = question.to_lowercase();

        let asks_gaming = q.contains("gaming");
        let asks_ram = q.contains("ram") || q.contains("memory") || q.contains("speicher");
        // Spezifischer als asks_security (Phase 3): "was wurde geblockt"/
        // "warum wurde X angehalten" fragen nach konkreten Events, nicht
        // nach dem generellen Subsystem-Status.
        let asks_blocked = q.contains("geblockt")
            || q.contains("blockiert")
            || q.contains("blocked")
            || q.contains("angehalten")
            || q.contains("gestoppt")
            || q.contains("stopped");
        let asks_security = q.contains("security") || q.contains("sicherheit");

        if asks_gaming {
            answer_gaming_mode(ctx)
        } else if asks_ram {
            answer_memory(ctx)
        } else if asks_blocked {
            answer_security_events(ctx)
        } else if asks_security {
            answer_security(ctx)
        } else {
            fallback(question)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(gaming_mode_active: bool) -> SystemContext {
        SystemContext {
            gaming_mode_active,
            isolated_cpus: if gaming_mode_active {
                Some("2-3".to_string())
            } else {
                None
            },
            mem_total_kb: Some(1_000_000),
            mem_available_kb: Some(200_000),
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_gaming_mode_question_german() {
        let backend = HeuristicBackend;
        let answer = backend.answer("ist gaming mode an?", &ctx(true)).await;
        assert!(answer.contains("an"));
        assert!(answer.contains("2-3"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_gaming_mode_question_when_off() {
        let backend = HeuristicBackend;
        let answer = backend.answer("is gaming mode on", &ctx(false)).await;
        assert!(answer.contains("aus"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_ram_question() {
        let backend = HeuristicBackend;
        let answer = backend.answer("was frisst RAM?", &ctx(false)).await;
        assert!(answer.contains("MiB"));
        assert!(answer.contains("80%")); // 200_000/1_000_000 verfügbar -> 80% genutzt
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_memory_status_question_english() {
        let backend = HeuristicBackend;
        let answer = backend.answer("memory status", &ctx(false)).await;
        assert!(answer.contains("MiB"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_security_status_question() {
        let backend = HeuristicBackend;
        let mut c = ctx(false);
        c.ebpf_active = true;
        let answer = backend.answer("security status", &c).await;
        assert!(answer.contains("aktiv"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_question_returns_fallback() {
        let backend = HeuristicBackend;
        let answer = backend.answer("wie ist das Wetter?", &ctx(false)).await;
        assert!(answer.contains("noch nicht beantworten"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ram_status_without_meminfo_does_not_panic() {
        let backend = HeuristicBackend;
        let mut c = ctx(false);
        c.mem_total_kb = None;
        c.mem_available_kb = None;
        let answer = backend.answer("ram status", &c).await;
        assert!(answer.contains("nicht lesbar"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_what_was_blocked_honestly_when_no_events() {
        let backend = HeuristicBackend;
        let mut c = ctx(false);
        c.ebpf_active = true;
        let answer = backend.answer("was wurde zuletzt geblockt?", &c).await;
        assert!(answer.contains("Nichts Auffälliges bisher"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_what_was_blocked_without_ebpf_feature() {
        let backend = HeuristicBackend;
        let c = ctx(false); // ebpf_active: false (Default)
        let answer = backend.answer("what was blocked recently?", &c).await;
        assert!(answer.contains("Nichts Auffälliges bisher"));
        assert!(answer.contains("nicht aktiv"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_why_was_process_stopped_with_real_event_data() {
        let backend = HeuristicBackend;
        let mut c = ctx(false);
        c.ebpf_active = true;
        c.recent_security_stops = 1;
        c.last_stopped_comm = Some("cryptominer".to_string());
        c.last_stopped_filename = Some("/tmp/cryptominer".to_string());
        let answer = backend.answer("warum wurde cryptominer angehalten?", &c).await;
        assert!(answer.contains("cryptominer"));
        assert!(answer.contains("/tmp/cryptominer"));
        assert!(answer.contains("tarnoctl resume"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_why_was_process_stopped_english() {
        let backend = HeuristicBackend;
        let mut c = ctx(false);
        c.ebpf_active = true;
        c.recent_security_stops = 2;
        c.last_stopped_comm = Some("evil.sh".to_string());
        c.last_stopped_filename = Some("/tmp/evil.sh".to_string());
        let answer = backend.answer("why was evil.sh stopped?", &c).await;
        assert!(answer.contains("2 Prozess(e)"));
        assert!(answer.contains("evil.sh"));
    }
}
