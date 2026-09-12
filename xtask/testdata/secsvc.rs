// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: a minimal Security Service. Reads a 32-byte SHA-256 hash from
// fd 0 (wired to the kernel<->service request pipe, not the console),
// writes back one verdict byte (1 = quarantine, 0 = allow) on fd 1, in a
// loop -- the real protocol `secsvc.rs` (kernel side) speaks.
//
// A magic all-0xFF "hash" (impossible to arise from a real SHA-256 of
// anything) is a test-only poison pill: exit immediately, simulating a
// crash, so the kernel side's crash-degrade fallback gets exercised for
// real instead of assumed.
use std::io::{Read, Write};

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
    loop {
        let mut hash = [0u8; 32];
        if stdin.read_exact(&mut hash).is_err() {
            break; // the kernel closed its end -- normal shutdown
        }
        if hash == [0xFFu8; 32] {
            break; // test poison pill: simulate a crash
        }
        let verdict: u8 = u8::from(hash == known_bad);
        let _ = stdout.write_all(&[verdict]);
        // std::io::Stdout is buffered -- without this, the verdict byte
        // sits in userspace and never reaches the kernel's blocking read
        // on the other end of the pipe at all (found via a real hang/wrong-
        // verdict failure, not assumed).
        let _ = stdout.flush();
    }
}
