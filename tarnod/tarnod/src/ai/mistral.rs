//! Phase-2-`AiBackend`: befragt die Mistral-AI-Cloud-API
//! (`https://api.mistral.ai/v1/chat/completions`) statt eines lokalen
//! Modells. Details/Herleitung siehe
//! docs/knowledge-base/05-mistral-ai-api-integration.md und
//! docs/month3-tarno-layer.md#tarno-ai.
//!
//! Wichtig: `AiBackend::answer` (der Trait aus `backend.rs`) gibt einen
//! reinen `String` zurück, kein `Result` — Phase 1 (`HeuristicBackend`)
//! kann per Definition nicht fehlschlagen. Damit `FallbackBackend`
//! (`fallback.rs`) trotzdem erkennen kann, *ob* ein Mistral-Aufruf
//! fehlgeschlagen ist (Netzwerkfehler, alle Retries ausgeschöpft, HTTP-
//! Fehler nach Retries) und dann auf `HeuristicBackend` umschalten kann,
//! bietet `MistralBackend` zusätzlich eine inhärente Methode
//! `try_answer(..) -> Result<String, MistralError>`. Die Trait-`answer`-
//! Implementierung ruft `try_answer` auf und wandelt einen `Err` in eine
//! ehrliche deutsche Fehlermeldung um (für den Fall, dass `MistralBackend`
//! irgendwo *ohne* `FallbackBackend` direkt verwendet wird) — der normale
//! Betriebspfad läuft aber über `FallbackBackend`, das `try_answer` direkt
//! aufruft und bei `Err` auf `HeuristicBackend` zurückfällt.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::backend::{AiBackend, SystemContext};

/// Modell-Standardwahl für Tarno AIs Anwendungsfall (kurze System-Status-
/// Fragen, kein langes Dokument) — siehe Recherche, "Small 3.1 ist die
/// naheliegende Standardwahl".
const DEFAULT_MODEL: &str = "mistral-small-latest";
/// Kurze Antworten reichen für einen System-Status-Assistenten — bewusst
/// klein gehalten, kein Long-Form-Chat.
const DEFAULT_MAX_TOKENS: u32 = 512;
const DEFAULT_TEMPERATURE: f32 = 0.7;
const BASE_URL: &str = "https://api.mistral.ai/v1/chat/completions";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Fixe Default-Rate: 0.8 Requests/Sekunde, angelehnt an den von Mistral
/// selbst für das Standardmodell veröffentlichten Durchsatz. Bewusst KEIN
/// Port der vollständigen Per-Modell-RPS-Tabelle aus der Python-Referenz
/// (`coding-jona/tarno`, `mistral_client.py`) — das wäre für diesen ersten
/// Rust-Cut Over-Engineering; ein einzelner fester Default reicht, solange
/// Tarno AI nur ein Modell verwendet. Siehe auch
/// docs/knowledge-base/05-mistral-ai-api-integration.md.
const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(1250); // 1s / 0.8

/// Maximale Anzahl Versuche bei HTTP 429 (Rate-Limit) bzw. bei 5xx/Netzwerk-
/// fehlern. 429 bekommt mehr Versuche, weil ein `Retry-After` meist ein
/// baldiges Erfolgsfenster ankündigt; 5xx/Netzwerkfehler sind seltener
/// selbstheilend, daher weniger Versuche, bevor auf Heuristik
/// zurückgefallen wird.
const MAX_RETRIES_RATE_LIMIT: u32 = 4;
const MAX_RETRIES_SERVER_ERROR: u32 = 2;
/// Obergrenze für eine per `Retry-After`-Header angeforderte Wartezeit —
/// verhindert, dass ein extremer/kaputter Header-Wert die Anfrage
/// minutenlang blockiert.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Basiswert für exponentielles Backoff, wenn kein `Retry-After`-Header
/// vorliegt (bzw. bei 5xx-Fehlern).
const BACKOFF_BASE: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum MistralError {
    /// Netzwerkfehler (Verbindung fehlgeschlagen, Timeout, DNS, ...).
    Network(String),
    /// HTTP-Antwort mit Fehlerstatus, nach Ausschöpfen aller Retries.
    Http { status: u16, body: String },
    /// Antwort war 2xx, aber das erwartete JSON-Schema fehlt/ist leer.
    UnexpectedResponse(String),
}

impl std::fmt::Display for MistralError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MistralError::Network(e) => write!(f, "netzwerkfehler: {e}"),
            MistralError::Http { status, body } => {
                write!(f, "http-fehler {status}: {body}")
            }
            MistralError::UnexpectedResponse(e) => write!(f, "unerwartete antwort: {e}"),
        }
    }
}

impl std::error::Error for MistralError {}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponseChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatResponseChoice>,
}

