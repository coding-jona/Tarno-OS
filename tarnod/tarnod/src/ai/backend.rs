//! `AiBackend`-Trait + `SystemContext`: der Vertrag zwischen Tarno AI und
//! dem echten System-Zustand. Bewusst so geschnitten, dass ein künftiges
//! Phase-2-LLM-Backend (siehe docs/month3-tarno-layer.md#tarno-ai) denselben
//! Trait implementieren kann, ohne `dispatch()` in main.rs anzufassen.

use async_trait::async_trait;

use crate::gaming::GamingController;

/// Live-Zustand des Systems, wie ihn Tarno AI für eine Antwort/einen
/// Vorschlag braucht. Alle Felder sind aus echten, bereits vorhandenen
/// Quellen befüllt (`GamingController`, `/proc/meminfo`, das `ebpf`-Cargo-
/// Feature) — nichts hier ist erfunden.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemContext {
    /// Governor auf "performance" gesetzt (siehe `gaming.rs::set_governor`).
    pub gaming_mode_active: bool,
    /// Inhalt von `/sys/devices/system/cpu/isolated`, falls vorhanden und
    /// nicht leer (`isolcpus=`-Boot-Parameter aktiv).
    pub isolated_cpus: Option<String>,
    /// Ob das `ebpf`-Cargo-Feature (Behavioral Security) in diesem Build
    /// aktiv ist — dieselbe Information wie `Request::SecurityStatus`.
    pub ebpf_active: bool,
    /// `MemTotal` aus `/proc/meminfo`, in kB.
    pub mem_total_kb: Option<u64>,
    /// `MemAvailable` aus `/proc/meminfo`, in kB.
    pub mem_available_kb: Option<u64>,
}

impl SystemContext {
    /// Sammelt den aktuellen System-Zustand. Reine Lesezugriffe (sysfs,
    /// procfs) — keine Schreibaktion, kann daher jederzeit gefahrlos vor
    /// jeder `AiQuery`/`AiStatus`-Antwort neu aufgerufen werden.
    pub fn gather(gaming: &GamingController) -> Self {
        let isolated_cpus = gaming
            .isolated_cpus()
            .ok()
            .filter(|s| !s.is_empty());
        let gaming_mode_active = gaming
            .current_governor()
            .map(|g| g == "performance")
            .unwrap_or(false);
        Self {
            gaming_mode_active,
            isolated_cpus,
            ebpf_active: cfg!(feature = "ebpf"),
            mem_total_kb: super::read_meminfo_kb("MemTotal:"),
            mem_available_kb: super::read_meminfo_kb("MemAvailable:"),
        }
    }
}

/// Ein Tarno-AI-Backend beantwortet eine Freitextfrage anhand des aktuellen
/// System-Kontexts. Phase 1 (`heuristic.rs`) mustererkennt bekannte Fragen,
/// Phase 2 (`mistral.rs`) befragt stattdessen die Mistral-AI-Cloud-API —
/// beide implementieren denselben Trait, `AiState`/`dispatch()` bleiben
/// unverändert.
///
/// `async fn` statt `fn` (Umstellung ggü. Phase 1): ein Netzwerk-Request
/// (Mistral) darf nicht blockierend im IPC-Handler laufen. `#[async_trait]`
/// statt eines nativen `async fn` im Trait, weil der Trait als
/// `Box<dyn AiBackend + Send + Sync>` dynamisch dispatcht wird (native
/// async-fn-in-trait ist dafür in dieser Edition noch nicht ergonomisch
/// nutzbar) — `HeuristicBackend` braucht dafür kein echtes `.await` intern,
/// `async_trait` macht daraus einfach einen sofort auflösenden Future.
#[async_trait]
pub trait AiBackend: Send + Sync {
    async fn answer(&self, question: &str, ctx: &SystemContext) -> String;
}
