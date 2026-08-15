//! Gemeinsame Event-Struktur zwischen dem eBPF-Programm (Kernel-Space,
//! `tarnod-guard-ebpf`) und dem Userspace-Loader (`tarnod-guard`).
//!
//! `#[repr(C)]` + feste Feldgrößen, damit das Layout auf beiden Seiten der
//! RingBuf-Grenze identisch ist. Siehe docs/month3-tarno-layer.md.

// `no_std` nur außerhalb von Tests: der `cargo test`-Harness selbst braucht
// std, die Tests am Dateiende prüfen aber ausschließlich die no_std-taugliche
// Logik oben.
#![cfg_attr(not(test), no_std)]

/// Maximale Länge von `comm` (Kernel-Task-Name, siehe `task_struct.comm`).
pub const COMM_LEN: usize = 16;
/// Maximale Anzahl an Bytes des Binärpfads, die wir aus dem
/// `sched_process_exec`-Tracepoint kopieren (längere Pfade werden
/// abgeschnitten, `filename_len` markiert die tatsächliche Länge).
pub const PATH_LEN: usize = 128;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExecEvent {
    pub pid: u32,
    pub uid: u32,
    pub comm: [u8; COMM_LEN],
    pub filename: [u8; PATH_LEN],
    pub filename_len: u16,
}

impl ExecEvent {
    pub const fn zeroed() -> Self {
        Self {
            pid: 0,
            uid: 0,
            comm: [0; COMM_LEN],
            filename: [0; PATH_LEN],
            filename_len: 0,
        }
    }

    /// Liefert `comm` als `&str` bis zum ersten NUL-Byte (oder ganz, falls
    /// keins vorhanden ist). Nutzt `from_utf8` defensiv, da Kernel-Strings
    /// nicht garantiert gültiges UTF-8 sind.
    pub fn comm_str(&self) -> &str {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(self.comm.len());
        core::str::from_utf8(&self.comm[..end]).unwrap_or("<invalid-utf8>")
    }

    /// Liefert den (ggf. abgeschnittenen) Binärpfad als `&str`.
    ///
    /// `filename_len` stammt aus der `__data_loc`-Kodierung des
    /// `sched_process_exec`-Tracepoints und zählt das terminierende NUL-Byte
    /// mit — wird hier abgeschnitten, damit `filename_str()` einen sauberen
    /// Pfad ohne `\0`-Suffix liefert.
    pub fn filename_str(&self) -> &str {
        let mut len = (self.filename_len as usize).min(PATH_LEN);
        if len > 0 && self.filename[len - 1] == 0 {
            len -= 1;
        }
        core::str::from_utf8(&self.filename[..len]).unwrap_or("<invalid-utf8>")
    }
}

// Safety: ExecEvent ist #[repr(C)], enthält nur Fixed-Size-Integer/Byte-Arrays
// ohne Padding-Bytes mit Bedeutung, und hat kein Drop — erfüllt damit die
// Anforderungen von `aya::Pod` (plain-old-data, sicher aus RingBuf-Bytes
// re-interpretierbar).
#[cfg(feature = "user")]
unsafe impl aya::Pod for ExecEvent {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comm_str_stops_at_nul() {
        let mut ev = ExecEvent::zeroed();
        ev.comm[..4].copy_from_slice(b"java");
        assert_eq!(ev.comm_str(), "java");
    }

    #[test]
    fn filename_str_respects_filename_len() {
        let mut ev = ExecEvent::zeroed();
        let path = b"/usr/bin/java";
        ev.filename[..path.len()].copy_from_slice(path);
        ev.filename_len = path.len() as u16;
        assert_eq!(ev.filename_str(), "/usr/bin/java");
    }

    #[test]
    fn zeroed_event_has_empty_strings_where_expected() {
        let ev = ExecEvent::zeroed();
        assert_eq!(ev.comm_str(), "");
        assert_eq!(ev.filename_str(), "");
    }
}