/// Baut den System-Prompt aus `SystemContext`. Bewusst knapp gehalten (kein
/// ausführliches Prompt-Engineering) — nur so viel Live-Kontext, dass die
/// Antwort geerdet ist, ohne bei jeder Anfrage unnötig viele Tokens zu
/// verbrauchen (siehe offene Frage dazu in der Recherche-Doku).
///
/// Seit Tarno-AI-Phase-3 (siehe docs/month3-tarno-layer.md#tarno-ai) enthält
/// der Kontext zusätzlich eine knappe Security-Event-Zusammenfassung, damit
/// eine Frage wie "was wurde zuletzt geblockt" real geerdet beantwortet wird
/// statt zu halluzinieren — Mistral bekommt nur diese Zusammenfassung, nicht
/// den vollen Event-Ring (der bleibt intern in `SecurityEventLog`).
fn system_prompt(ctx: &SystemContext) -> String {
    let gaming = if ctx.gaming_mode_active { "an" } else { "aus" };
    let isolated = ctx.isolated_cpus.as_deref().unwrap_or("keine");
    let ebpf = if ctx.ebpf_active { "aktiv" } else { "inaktiv" };
    let mem = match (ctx.mem_total_kb, ctx.mem_available_kb) {
        (Some(total), Some(avail)) if total > 0 => {
            let used_percent = 100u64.saturating_sub(avail.saturating_mul(100) / total);
            format!("{used_percent}% belegt")
        }
        _ => "unbekannt".to_string(),
    };
    let security_events = if ctx.recent_security_stops == 0 {
        "keine kürzlich gestoppten Prozesse".to_string()
    } else {
        format!(
            "{} kürzlich gestoppte(r) Prozess(e), zuletzt: {} ({})",
            ctx.recent_security_stops,
            ctx.last_stopped_comm.as_deref().unwrap_or("unbekannt"),
            ctx.last_stopped_filename.as_deref().unwrap_or("unbekannter Pfad"),
        )
    };
    format!(
        "Du bist Tarno AI, der eingebaute System-Assistent von Tarno OS \
         (ein Gaming-optimiertes Linux für Minecraft auf Debian-Basis). \
         Antworte kurz, konkret und auf Deutsch, außer die Frage ist auf \
         Englisch. Aktueller System-Kontext: Gaming-Mode ist {gaming}, \
         isolierte CPUs: {isolated}, Behavioral-Security (eBPF): {ebpf}, \
         RAM: {mem}, Security-Events: {security_events}."
    )
}

fn build_request(model: &str, question: &str, ctx: &SystemContext, max_tokens: u32, temperature: f32) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "system",
                content: system_prompt(ctx),
            },
            ChatMessage {
                role: "user",
                content: question.to_string(),
            },
        ],
        max_tokens,
        temperature,
        stream: false,
    }
}

/// Reine Backoff-Berechnung, ohne Timer/Netzwerk — getrennt testbar.
/// `retry_after` gewinnt, falls vorhanden (auf `MAX_RETRY_AFTER` gedeckelt);
/// sonst exponentielles Backoff ab `BACKOFF_BASE` (`base * 2^attempt`).
fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(ra) = retry_after {
        return ra.min(MAX_RETRY_AFTER);
    }
    let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    (BACKOFF_BASE * factor).min(MAX_RETRY_AFTER)
}

/// Parst den `Retry-After`-Header (Sekunden als Ganzzahl, wie von Mistral/
/// den meisten REST-APIs gesendet).
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

pub struct MistralBackend {
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    min_interval: Duration,
    last_call: Mutex<Option<Instant>>,
    client: reqwest::Client,
    /// Chat-Completions-Endpoint. In Produktion immer `BASE_URL`; Tests
    /// überschreiben dieses Feld auf eine lokale/unroutbare Adresse, damit
    /// der Netzwerkfehler-/Retry-Pfad ohne echten Zugriff auf
    /// api.mistral.ai geprüft werden kann (siehe `tests`-Modul unten).
    base_url: String,
}

