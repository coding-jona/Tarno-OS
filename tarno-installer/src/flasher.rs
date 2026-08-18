//! Kopier-Engine: schreibt ein Image blockweise auf ein Zielgerät (oder,
//! für Tests, eine gewöhnliche Datei) und meldet Fortschritt über einen
//! Channel. Bewusst in reinem Rust implementiert (kein `dd`-Shell-Aufruf)
//! für volle Kontrolle über Fortschritt/Abbruch/Fehlerbehandlung.

#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

/// 4 MiB, wie bei `dd bs=4M` — großer genug für guten Durchsatz auf
/// rotierenden wie Flash-Medien, klein genug für flüssige Fortschrittsupdates.
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum FlashEvent {
    Progress { written: u64, total: u64, bytes_per_sec: f64 },
    Done { written: u64, elapsed_secs: f64 },
    Cancelled,
    Error(String),
}

/// Wird von der UI gesetzt, um einen laufenden Flash-Vorgang abzubrechen
/// (z.B. "Abbrechen"-Button).
pub type CancelFlag = Arc<AtomicBool>;

pub fn new_cancel_flag() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}

/// Kopiert `source` nach `dest` in `CHUNK_SIZE`-Blöcken, meldet nach jedem
/// Block Fortschritt über `events`. `dest` wird komplett überschrieben —
/// beim Aufrufer liegt die Verantwortung, vorher die Bestätigung des
/// Nutzers eingeholt zu haben (siehe `app.rs`).
pub fn flash(source: &Path, dest: &Path, events: &Sender<FlashEvent>, cancel: &CancelFlag) {
    if let Err(e) = flash_inner(source, dest, events, cancel) {
        let _ = events.send(FlashEvent::Error(e));
    }
}

fn flash_inner(
    source: &Path,
    dest: &Path,
    events: &Sender<FlashEvent>,
    cancel: &CancelFlag,
) -> Result<(), String> {
    let mut src = File::open(source)
        .map_err(|e| format!("Quelle {} konnte nicht geöffnet werden: {e}", source.display()))?;
    let total = src.metadata().map_err(|e| e.to_string())?.len();

    let mut out = open_dest(dest)?;

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut written: u64 = 0;
    let start = Instant::now();
    let mut last_report = Instant::now();

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = events.send(FlashEvent::Cancelled);
            return Ok(());
        }
        let n = src.read(&mut buf).map_err(|e| format!("Lesefehler: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("Schreibfehler auf {}: {e}", dest.display()))?;
        written += n as u64;

        if last_report.elapsed().as_millis() >= 150 || written == total {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let _ = events.send(FlashEvent::Progress {
                written,
                total,
                bytes_per_sec: written as f64 / elapsed,
            });
            last_report = Instant::now();
        }
    }

    out.sync_all().map_err(|e| format!("Sync fehlgeschlagen: {e}"))?;
    let elapsed = start.elapsed().as_secs_f64();
    let _ = events.send(FlashEvent::Done { written, elapsed_secs: elapsed });
    Ok(())
}

#[cfg(unix)]
fn open_dest(dest: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        // O_SYNC: Schreibzugriffe warten auf physisches Commit statt nur im
        // Page-Cache zu landen - langsamer, aber sicherer für einen
        // Imaging-Vorgang auf einen Wechseldatenträger, den man direkt
        // danach abzieht.
        .custom_flags(libc::O_SYNC)
        .open(dest)
        .map_err(|e| {
            format!(
                "Ziel {} konnte nicht zum Schreiben geöffnet werden: {e}",
                dest.display()
            )
        })
}

/// Windows: kein `O_SYNC`-Äquivalent über `OpenOptions` — stattdessen
/// öffnet `win32::open_dest_handle` das physische Laufwerk direkt über
/// `CreateFileW` und sperrt/dismountet vorher die zugehörigen
/// Laufwerksbuchstaben (siehe `win32.rs`). `sync_all()` in `flash_inner`
/// entspricht unter Windows `FlushFileBuffers`, das `std::fs::File`
/// automatisch für uns aufruft.
#[cfg(windows)]
fn open_dest(dest: &Path) -> Result<File, String> {
    crate::win32::open_dest_handle(dest)
}

#[cfg(not(any(unix, windows)))]
fn open_dest(dest: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .open(dest)
        .map_err(|e| format!("Ziel {} konnte nicht geöffnet werden: {e}", dest.display()))
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tarno-installer-flasher-test-{}-{}-{label}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// Deterministischer Pseudo-Zufallsinhalt (kein Extra-Dependency auf
    /// eine rand-Crate nötig) - reicht, um "wurde wirklich byteweise
    /// kopiert" von "zufällig durch Nullen o.ä. bestanden" zu unterscheiden.
    fn fake_random_bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn flash_copies_content_byte_for_byte() {
        let source_path = temp_path("src.img");
        let dest_path = temp_path("dst.img");

        let content = fake_random_bytes(10 * 1024 * 1024 + 137, 42); // > 2 Chunks, nicht Chunk-aligned
        std::fs::write(&source_path, &content).unwrap();
        std::fs::write(&dest_path, vec![0u8; content.len()]).unwrap(); // Ziel muss existieren (kein create())

        let (tx, rx) = channel();
        let cancel = new_cancel_flag();
        flash(&source_path, &dest_path, &tx, &cancel);

        let events: Vec<FlashEvent> = rx.try_iter().collect();
        assert!(
            events.iter().any(|e| matches!(e, FlashEvent::Progress { .. })),
            "erwartet mindestens ein Progress-Event, bekommen: {events:?}"
        );
        let done = events.iter().find_map(|e| match e {
            FlashEvent::Done { written, .. } => Some(*written),
            _ => None,
        });
        assert_eq!(done, Some(content.len() as u64));

        let written_content = std::fs::read(&dest_path).unwrap();
        assert_eq!(written_content, content, "Zielinhalt weicht vom Quellinhalt ab");

        let _ = std::fs::remove_file(&source_path);
        let _ = std::fs::remove_file(&dest_path);
    }

    #[test]
    fn flash_reports_error_on_missing_source() {
        let source_path = temp_path("missing-src.img");
        let dest_path = temp_path("dst-for-missing-src.img");
        std::fs::write(&dest_path, vec![0u8; 16]).unwrap();

        let (tx, rx) = channel();
        let cancel = new_cancel_flag();
        flash(&source_path, &dest_path, &tx, &cancel);

        let events: Vec<FlashEvent> = rx.try_iter().collect();
        assert!(matches!(events.as_slice(), [FlashEvent::Error(_)]));

        let _ = std::fs::remove_file(&dest_path);
    }

    #[test]
    fn flash_stops_immediately_when_already_cancelled() {
        let source_path = temp_path("src-for-cancel.img");
        let dest_path = temp_path("dst-for-cancel.img");
        let content = fake_random_bytes(1024 * 1024, 7);
        std::fs::write(&source_path, &content).unwrap();
        std::fs::write(&dest_path, vec![0u8; content.len()]).unwrap();

        let (tx, rx) = channel();
        let cancel = new_cancel_flag();
        cancel.store(true, Ordering::Relaxed);
        flash(&source_path, &dest_path, &tx, &cancel);

        let events: Vec<FlashEvent> = rx.try_iter().collect();
        assert!(matches!(events.as_slice(), [FlashEvent::Cancelled]));

        let _ = std::fs::remove_file(&source_path);
        let _ = std::fs::remove_file(&dest_path);
    }

    #[test]
    fn format_bytes_uses_appropriate_unit() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(16 * 1024 * 1024 * 1024), "16.0 GiB");
    }
}
