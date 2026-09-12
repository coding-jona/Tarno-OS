// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: a minimal Security Service. Reads a 32-byte SHA-256 hash from
// fd 0 (wired to the kernel<->service request pipe, not the console),
// writes back one verdict byte (1 = quarantine, 0 = allow) on fd 1, in a
// loop -- the real protocol `secsvc.rs` (kernel side) speaks.
//
// Quarantine store: every quarantine decision is appended to
// `/etc/thos/quarantine.log` on ext2, via ordinary POSIX file I/O -- the
// service is a real process with its own filesystem access, independent
// of the IPC pipes entirely. No RTC yet (see `syscall.rs`'s own `SYS_TIME`
// stub), so entries are keyed by a boot-relative sequence number instead
// of a wall-clock timestamp -- a real, stated limitation, not a silently
// wrong one.
//
// A magic all-0xFF "hash" (impossible to arise from a real SHA-256 of
// anything) is a test-only poison pill: exit immediately, simulating a
// crash, so the kernel side's crash-degrade fallback gets exercised for
// real instead of assumed. The quarantine log already written survives
// that exit untouched -- it's on disk, not in this process's memory.
use std::io::{Read, Seek, SeekFrom, Write};

fn quarantine_log(hash: &[u8; 32], seq: u64, reason: &str) {
    let _ = std::fs::create_dir_all("/etc/thos");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).write(true).open("/etc/thos/quarantine.log") else {
        return;
    };
    // No O_APPEND support yet (a real, separate gap) -- open reads the
    // existing content in as the in-memory buffer without truncating it
    // (there's no O_TRUNC support either), so seeking to the end before
    // writing is what actually makes this an append, not an overwrite.
    let _ = f.seek(SeekFrom::End(0));
    let mut line = format!("seq={seq} reason={reason} hash=");
    for b in hash {
        line.push_str(&format!("{b:02x}"));
    }
    line.push('\n');
    let _ = f.write_all(line.as_bytes());
}

fn main() {
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    // SHA-256 of "THOS-SECSVC-ONLY-TEST-MARKER-v1" (execgate::SECSVC_ONLY_MARKER)
    // -- deliberately not in the kernel's own local BLOCKED_HASHES, so a
    // quarantine verdict on it can only have come from this process.
    let known_bad: [u8; 32] = [
        0x85, 0x30, 0xf6, 0x10, 0x97, 0x07, 0xe6, 0x93, 0xaf, 0xcd, 0x91, 0x83, 0x52, 0x07, 0x91, 0x86,
        0x30, 0x06, 0xcd, 0xad, 0xed, 0x77, 0xac, 0x2c, 0x17, 0x9a, 0x5f, 0x2f, 0x87, 0x1c, 0x93, 0x32,
    ];
    let mut seq: u64 = 0;
    loop {
        let mut hash = [0u8; 32];
        if stdin.read_exact(&mut hash).is_err() {
            break; // the kernel closed its end -- normal shutdown
        }
        if hash == [0xFFu8; 32] {
            break; // test poison pill: simulate a crash
        }
        seq += 1;
        let verdict: u8 = u8::from(hash == known_bad);
        if verdict != 0 {
            quarantine_log(&hash, seq, "known-bad hash (test marker)");
        }
        let _ = stdout.write_all(&[verdict]);
        // std::io::Stdout is buffered -- without this, the verdict byte
        // sits in userspace and never reaches the kernel's blocking read
        // on the other end of the pipe at all (found via a real hang/wrong-
        // verdict failure, not assumed).
        let _ = stdout.flush();
    }
}
