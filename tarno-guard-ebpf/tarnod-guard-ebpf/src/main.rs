//! `tarnod-guard` — eBPF-Programm (Kernel-Space).
//!
//! Hängt sich an den Tracepoint `sched:sched_process_exec` und schickt für
//! jeden neu gestarteten Prozess ein `ExecEvent` (PID, UID, comm, Binärpfad)
//! über eine RingBuf-Map an den Userspace-Loader (`tarnod-guard`/`tarnod`).
//!
//! Bewusst KEIN `bpf_send_signal()` hier: eBPF kann nur Signale an den
//! *aktuell laufenden* Task senden, nicht an beliebige PIDs. Die
//! Policy-Entscheidung (Allow/Deny) und das SIGSTOP passieren im Userspace.
//! Begründung: docs/month3-tarno-layer.md#warum-sigstop-aus-userspace.

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::{bpf_get_current_comm, bpf_get_current_pid_tgid, bpf_get_current_uid_gid},
    macros::{map, tracepoint},
    maps::RingBuf,
    programs::TracePointContext,
};
use tarnod_guard_common::{ExecEvent, PATH_LEN};

/// Offset des `__data_loc char[] filename`-Felds im
/// `sched_process_exec`-Tracepoint-Format (siehe
/// /sys/kernel/tracing/events/sched/sched_process_exec/format).
const FILENAME_DATA_LOC_OFFSET: usize = 8;

#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

#[tracepoint]
pub fn tarnod_exec(ctx: TracePointContext) -> u32 {
    match try_tarnod_exec(&ctx) {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

fn try_tarnod_exec(ctx: &TracePointContext) -> Result<u32, i64> {
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = (pid_tgid >> 32) as u32;
    let uid_gid = bpf_get_current_uid_gid();
    let uid = (uid_gid & 0xFFFF_FFFF) as u32;
    let comm = bpf_get_current_comm().unwrap_or([0u8; 16]);

    // __data_loc-Kodierung: low 16 bit = Offset (relativ zum Tracepoint-
    // Record-Anfang), high 16 bit = Länge des Strings.
    let data_loc: u32 = unsafe { ctx.read_at(FILENAME_DATA_LOC_OFFSET).map_err(|_| 1i64)? };
    let str_offset = (data_loc & 0xFFFF) as usize;
    let str_len = ((data_loc >> 16) & 0xFFFF) as usize;
    let copy_len = if str_len < PATH_LEN { str_len } else { PATH_LEN };

    let mut entry = match EVENTS.reserve::<ExecEvent>(0) {
        Some(e) => e,
        None => return Err(1),
    };

    let ev_ptr = entry.as_mut_ptr();
    unsafe {
        (*ev_ptr).pid = pid;
        (*ev_ptr).uid = uid;
        (*ev_ptr).comm = comm;
        (*ev_ptr).filename = [0u8; PATH_LEN];
        (*ev_ptr).filename_len = copy_len as u16;

        // Bytes einzeln kopieren statt bpf_probe_read_str (die __data_loc-
        // Strings liegen bereits im Tracepoint-Puffer, kein separater
        // Pointer-Dereferenzierung nötig) — feste Obergrenze PATH_LEN,
        // verifier-freundlicher bounded loop (Kernel 5.3+).
        let mut i = 0usize;
        while i < copy_len && i < PATH_LEN {
            match ctx.read_at::<u8>(str_offset + i) {
                Ok(byte) => (*ev_ptr).filename[i] = byte,
                Err(_) => break,
            }
            i += 1;
        }
    }
    entry.submit(0);
    Ok(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 4] = *b"GPL\0";
