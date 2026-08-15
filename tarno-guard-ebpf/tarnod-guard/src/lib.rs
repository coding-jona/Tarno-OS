//! Userspace-Loader-Bibliothek für `tarnod-guard-ebpf`.
//!
//! Lädt das (vorkompilierte, eingebettete) eBPF-Objekt, hängt es an den
//! `sched_process_exec`-Tracepoint und liefert einen einfachen async-Stream
//! über die empfangenen `ExecEvent`s. Policy-Entscheidungen (Allow/Deny,
//! SIGSTOP) sind bewusst NICHT hier, sondern im aufrufenden Daemon
//! (`tarnod`, Feature `ebpf`) — siehe docs/month3-tarno-layer.md.

use anyhow::{Context, Result};
use aya::maps::{MapData, RingBuf};
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Ebpf};
use tarnod_guard_common::ExecEvent;
use tokio::io::unix::AsyncFd;

/// Vorkompiliertes eBPF-Objekt (siehe `assets/README.md` für
/// Regenerierungs-Anleitung, `build.sh` für das Build-Skript). Wird
/// eingebettet statt zur Build-Zeit aus `tarnod-guard-ebpf` erzeugt, damit
/// weder `tarnod` noch ein Buildroot-Cross-Build eine Host-BPF-Toolchain
/// (nightly Rust + bpf-linker) brauchen — Begründung:
/// docs/architecture.md#buildroot-integration-tarno-br2-external.
static EBPF_OBJECT: &[u8] = include_bytes_aligned!("../assets/tarnod-guard.bpf.o");

/// Hebt `RLIMIT_MEMLOCK` an (auf älteren Kerneln ohne memcg-basiertes
/// BPF-Accounting nötig, um Maps anlegen zu können).
fn bump_memlock_rlimit() {
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    let ret = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };
    if ret != 0 {
        log::warn!(
            "setrlimit(RLIMIT_MEMLOCK) fehlgeschlagen (auf Kerneln mit memcg-BPF-Accounting \
             i.d.R. unkritisch)"
        );
    }
}

pub struct Guard {
    ebpf: Ebpf,
}

impl Guard {
    /// Lädt und attached das eBPF-Programm. Braucht `CAP_BPF` + `CAP_PERFMON`
    /// (oder root) sowie einen Kernel mit BTF (`/sys/kernel/btf/vmlinux`).
    pub fn load() -> Result<Self> {
        bump_memlock_rlimit();

        let mut ebpf = Ebpf::load(EBPF_OBJECT).context("eBPF-Objekt konnte nicht geladen werden")?;
        if let Err(e) = aya_log::EbpfLogger::init(&mut ebpf) {
            log::warn!("aya-log-Initialisierung fehlgeschlagen (nicht fatal): {e}");
        }

        let program: &mut TracePoint = ebpf
            .program_mut("tarnod_exec")
            .context("Programm 'tarnod_exec' nicht im eBPF-Objekt gefunden")?
            .try_into()
            .context("Programm ist kein TracePoint")?;
        program.load().context("Laden des tarnod_exec-Programms fehlgeschlagen")?;
        program
            .attach("sched", "sched_process_exec")
            .context("Attach an sched:sched_process_exec fehlgeschlagen")?;

        Ok(Self { ebpf })
    }

    /// Gibt einen Stream über die empfangenen `ExecEvent`s zurück. Darf nur
    /// einmal aufgerufen werden (nimmt die RingBuf-Map aus dem eBPF-Objekt).
    pub fn events(&mut self) -> Result<EventStream> {
        let map = self.ebpf.take_map("EVENTS").context("EVENTS-Map nicht gefunden")?;
        let ring = RingBuf::try_from(map)?;
        let fd = AsyncFd::new(ring).context("AsyncFd für RingBuf fehlgeschlagen")?;
        Ok(EventStream {
            fd,
            pending: std::collections::VecDeque::new(),
        })
    }
}

pub struct EventStream {
    fd: AsyncFd<RingBuf<MapData>>,
    /// Bereits aus dem Kernel gelesene, aber noch nicht an den Aufrufer
    /// ausgelieferte Events. Wichtig für Korrektheit: `AsyncFd`/`epoll`
    /// arbeitet edge-triggered — wenn wir bei einer Readiness-Benachrichtigung
    /// mehrere Events im RingBuf vorfinden, aber nur eins zurückgeben und den
    /// Rest verwerfen, kommt ohne neue Kernel-seitige Aktivität u.U. nie
    /// wieder eine Readiness-Benachrichtigung für die übrigen Events — sie
    /// würden sonst stillschweigend verloren gehen.
    pending: std::collections::VecDeque<ExecEvent>,
}

impl EventStream {
    /// Wartet auf das nächste Event. Bricht ExecEvents mit ungültiger
    /// Bytelänge (sollte bei korrektem Producer nie vorkommen) still ab.
    pub async fn next(&mut self) -> Result<ExecEvent> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(ev);
            }

            let mut guard = self.fd.readable_mut().await?;
            let ring = guard.get_inner_mut();
            while let Some(item) = ring.next() {
                if item.len() >= core::mem::size_of::<ExecEvent>() {
                    let ev =
                        unsafe { core::ptr::read_unaligned(item.as_ptr() as *const ExecEvent) };
                    self.pending.push_back(ev);
                }
            }
            // Alle aktuell verfügbaren Items wurden vollständig aus dem Ring
            // gelesen (nicht nur das erste) — jetzt ist clear_ready() korrekt.
            guard.clear_ready();
        }
    }
}
