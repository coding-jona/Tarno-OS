//! Bindet `tarno-guard-ebpf` (siehe ../../../tarno-guard-ebpf/) in `tarnod`
//! ein: startet den Loader, wertet jedes `ExecEvent` gegen eine Policy
//! (Deny-Liste von Pfad-Substrings) aus und ruft bei einem Treffer
//! `process_ctl::stop`. Die Policy-Entscheidung + SIGSTOP passieren
//! bewusst hier (Userspace), nicht im eBPF-Programm — Begründung:
//! docs/month3-tarno-layer.md#warum-sigstop-aus-userspace.
//!
//! Tarno-AI-Phase-3 (siehe docs/month3-tarno-layer.md#tarno-ai): jedes
//! ausgewertete Event wird zusätzlich — rein lesend/protokollierend, ohne
//! Einfluss auf die Policy-Entscheidung oben — in `state.security_events`
//! (`security::events::SecurityEventLog`) gepusht, damit Tarno AI darüber
//! reden kann ("was wurde zuletzt geblockt").

use std::sync::Arc;

use tarnod_guard::Guard;

use super::events::{SecurityAction, SecurityEventRecord};
use crate::AppState;

/// Einfache Allow/Deny-Policy: ein Binärpfad gilt als verdächtig, wenn er
/// eine der konfigurierten Deny-Substrings enthält. Bewusst simpel gehalten
/// (kein Regex/Glob-Parser) — reicht für Phase 1 (Tracepoint-basiert,
/// reaktiv), siehe docs/month3-tarno-layer.md.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub deny_substrings: Vec<String>,
}

impl Policy {
    /// Liest die Deny-Liste aus `TARNOD_DENY_LIST` (kommagetrennt), z.B.
    /// `TARNOD_DENY_LIST=/tmp/,cryptominer`.
    pub fn from_env() -> Self {
        let raw = std::env::var("TARNOD_DENY_LIST").unwrap_or_default();
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Self {
        let deny_substrings = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self { deny_substrings }
    }

    pub fn is_denied(&self, filename: &str) -> bool {
        self.deny_substrings.iter().any(|pat| filename.contains(pat.as_str()))
    }
}

/// Lädt das eBPF-Programm, attached es und verarbeitet Events, bis ein
/// Fehler auftritt (z.B. Programm wurde extern detached). Läuft als
/// eigener Tokio-Task neben dem IPC-Server (siehe main.rs). `state` wird
/// nur für den additiven Tarno-AI-Phase-3-Event-Log gebraucht
/// (`state.security_events`) — die Policy-Entscheidung selbst hängt
/// weiterhin ausschließlich an `policy`.
pub async fn run(policy: Policy, state: Arc<AppState>) -> anyhow::Result<()> {
    let mut guard = Guard::load()?;
    eprintln!(
        "tarnod: eBPF-Behavioral-Security aktiv (deny-list: {:?})",
        policy.deny_substrings
    );

    let mut events = guard.events()?;
    loop {
        let ev = events.next().await?;
        let filename = ev.filename_str();
        let denied = policy.is_denied(filename);

        // Tarno-AI-Phase-3: additiv protokollieren, unabhängig davon, ob die
        // Policy den Prozess durchlässt oder stoppt — siehe Moduldoc oben.
        state.security_events.push(SecurityEventRecord::new(
            ev.pid,
            ev.uid,
            ev.comm_str(),
            filename,
            if denied {
                SecurityAction::Stopped
            } else {
                SecurityAction::Allowed
            },
        ));

        if denied {
            eprintln!(
                "tarnod: verdächtiger Prozess erkannt: pid={} uid={} comm={} filename={}",
                ev.pid,
                ev.uid,
                ev.comm_str(),
                filename
            );
            match crate::process_ctl::stop(ev.pid as i32) {
                Ok(()) => eprintln!(
                    "tarnod: pid={} angehalten (SIGSTOP). Fortsetzen mit: tarnoctl resume {}",
                    ev.pid, ev.pid
                ),
                Err(e) => eprintln!("tarnod: SIGSTOP fehlgeschlagen für pid={}: {e}", ev.pid),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_deny_list() {
        let policy = Policy::parse("/tmp/, cryptominer ,evil.sh");
        assert_eq!(
            policy.deny_substrings,
            vec!["/tmp/".to_string(), "cryptominer".to_string(), "evil.sh".to_string()]
        );
    }

    #[test]
    fn empty_env_yields_empty_policy() {
        let policy = Policy::parse("");
        assert!(policy.deny_substrings.is_empty());
        assert!(!policy.is_denied("/usr/bin/java"));
    }

    #[test]
    fn is_denied_matches_substring() {
        let policy = Policy::parse("cryptominer,/tmp/");
        assert!(policy.is_denied("/tmp/sneaky"));
        assert!(policy.is_denied("/usr/bin/cryptominer"));
        assert!(!policy.is_denied("/usr/bin/java"));
    }
}
