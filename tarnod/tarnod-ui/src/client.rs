//! Hintergrund-Thread, der die IPC-Verbindung zu `tarnod` hält.
//!
//! Läuft synchron in einem eigenen OS-Thread (die GUI selbst ist
//! immediate-mode/nicht-async) — Requests kommen über einen Channel rein,
//! Antworten/Verbindungsereignisse gehen über einen zweiten Channel raus.
//! Die egui-App pollt den Event-Channel in `update()`, siehe `app.rs`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Duration;

use tarnod_protocol::{Request, Response};

const RECONNECT_DELAY: Duration = Duration::from_millis(1500);

pub enum ClientEvent {
    Connected,
    Disconnected(String),
    Response(Response),
}

pub struct Client {
    tx_request: Sender<Request>,
    rx_event: Receiver<ClientEvent>,
}

impl Client {
    pub fn spawn(socket_path: PathBuf) -> Self {
        let (tx_request, rx_request) = channel::<Request>();
        let (tx_event, rx_event) = channel::<ClientEvent>();

        thread::spawn(move || worker_loop(socket_path, &rx_request, &tx_event));

        Self {
            tx_request,
            rx_event,
        }
    }

    /// Schickt eine Anfrage an den Worker-Thread. Nicht-blockierend; die
    /// Antwort kommt asynchron über `poll_events`.
    pub fn send(&self, req: Request) {
        // Fehler ignorieren: passiert nur, wenn der Worker-Thread beendet
        // ist (z.B. beim App-Shutdown) — dann gibt es ohnehin nichts mehr
        // zu tun.
        let _ = self.tx_request.send(req);
    }

    /// Liefert alle seit dem letzten Aufruf eingetroffenen Events, ohne zu
    /// blockieren. Wird von `TarnodApp::update` jeden Frame aufgerufen.
    pub fn poll_events(&self) -> Vec<ClientEvent> {
        self.rx_event.try_iter().collect()
    }
}

fn worker_loop(socket_path: PathBuf, rx_request: &Receiver<Request>, tx_event: &Sender<ClientEvent>) {
    loop {
        match UnixStream::connect(&socket_path) {
            Ok(stream) => {
                let _ = tx_event.send(ClientEvent::Connected);
                if let Err(e) = connection_loop(&stream, rx_request, tx_event) {
                    let _ = tx_event.send(ClientEvent::Disconnected(e));
                }
            }
            Err(e) => {
                let _ = tx_event.send(ClientEvent::Disconnected(format!(
                    "verbindung zu {} fehlgeschlagen: {e}",
                    socket_path.display()
                )));
            }
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

/// Eine Anfrage rein, eine Antwort raus, wiederholen — solange die
/// Verbindung hält. Requests kommen strikt in der Reihenfolge zurück, in
/// der sie gesendet wurden (kein Pipelining), das nutzt `app.rs` aus, um
/// Antworten ohne Request-IDs den richtigen UI-Feldern zuzuordnen.
fn connection_loop(
    stream: &UnixStream,
    rx_request: &Receiver<Request>,
    tx_event: &Sender<ClientEvent>,
) -> Result<(), String> {
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    loop {
        let req = match rx_request.recv() {
            Ok(r) => r,
            // Sender-Ende weg (App wird beendet) -> Thread sauber verlassen.
            Err(_) => std::process::exit(0),
        };
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .map_err(|e| format!("schreiben fehlgeschlagen: {e}"))?;

        let mut resp_line = String::new();
        let n = reader
            .read_line(&mut resp_line)
            .map_err(|e| format!("lesen fehlgeschlagen: {e}"))?;
        if n == 0 {
            return Err("verbindung vom Daemon geschlossen".to_string());
        }
        let resp: Response = serde_json::from_str(&resp_line)
            .map_err(|e| format!("ungültige Antwort: {e}"))?;
        let _ = tx_event.send(ClientEvent::Response(resp));
    }
}
