//! `tarnoctl` — dünner CLI-Client für `tarnod`.
//!
//! Bewusst synchron/std-only (kein tokio): ein CLI-Aufruf schickt genau eine
//! Anfrage und wartet auf genau eine Antwort, dafür lohnt sich keine
//! async-Runtime. Siehe docs/architecture.md#tarnoctl-cli-client.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn socket_path() -> PathBuf {
    std::env::var("TARNOD_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/tarnod/tarnod.sock"))
}

fn usage() -> ! {
    eprintln!(
        "usage: tarnoctl <ping|gaming-mode <on|off|status>|api-key <name>|security-status|resume <pid>>"
    );
    std::process::exit(2);
}

fn build_request(args: &[String]) -> String {
    match args.first().map(|s| s.as_str()) {
        Some("ping") => r#"{"cmd":"ping"}"#.to_string(),
        Some("gaming-mode") => match args.get(1).map(|s| s.as_str()) {
            Some("on") => r#"{"cmd":"set_gaming_mode","enabled":true}"#.to_string(),
            Some("off") => r#"{"cmd":"set_gaming_mode","enabled":false}"#.to_string(),
            Some("status") | None => r#"{"cmd":"get_gaming_mode"}"#.to_string(),
            _ => usage(),
        },
        Some("api-key") => {
            let name = args.get(1).unwrap_or_else(|| usage());
            format!(r#"{{"cmd":"get_api_key","name":"{name}"}}"#)
        }
        Some("security-status") => r#"{"cmd":"security_status"}"#.to_string(),
        Some("resume") => {
            let pid = args.get(1).unwrap_or_else(|| usage());
            format!(r#"{{"cmd":"resume_process","pid":{pid}}}"#)
        }
        _ => usage(),
    }
}

fn send(request: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.write_all(request.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim_end().to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let request = build_request(&args);

    match send(&request) {
        Ok(resp) => println!("{resp}"),
        Err(e) => {
            eprintln!("tarnoctl: fehler beim Verbinden mit {:?}: {e}", socket_path());
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ping_request() {
        assert_eq!(build_request(&["ping".into()]), r#"{"cmd":"ping"}"#);
    }

    #[test]
    fn builds_gaming_mode_on_request() {
        assert_eq!(
            build_request(&["gaming-mode".into(), "on".into()]),
            r#"{"cmd":"set_gaming_mode","enabled":true}"#
        );
    }

    #[test]
    fn builds_gaming_mode_status_request_by_default() {
        assert_eq!(
            build_request(&["gaming-mode".into()]),
            r#"{"cmd":"get_gaming_mode"}"#
        );
    }

    #[test]
    fn builds_api_key_request() {
        assert_eq!(
            build_request(&["api-key".into(), "MOJANG".into()]),
            r#"{"cmd":"get_api_key","name":"MOJANG"}"#
        );
    }

    #[test]
    fn builds_resume_request() {
        assert_eq!(
            build_request(&["resume".into(), "1234".into()]),
            r#"{"cmd":"resume_process","pid":1234}"#
        );
    }
}
