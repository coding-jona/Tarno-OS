//! Phase-3-Baustein für Tarno AI (siehe docs/month3-tarno-layer.md#tarno-ai):
//! ein beschränkter Ringpuffer der jüngsten `ExecEvent`s, den
//! `security::ebpf_loader` (Feature `ebpf`) befüllt und den
//! `ai::backend::SystemContext` nur lesend konsumiert.
//!
//! Bewusst **unabhängig** vom `ebpf`-Feature kompiliert (kein `#[cfg]` auf
//! Modulebene): ohne das Feature bleibt der Log schlicht immer leer (nichts
//! pusht jemals hinein), statt dass `ai::backend`/`main.rs` per `#[cfg]`
//! zwei verschiedene `SystemContext`-Formen bräuchten. Additiv zur
//! bestehenden Tracepoint/Policy-Engine — dieser Log trifft/ersetzt keine
//! Policy-/SIGSTOP-Entscheidung, er protokolliert sie nur nachträglich.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Ob die Policy-Engine (`ebpf_loader::Policy`) den Prozess durchgelassen
/// oder per SIGSTOP angehalten hat. Rein informativ — die tatsächliche
/// Entscheidung fällt weiterhin ausschließlich in `ebpf_loader::run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityAction {
    Allowed,
    Stopped,
}

/// Ein einzelner, bereits abgeschlossener Security-Event (Kopie der
/// relevanten `ExecEvent`-Felder als eigene `String`s statt der rohen
/// `[u8; N]`-Kernel-Buffer, da dieser Log unabhängig vom `ebpf`-Feature/
/// `tarnod-guard-common` kompilierbar bleiben soll).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityEventRecord {
    pub pid: u32,
    pub uid: u32,
    pub comm: String,
    pub filename: String,
    pub action: SecurityAction,
    /// Unix-Timestamp (Sekunden) des Zeitpunkts, an dem `tarnod` das Event
    /// verarbeitet hat (nicht der exakte Kernel-Tracepoint-Zeitpunkt — für
    /// eine Chat-Antwort reicht diese Auflösung).
    pub timestamp_secs: u64,
}

impl SecurityEventRecord {
    pub fn new(
        pid: u32,
        uid: u32,
        comm: impl Into<String>,
        filename: impl Into<String>,
        action: SecurityAction,
    ) -> Self {
        Self {
            pid,
            uid,
            comm: comm.into(),
            filename: filename.into(),
            action,
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

/// Maximale Anzahl gespeicherter Events — derselbe Deckel/dieselbe FIFO-
/// Kappung wie bei `AiState::suggestions` (`ai/mod.rs`, `MAX_SUGGESTIONS`).
/// Bewusst kein größerer Wert: dieser Log ist für "was ist zuletzt passiert"
/// gedacht, kein vollständiges Audit-Log (dafür gäbe es andere Werkzeuge,
/// z.B. Journald/Kernel-Tracing direkt).
const MAX_EVENTS: usize = 50;

/// Beschränkter FIFO-Log, analog zu `AiState::suggestions` in `ai/mod.rs` —
/// `Mutex<Vec<_>>` statt z.B. `RefCell`, weil der Log sowohl vom
/// eBPF-RingBuf-Poller-Task (`ebpf_loader::run`) als auch von IPC-Handlern
/// (`AiQuery`/`AiStatus` via `SystemContext::gather`) aus verschiedenen
/// `tokio`-Tasks heraus gleichzeitig erreichbar ist.
#[derive(Debug, Default)]
pub struct SecurityEventLog {
    events: Mutex<Vec<SecurityEventRecord>>,
}

impl SecurityEventLog {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Fügt ein Event hinzu, kappt auf `MAX_EVENTS` (älteste zuerst raus) —
    /// exakt dasselbe Muster wie `AiState::push_suggestion`.
    pub fn push(&self, record: SecurityEventRecord) {
        let mut events = self.events.lock().unwrap();
        events.push(record);
        if events.len() > MAX_EVENTS {
            let excess = events.len() - MAX_EVENTS;
            events.drain(0..excess);
        }
    }

    /// Kopie aller aktuell gespeicherten Events, älteste zuerst.
    pub fn recent(&self) -> Vec<SecurityEventRecord> {
        self.events.lock().unwrap().clone()
    }

    /// Anzahl der Events mit `SecurityAction::Stopped` im aktuellen Ring.
    pub fn stop_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.action == SecurityAction::Stopped)
            .count()
    }

    /// Der jüngste Event mit `SecurityAction::Stopped`, falls einer im
    /// aktuellen Ring vorhanden ist (kann durch neuere `Allowed`-Events
    /// verdrängt worden sein, siehe `MAX_EVENTS`-Kappung oben).
    pub fn last_stopped(&self) -> Option<SecurityEventRecord> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e.action == SecurityAction::Stopped)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(comm: &str, action: SecurityAction) -> SecurityEventRecord {
        SecurityEventRecord::new(1234, 1000, comm, format!("/usr/bin/{comm}"), action)
    }

    #[test]
    fn starts_empty() {
        let log = SecurityEventLog::new();
        assert!(log.recent().is_empty());
        assert_eq!(log.stop_count(), 0);
        assert!(log.last_stopped().is_none());
    }

    #[test]
    fn push_caps_at_max_events() {
        let log = SecurityEventLog::new();
        for i in 0..(MAX_EVENTS + 10) {
            log.push(record(&format!("proc{i}"), SecurityAction::Allowed));
        }
        let events = log.recent();
        assert_eq!(events.len(), MAX_EVENTS);
        // älteste Einträge müssen raus sein, neuester muss drin sein.
        assert_eq!(events.last().unwrap().comm, format!("proc{}", MAX_EVENTS + 9));
    }

    #[test]
    fn stop_count_only_counts_stopped_events() {
        let log = SecurityEventLog::new();
        log.push(record("java", SecurityAction::Allowed));
        log.push(record("cryptominer", SecurityAction::Stopped));
        log.push(record("bash", SecurityAction::Allowed));
        log.push(record("evil.sh", SecurityAction::Stopped));
        assert_eq!(log.stop_count(), 2);
    }

    #[test]
    fn last_stopped_returns_most_recent_stop_not_most_recent_event() {
        let log = SecurityEventLog::new();
        log.push(record("cryptominer", SecurityAction::Stopped));
        log.push(record("bash", SecurityAction::Allowed));
        log.push(record("java", SecurityAction::Allowed));
        let last = log.last_stopped().expect("ein Stop sollte vorhanden sein");
        assert_eq!(last.comm, "cryptominer");
    }

    #[test]
    fn last_stopped_can_be_evicted_by_fifo_cap() {
        let log = SecurityEventLog::new();
        log.push(record("cryptominer", SecurityAction::Stopped));
        for i in 0..MAX_EVENTS {
            log.push(record(&format!("noise{i}"), SecurityAction::Allowed));
        }
        // Der Stop war der allererste Eintrag und wurde durch MAX_EVENTS
        // neuere Allowed-Events verdrängt — ehrliches Verhalten des
        // FIFO-Deckels, kein Bug.
        assert!(log.last_stopped().is_none());
    }
}
