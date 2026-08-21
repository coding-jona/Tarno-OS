//! Tarno AI — Phase 1: heuristische Assistenz + proaktives Tuning, kein
//! LLM (siehe docs/month3-tarno-layer.md#tarno-ai für Phase-2/3-Ausblick).
//! Geschwister-Modul von `gaming.rs`, `security/`, `vault.rs`.

pub mod backend;
pub mod heuristic;
pub mod tuning;

use std::sync::Mutex;

use backend::AiBackend;
use heuristic::HeuristicBackend;

/// Maximale Anzahl gespeicherter Tuning-Vorschläge — der Daemon läuft
/// dauerhaft, die Queue darf nicht unbegrenzt wachsen. Älteste Einträge
/// fallen zuerst raus (FIFO).
const MAX_SUGGESTIONS: usize = 50;

/// Zustand von Tarno AI: das aktive Backend (Phase 1: `HeuristicBackend`,
/// künftig austauschbar gegen ein Phase-2-LLM-Backend, siehe `backend.rs`)
/// sowie die Queue proaktiver Tuning-Vorschläge, die `tuning::run` befüllt.
///
/// `Box<dyn AiBackend + Send + Sync>` und `Mutex` statt z.B. `RefCell`,
/// weil `AiState` über `AppState` in einem `Arc` liegt und aus mehreren
/// `tokio::spawn`-Tasks gleichzeitig erreichbar ist (IPC-Client-Handler in
/// `ipc.rs`, Tuning-Task in `tuning.rs`) — dasselbe Muster wie `AppState`
/// selbst bereits über die vorhandenen Felder hinweg voraussetzt.
pub struct AiState {
    backend: Box<dyn AiBackend + Send + Sync>,
    suggestions: Mutex<Vec<String>>,
}

impl AiState {
    pub fn new() -> Self {
        Self {
            backend: Box::new(HeuristicBackend),
            suggestions: Mutex::new(Vec::new()),
        }
    }

    pub fn answer(&self, question: &str, ctx: &backend::SystemContext) -> String {
        self.backend.answer(question, ctx)
    }

    /// Fügt einen Vorschlag hinzu, kappt die Queue auf `MAX_SUGGESTIONS`
    /// (älteste zuerst raus).
    pub fn push_suggestion(&self, suggestion: String) {
        let mut queue = self.suggestions.lock().unwrap();
        queue.push(suggestion);
        if queue.len() > MAX_SUGGESTIONS {
            let excess = queue.len() - MAX_SUGGESTIONS;
            queue.drain(0..excess);
        }
    }

    /// Kopie der aktuell gesammelten Vorschläge (für `Request::AiSuggestions`).
    pub fn suggestions(&self) -> Vec<String> {
        self.suggestions.lock().unwrap().clone()
    }
}

impl Default for AiState {
    fn default() -> Self {
        Self::new()
    }
}

/// Liest ein einzelnes Feld aus `/proc/meminfo` (z.B. `"MemTotal:"`) und
/// gibt dessen Wert in kB zurück. Bewusst ohne neue Abhängigkeit umgesetzt
/// (kein `sysinfo`-Crate) — `/proc/meminfo` ist ein stabiles Kernel-ABI,
/// ein simpler Zeilen-Parser reicht.
pub(crate) fn read_meminfo_kb(field: &str) -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let value = rest.trim().split_whitespace().next()?;
            return value.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_suggestion_caps_queue_length() {
        let state = AiState::new();
        for i in 0..(MAX_SUGGESTIONS + 10) {
            state.push_suggestion(format!("suggestion {i}"));
        }
        let queue = state.suggestions();
        assert_eq!(queue.len(), MAX_SUGGESTIONS);
        // älteste Einträge müssen raus sein, neueste müssen drin sein
        assert_eq!(queue.last().unwrap(), &format!("suggestion {}", MAX_SUGGESTIONS + 9));
    }

    #[test]
    fn suggestions_start_empty() {
        let state = AiState::new();
        assert!(state.suggestions().is_empty());
    }

    #[test]
    fn read_meminfo_kb_parses_real_proc_meminfo() {
        // /proc/meminfo existiert auf jedem Linux-Testrunner (auch Sandboxen).
        let total = read_meminfo_kb("MemTotal:");
        assert!(total.is_some());
        assert!(total.unwrap() > 0);
    }

    #[test]
    fn read_meminfo_kb_returns_none_for_unknown_field() {
        assert!(read_meminfo_kb("DefinitelyNotAField:").is_none());
    }
}
