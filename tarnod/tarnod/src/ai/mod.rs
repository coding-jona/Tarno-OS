//! Tarno AI — Phase 1: heuristische Assistenz + proaktives Tuning, kein
//! LLM (siehe docs/month3-tarno-layer.md#tarno-ai für Phase-2/3-Ausblick).
//! Geschwister-Modul von `gaming.rs`, `security/`, `vault.rs`.

pub mod backend;
pub mod fallback;
pub mod heuristic;
pub mod mistral;
pub mod tuning;

use std::sync::Mutex;

use backend::AiBackend;
use fallback::FallbackBackend;
use heuristic::HeuristicBackend;
use mistral::MistralBackend;

/// Name des Vault-Eintrags, unter dem der Mistral-API-Key erwartet wird —
/// dieselbe generische `KEY=VALUE`-Vault wie für `MOJANG_API_KEY` (siehe
/// `vault.rs`). Siehe auch
/// docs/month3-tarno-layer.md#mistral-api-key-einrichten.
pub const MISTRAL_API_KEY_NAME: &str = "MISTRAL_API_KEY";

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
    /// Ob beim Start ein `MISTRAL_API_KEY` in der Vault gefunden wurde
    /// (Phase-2-Modus aktiv) oder nicht (Phase-1-Heuristik-Modus). Für
    /// `Request::AiStatus`/`tarnoctl ai status`, siehe `main.rs`.
    mistral_configured: bool,
}

impl AiState {
    /// Baut `AiState` ohne Vault-Zugriff — immer Phase-1-Heuristik-Modus.
    /// Bleibt für bestehende Aufrufer/Tests erhalten, die keinen `Vault`
    /// zur Hand haben; der reale Startpfad (`main.rs`) nutzt `from_vault`.
    pub fn new() -> Self {
        Self {
            backend: Box::new(HeuristicBackend),
            suggestions: Mutex::new(Vec::new()),
            mistral_configured: false,
        }
    }

    /// Baut `AiState` und wählt das Backend anhand der Vault: ist
    /// `MISTRAL_API_KEY` gesetzt und nicht leer, wird
    /// `FallbackBackend(MistralBackend -> HeuristicBackend)` aktiv (Phase 2
    /// mit Netzwerk-/API-Fallback), sonst bleibt es bei reiner
    /// `HeuristicBackend` (Phase 1) — kein Absturz, kein Fehler, falls kein
    /// Key konfiguriert ist. Siehe
    /// docs/month3-tarno-layer.md#mistral-api-key-einrichten.
    pub fn from_vault(vault: &crate::vault::Vault) -> Self {
        match vault.get(MISTRAL_API_KEY_NAME).filter(|k| !k.is_empty()) {
            Some(key) => Self {
                backend: Box::new(FallbackBackend::new(
                    MistralBackend::new(key.to_string()),
                    Box::new(HeuristicBackend),
                )),
                suggestions: Mutex::new(Vec::new()),
                mistral_configured: true,
            },
            None => Self {
                backend: Box::new(HeuristicBackend),
                suggestions: Mutex::new(Vec::new()),
                mistral_configured: false,
            },
        }
    }

    pub async fn answer(&self, question: &str, ctx: &backend::SystemContext) -> String {
        self.backend.answer(question, ctx).await
    }

    /// Ob Tarno AI im Phase-2-Modus (Mistral konfiguriert) oder im
    /// Phase-1-Heuristik-Modus läuft — für `Request::AiStatus`.
    pub fn mistral_configured(&self) -> bool {
        self.mistral_configured
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