impl MistralBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: DEFAULT_TEMPERATURE,
            min_interval: DEFAULT_MIN_INTERVAL,
            last_call: Mutex::new(None),
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("reqwest::Client::builder() mit statischer Konfiguration darf nicht fehlschlagen"),
            base_url: BASE_URL.to_string(),
        }
    }

    /// Test-Konstruktor: erlaubt einen überschriebenen `base_url` und
    /// `timeout`, damit Tests (hier und in `fallback.rs`) den Netzwerk-
    /// fehler-/Retry-Pfad gegen eine lokale/unroutbare Adresse durchlaufen
    /// können, ohne echten Zugriff auf `api.mistral.ai`. `pub(crate)`, kein
    /// öffentlicher API-Oberfläche-Zuwachs.
    #[cfg(test)]
    pub(crate) fn new_for_test(api_key: String, base_url: String, timeout: Duration) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_string(),
            max_tokens: 16,
            temperature: 0.0,
            min_interval: Duration::from_millis(0),
            last_call: Mutex::new(None),
            client: reqwest::Client::builder().timeout(timeout).build().unwrap(),
            base_url,
        }
    }

    /// Wartet ggf. den Rest von `min_interval` seit dem letzten Aufruf ab
    /// (einfacher Token-Bucket-der-Größe-1 / Minimum-Intervall-Limiter).
    async fn throttle(&self) {
        let wait = {
            let mut last = self.last_call.lock().unwrap();
            let now = Instant::now();
            let wait = match *last {
                Some(prev) => {
                    let elapsed = now.duration_since(prev);
                    if elapsed < self.min_interval {
                        Some(self.min_interval - elapsed)
                    } else {
                        None
                    }
                }
                None => None,
            };
            *last = Some(now);
            wait
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
    }

    /// Ein einzelner Chat-Completion-Aufruf, inklusive Retry/Backoff bei
    /// 429 (Rate-Limit) und 5xx (Server-/Netzwerkfehler). Gibt bei
    /// Erschöpfen der Retries einen `MistralError` zurück, statt zu
    /// panicken oder den Daemon abstürzen zu lassen — der Aufrufer
    /// (`FallbackBackend` bzw. die Trait-`answer`-Impl unten) entscheidet,
    /// was mit dem Fehler passiert.
    pub async fn try_answer(&self, question: &str, ctx: &SystemContext) -> Result<String, MistralError> {
        let payload = build_request(&self.model, question, ctx, self.max_tokens, self.temperature);

        let mut rate_limit_attempts = 0u32;
        let mut server_error_attempts = 0u32;

        loop {
            self.throttle().await;

            let response = self
                .client
                .post(&self.base_url)
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await;

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    if server_error_attempts < MAX_RETRIES_SERVER_ERROR {
                        server_error_attempts += 1;
                        tokio::time::sleep(backoff_delay(server_error_attempts, None)).await;
                        continue;
                    }
                    return Err(MistralError::Network(e.to_string()));
                }
            };

            let status = response.status();

            if status.as_u16() == 429 {
                if rate_limit_attempts < MAX_RETRIES_RATE_LIMIT {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_retry_after);
                    rate_limit_attempts += 1;
                    tokio::time::sleep(backoff_delay(rate_limit_attempts, retry_after)).await;
                    continue;
                }
                let body = response.text().await.unwrap_or_default();
                return Err(MistralError::Http { status: 429, body });
            }

            if status.is_server_error() {
                if server_error_attempts < MAX_RETRIES_SERVER_ERROR {
                    server_error_attempts += 1;
                    tokio::time::sleep(backoff_delay(server_error_attempts, None)).await;
                    continue;
                }
                let body = response.text().await.unwrap_or_default();
                return Err(MistralError::Http {
                    status: status.as_u16(),
                    body,
                });
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MistralError::Http {
                    status: status.as_u16(),
                    body,
                });
            }

            let parsed: ChatResponse = response
                .json()
                .await
                .map_err(|e| MistralError::UnexpectedResponse(e.to_string()))?;

            return parse_chat_response(parsed);
        }
    }
}

fn parse_chat_response(resp: ChatResponse) -> Result<String, MistralError> {
    resp.choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| MistralError::UnexpectedResponse("keine choices[0].message.content".to_string()))
}

