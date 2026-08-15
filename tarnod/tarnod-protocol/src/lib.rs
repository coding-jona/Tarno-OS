//! JSON-Request/Response-Protokoll für den `tarnod`-IPC-Socket.
//!
//! Framing: newline-delimited JSON (siehe `tarnod/src/ipc.rs`). Geteilt
//! zwischen dem Daemon (`tarnod`), dem CLI-Client (`tarnoctl`, aktuell noch
//! mit eigenen JSON-String-Literalen) und der GUI (`tarnod-ui`), damit
//! Protokolländerungen nicht mehrfach nachgezogen werden müssen. Das
//! Protokoll ist bewusst klein gehalten, siehe
//! docs/month3-tarno-layer.md#ipc-design.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Erreichbarkeits-Check.
    Ping,
    /// Aktuellen Gaming-Mode-Status abfragen (u.a. isolierte CPUs).
    GetGamingMode,
    /// Gaming-Mode ein-/ausschalten (CPU-Governor umschalten).
    SetGamingMode { enabled: bool },
    /// API-Key aus dem In-Memory-Vault abrufen.
    GetApiKey { name: String },
    /// Status des Behavioral-Security-Subsystems (eBPF).
    SecurityStatus,
    /// Einen zuvor per SIGSTOP angehaltenen Prozess fortsetzen (SIGCONT).
    ResumeProcess { pid: i32 },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: serde_json::Value },
    Error { message: String },
}

impl Response {
    pub fn ok(data: serde_json::Value) -> Self {
        Response::Ok { data }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ping() {
        let req: Request = serde_json::from_str(r#"{"cmd":"ping"}"#).unwrap();
        assert!(matches!(req, Request::Ping));
    }

    #[test]
    fn parses_set_gaming_mode() {
        let req: Request =
            serde_json::from_str(r#"{"cmd":"set_gaming_mode","enabled":true}"#).unwrap();
        match req {
            Request::SetGamingMode { enabled } => assert!(enabled),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parses_get_api_key() {
        let req: Request = serde_json::from_str(r#"{"cmd":"get_api_key","name":"foo"}"#).unwrap();
        match req {
            Request::GetApiKey { name } => assert_eq!(name, "foo"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_cmd() {
        let result: Result<Request, _> = serde_json::from_str(r#"{"cmd":"nonsense"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn response_ok_serializes_with_status_field() {
        let resp = Response::ok(serde_json::json!({"a": 1}));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"status\":\"ok\""));
        assert!(s.contains("\"a\":1"));
    }

    #[test]
    fn response_err_serializes_with_message() {
        let resp = Response::err("boom");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"status\":\"error\""));
        assert!(s.contains("\"message\":\"boom\""));
    }

    #[test]
    fn request_round_trips_through_serialize_deserialize() {
        // Wichtig fuer tarnod-ui: die GUI serialisiert Request selbst
        // (statt wie tarnoctl rohe JSON-Strings zu bauen).
        let req = Request::ResumeProcess { pid: 1234 };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();
        match parsed {
            Request::ResumeProcess { pid } => assert_eq!(pid, 1234),
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
