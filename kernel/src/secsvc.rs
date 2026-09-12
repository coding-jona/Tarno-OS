// SPDX-License-Identifier: GPL-2.0-or-later
//! The Security Service — a real, isolated *userspace* process, not a
//! kernel module. See `docs/thos/roadmap.md`'s security-architecture
//! section: "the scanner never runs in the kernel" is the whole point of
//! this split — a bug in a hash-signature check must not be able to panic
//! ring 0. This is the first slice: real process isolation, a real
//! kernel↔service channel, and one real decision (`check_hash`) actually
//! moved out of the kernel — not the eventual YARA / heuristics / update
//! mechanism, which stays this process's job to grow into.
//!
//! **Channel**: two ordinary pipes (`crate::file::pipe`, the same
//! primitive `pipe()`/`pipe2()` hand out to user processes) — a
//! request pipe (kernel writes, the service reads) and a response pipe
//! (the service writes, the kernel reads). The kernel holds its two ends
//! directly (no fd/task indirection); the service process's fd 0/1 are
//! wired to its two ends by [`crate::process::spawn_with_fds`] instead of
//! the usual console-backed stdio, so nothing it reads or writes is
//! visible to (or forgeable by) any other process.
//!
//! **Protocol**: one request = 32 bytes (a SHA-256 hash); one response =
//! 1 byte (`0` = allow, nonzero = quarantine). Deliberately the smallest
//! real thing that lets the verdict actually live outside the kernel.
//!
//! **Crash safety** ("a crash there degrades to a policy default, it does
//! not take the kernel down" — the roadmap's own requirement): needs no
//! new mechanism at all. `process::set_exit_status` already clears a
//! task's fd table the instant it exits, for any reason (a clean exit or
//! a fault) — so the moment the service process is gone, its end of the
//! response pipe closes, and the kernel's blocking `read` on its own end
//! returns a real `0` (EOF) instead of hanging. [`check_hash`] treats
//! that (or any negative result) as "unavailable" and the caller
//! (`execgate::check`) falls back to its own local, always-available
//! list — the actual policy default.

use alloc::sync::Arc;
use alloc::vec;

use crate::file::{FileOps, PipeReadEnd, PipeWriteEnd};

struct Channel {
    /// The kernel's end of the request pipe (kernel → service).
    req_write: Arc<PipeWriteEnd>,
    /// The kernel's end of the response pipe (service → kernel).
    resp_read: Arc<PipeReadEnd>,
}

static CHANNEL: spin::Mutex<Option<Channel>> = spin::Mutex::new(None);

/// Spawn `/secsvc` off `fs` with its stdio wired to a fresh pair of pipes
/// instead of the console, and record the kernel's own ends. `false` if
/// there's no `/secsvc` on disk, or it fails to load — [`check_hash`]
/// degrades gracefully either way (there's simply no channel to use).
pub fn spawn(fs: &crate::ext2::Ext2) -> bool {
    let Some(bytes) = fs.read_path("/secsvc") else {
        crate::kprintln!("THOS: secsvc           no /secsvc on disk — exec-gate stays local-only");
        return false;
    };
    let (req_read, req_write) = crate::file::pipe();
    let (resp_read, resp_write) = crate::file::pipe();
    let stderr: Arc<dyn crate::file::FileOps> = Arc::new(crate::file::ConsoleFile { writable: true });
    let fds = vec![
        Some(crate::process::FdEntry {
            obj: crate::process::HandleObject::File(req_read),
            cloexec: false,
        }),
        Some(crate::process::FdEntry {
            obj: crate::process::HandleObject::File(resp_write),
            cloexec: false,
        }),
        // stderr -> the console, not the response pipe: a panic message from
        // the service is visible in the boot log for real diagnosis, not
        // silently mixed into (or missing from) the verdict protocol.
        Some(crate::process::FdEntry { obj: crate::process::HandleObject::File(stderr), cloexec: false }),
    ];
    match crate::process::spawn_with_fds(0, &bytes, &["/secsvc"], &[], 0, 0, fds) {
        Ok(pid) => {
            *CHANNEL.lock() = Some(Channel { req_write, resp_read });
            crate::kprintln!("THOS: secsvc ok        pid {pid}, isolated userspace, {} bytes", bytes.len());
            true
        }
        Err(e) => {
            crate::kprintln!("THOS: secsvc FAIL      {e}");
            false
        }
    }
}

/// Ask the Security Service whether `hash` is known-bad. `Some(true)` /
/// `Some(false)` is a real verdict from the isolated process; `None` means
/// unavailable (never spawned, or its end of the channel is gone — see
/// this module's own doc comment for why that's real `EOF`, not a guess)
/// and the caller must fall back to its own policy default.
pub fn check_hash(hash: &[u8; 32]) -> Option<bool> {
    let guard = CHANNEL.lock();
    let ch = guard.as_ref()?;
    if ch.req_write.write(hash) != 32 {
        return None; // EPIPE (or a short write — treat either as gone)
    }
    let mut verdict = [0u8; 1];
    if ch.resp_read.read(&mut verdict) != 1 {
        return None; // EOF — the service is gone
    }
    Some(verdict[0] != 0)
}
