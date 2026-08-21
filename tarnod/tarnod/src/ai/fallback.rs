//! `FallbackBackend`: versucht ein primäres Backend (in der Praxis:
//! `MistralBackend`) und fällt bei jedem Fehlschlag (Netzwerkfehler, alle
//! Retries ausgeschöpft, HTTP-Fehler nach Retries) transparent auf ein
//! sekundäres Backend (`HeuristicBackend`) zurück — der Nutzer bekommt nie
//! eine rohe Fehlermeldung, sondern immer eine Antwort. Siehe
//! docs/month3-tarno-layer.md#tarno-ai (vereinfachte Zwei-Stufen-Version
//! des `FallbackProvider`-Konzepts aus der Python-Referenz
//! `coding-jona/tarno`).
//!
//! Bewusst kein generischer `Vec<Box<dyn AiBackend>>`-Chain wie im Python-
//! Original — für Phase 2 reicht exakt eine Fallback-Stufe
//! (Mistral -> Heuristik), das hält den Code einfach und deckt den
//! tatsächlichen Bedarf ("die M6700-Zielhardware hat nicht garantiert immer
//! Internet") vollständig ab.

use async_trait::async_trait;

use super::backend::{AiBackend, SystemContext};
use super::mistral::MistralBackend;

pub struct FallbackBackend {
    primary: MistralBackend,
    secondary: Box<dyn AiBackend + Send + Sync>,
}

impl FallbackBackend {
    pub fn new(primary: MistralBackend, secondary: Box<dyn AiBackend + Send + Sync>) -> Self {
        Self { primary, secondary }
    }
}

#[async_trait]
impl AiBackend for FallbackBackend {
    async fn answer(&self, question: &str, ctx: &SystemContext) -> String {
        match self.primary.try_answer(question, ctx).await {
            Ok(answer) => answer,
            Err(_) => self.secondary.answer(question, ctx).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::heuristic::HeuristicBackend;
    use std::time::Duration;

    fn ctx() -> SystemContext {
        SystemContext {
            gaming_mode_active: false,
            isolated_cpus: None,
            ebpf_active: false,
            mem_total_kb: Some(1_000_000),
            mem_available_kb: Some(200_000),
        }
    }

    /// Baut ein `MistralBackend`, das garantiert fehlschlägt (unroutbarer
    /// Endpoint, kein echtes Netz zu api.mistral.ai nötig) — dasselbe Muster
    /// wie in `mistral.rs`'s eigenen Tests.
    fn failing_mistral() -> MistralBackend {
        MistralBackend::new_for_test(
            "test-key".to_string(),
            "http://127.0.0.1:1/chat/completions".to_string(),
            Duration::from_millis(500),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn falls_through_to_heuristic_when_mistral_fails() {
        let fallback = FallbackBackend::new(failing_mistral(), Box::new(HeuristicBackend));
        let answer = fallback.answer("ist gaming mode an?", &ctx()).await;
        // Die Heuristik-Antwort für die Gaming-Mode-Frage enthält "Gaming-
        // Mode ist ..." und NICHT die Mistral-Fehlermeldung — das beweist,
        // dass tatsächlich durchgefallen wurde, nicht nur, dass irgendeine
        // Antwort kam.
        assert!(answer.contains("Gaming-Mode ist"));
        assert!(!answer.contains("Mistral ist gerade nicht erreichbar"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn never_panics_even_when_primary_fails() {
        let fallback = FallbackBackend::new(failing_mistral(), Box::new(HeuristicBackend));
        let _ = fallback.answer("irgendeine Frage", &ctx()).await;
        // Kein Panic bis hierhin ist der eigentliche Test.
    }
}
