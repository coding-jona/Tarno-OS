//! Hintergrund-Thread, der `tarnod`-Status für die Taskleiste pollt.
//!
//! Bewusst read-only/poll-only (kein Request/Response-Protokoll wie in
//! `tarnod-ui`): die Taskleiste zeigt nur Status an, sie steuert `tarnod`
//! nicht — Steuerung bleibt Aufgabe von `tarnod-ui`/`tarnoctl`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tarnod_protocol::{Request, Response};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default)]
pub struct TarnodStatus {
    pub connected: bool,
    pub isolated_cpus: Option<String>,
    pub ebpf_active: Option<bool>,
}

pub type SharedStatus = Arc<Mutex<TarnodStatus>>;

/// Startet den Poll-Thread und gibt ein geteiltes Handle auf den zuletzt
/// bekannten Status zurück, das die Render-Schleife jeden Frame lesen kann
/// (kein Channel nötig, da uns nur der aktuelle Stand interessiert, keine
/// Event-Historie).
pub fn spawn() -> SharedStatus {
    let status: SharedStatus = Arc::new(Mutex::new(TarnodStatus::default()));
    let worker_status = Arc::clone(&status);
    thread::spawn(move || worker(&worker_status));
    status
}

fn socket_path() -> PathBuf {
    std::env::var("TARNOD_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/tarnod/tarnod.sock"))
}

fn worker(status: &SharedStatus) {
    let path = socket_path();
    loop {
        let result = poll_once(&path);
        let mut guard = status.lock().expect("tarnod status mutex poisoned");
        match result {
            Ok(s) => *guard = s,
            Err(_) => guard.connected = false,
        }
        drop(guard);
        thread::sleep(POLL_INTERVAL);
    }
}

fn poll_once(path: &PathBuf) -> std::io::Result<TarnodStatus> {
    let stream = UnixStream::connect(path)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let gaming = send_request(&mut writer, &mut reader, &Request::GetGamingMode)?;
    let security = send_request(&mut writer, &mut reader, &Request::SecurityStatus)?;

    let isolated_cpus = match gaming {
        Response::Ok { data } => data
            .get("isolated_cpus")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        Response::Error { .. } => None,
    };
    let ebpf_active = match security {
        Response::Ok { data } => data.get("ebpf_active").and_then(|v| v.as_bool()),
        Response::Error { .. } => None,
    };

    Ok(TarnodStatus {
        connected: true,
        isolated_cpus,
        ebpf_active,
    })
}

fn send_request(
    writer: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    req: &Request,
) -> std::io::Result<Response> {
    let line = serde_json::to_string(req)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    let mut resp_line = String::new();
    let n = reader.read_line(&mut resp_line)?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "tarnod hat die Verbindung geschlossen",
        ));
    }
    serde_json::from_str(&resp_line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