#[async_trait]
impl AiBackend for MistralBackend {
    async fn answer(&self, question: &str, ctx: &SystemContext) -> String {
        match self.try_answer(question, ctx).await {
            Ok(answer) => answer,
            Err(e) => format!("Mistral ist gerade nicht erreichbar ({e})."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SystemContext {
        SystemContext {
            gaming_mode_active: true,
            isolated_cpus: Some("2-3".to_string()),
            ebpf_active: true,
            mem_total_kb: Some(1_000_000),
            mem_available_kb: Some(200_000),
            ..Default::default()
        }
    }

    #[test]
    fn build_request_has_expected_shape() {
        let req = build_request("mistral-small-latest", "ist gaming mode an?", &ctx(), 512, 0.7);
        assert_eq!(req.model, "mistral-small-latest");
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.messages[1].content, "ist gaming mode an?");
        assert!(!req.stream);
        assert_eq!(req.max_tokens, 512);
    }

    #[test]
    fn build_request_serializes_to_openai_compatible_json() {
        let req = build_request("mistral-small-latest", "frage", &ctx(), 512, 0.7);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"model\":\"mistral-small-latest\""));
        assert!(json.contains("\"messages\""));
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"max_tokens\":512"));
        assert!(json.contains("\"stream\":false"));
    }

    #[test]
    fn system_prompt_embeds_live_context() {
        let prompt = system_prompt(&ctx());
        assert!(prompt.contains("Tarno AI"));
        assert!(prompt.contains("an")); // gaming mode an
        assert!(prompt.contains("2-3")); // isolierte CPUs
        assert!(prompt.contains("aktiv")); // ebpf
        assert!(prompt.contains("keine kürzlich gestoppten Prozesse")); // Phase 3, kein Stop in ctx()
    }

    #[test]
    fn system_prompt_embeds_recent_security_stop() {
        let mut c = ctx();
        c.recent_security_stops = 2;
        c.last_stopped_comm = Some("cryptominer".to_string());
        c.last_stopped_filename = Some("/tmp/cryptominer".to_string());
        let prompt = system_prompt(&c);
        assert!(prompt.contains("2 kürzlich gestoppte(r) Prozess(e)"));
        assert!(prompt.contains("cryptominer"));
        assert!(prompt.contains("/tmp/cryptominer"));
    }

    #[test]
    fn parse_chat_response_extracts_message_content() {
        let json = r#"{
            "id": "abc",
            "object": "chat.completion",
            "model": "mistral-small-latest",
            "choices": [{"message": {"role": "assistant", "content": "Gaming-Mode ist an."}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;
        let parsed: ChatResponse = serde_json::from_str(json).unwrap();
        let answer = parse_chat_response(parsed).unwrap();
        assert_eq!(answer, "Gaming-Mode ist an.");
    }

    #[test]
    fn parse_chat_response_errors_on_empty_choices() {
        let json = r#"{"id": "abc", "object": "chat.completion", "model": "m", "choices": []}"#;
        let parsed: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(parse_chat_response(parsed).is_err());
    }

    #[test]
    fn parse_chat_response_errors_on_empty_content() {
        let json = r#"{"id": "abc", "object": "chat.completion", "model": "m",
            "choices": [{"message": {"role": "assistant", "content": ""}}]}"#;
        let parsed: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(parse_chat_response(parsed).is_err());
    }

    #[test]
    fn backoff_delay_uses_retry_after_when_present() {
        let d = backoff_delay(0, Some(Duration::from_secs(3)));
        assert_eq!(d, Duration::from_secs(3));
    }

    #[test]
    fn backoff_delay_caps_retry_after_at_max() {
        let d = backoff_delay(0, Some(Duration::from_secs(600)));
        assert_eq!(d, MAX_RETRY_AFTER);
    }

    #[test]
    fn backoff_delay_grows_exponentially_without_retry_after() {
        let d0 = backoff_delay(0, None);
        let d1 = backoff_delay(1, None);
        let d2 = backoff_delay(2, None);
        assert_eq!(d0, BACKOFF_BASE);
        assert_eq!(d1, BACKOFF_BASE * 2);
        assert_eq!(d2, BACKOFF_BASE * 4);
        assert!(d1 > d0 && d2 > d1);
    }

    #[test]
    fn backoff_delay_caps_exponential_growth_at_max() {
        let d = backoff_delay(20, None);
        assert_eq!(d, MAX_RETRY_AFTER);
    }

    #[test]
    fn parse_retry_after_parses_seconds() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
    }

    #[test]
    fn parse_retry_after_returns_none_for_garbage() {
        assert_eq!(parse_retry_after("not-a-number"), None);
    }

    /// Baut ein `MistralBackend`, das NIE `api.mistral.ai` kontaktiert:
    /// `base_url` zeigt auf `127.0.0.1:1` (Port 1 ist praktisch garantiert
    /// nicht offen -> sofortiges `ConnectionRefused`, kein DNS/Internet
    /// nötig, kein Timeout-Warten). Damit lässt sich der Netzwerkfehler-/
    /// Retry-Pfad real durchlaufen, ohne echten Zugriff auf die Mistral-API
    /// (in dieser Sandbox ohnehin kein Key/kein Netz zu externen Hosts
    /// vorhanden) und ohne die Tests durch echte Retry-Wartezeiten
    /// auszubremsen.
    fn unreachable_backend() -> MistralBackend {
        MistralBackend::new_for_test(
            "test-key".to_string(),
            "http://127.0.0.1:1/chat/completions".to_string(),
            Duration::from_millis(500),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn try_answer_returns_network_error_for_unreachable_host() {
        let result = unreachable_backend().try_answer("test", &ctx()).await;
        // Verbindungsfehler (kein Netz/kein offener Port) muss als
        // sauberer `Err` zurückkommen, niemals als Panic.
        assert!(matches!(result, Err(MistralError::Network(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answer_never_panics_and_returns_german_fallback_text_on_failure() {
        let answer = unreachable_backend().answer("test", &ctx()).await;
        assert!(answer.contains("Mistral ist gerade nicht erreichbar"));
    }
}
