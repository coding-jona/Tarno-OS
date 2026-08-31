// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — a line-disciplined console.
//!
//! The xHCI keyboard thread feeds 8-byte HID boot reports here; this decodes
//! them (with a US layout + shift), echoes to the serial console, does minimal
//! line editing (backspace), and queues bytes for `read` on fd 0.

use core::sync::atomic::{AtomicU8, Ordering};

use alloc::collections::VecDeque;

use spin::Mutex;

use crate::serial;
use crate::wait::WaitQueue;

static QUEUE: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
/// Woken when a byte lands in `QUEUE` — a blocked `read` on fd 0 sleeps on it.
static INPUT_WQ: WaitQueue = WaitQueue::new();
static PREV: Mutex<[u8; 6]> = Mutex::new([0; 6]);
/// Bytes on the current line not yet consumed by a reader — for backspace.
static LINE_LEN: Mutex<usize> = Mutex::new(0);

/// Echo mode: 0 = echo the character, 1 = echo `*` (password entry),
/// 2 = echo nothing.
static ECHO: AtomicU8 = AtomicU8::new(0);

#[allow(dead_code)] // used by the `interactive` login flow
pub const ECHO_NORMAL: u8 = 0;
#[allow(dead_code)]
pub const ECHO_MASKED: u8 = 1;

/// Set how typed characters are echoed to the serial console.
#[allow(dead_code)]
pub fn set_echo(mode: u8) {
    ECHO.store(mode, Ordering::Relaxed);
}

/// HID Usage ID → ASCII for a **German (QWERTZ, ISO)** layout.
///
/// `(plain, shifted, altgr)`. 0 = ignore. Only the ASCII-representable keys are
/// mapped: umlauts (ä/ö/ü/ß), the dead keys (´`^) and €/µ/² are left at 0 until
/// the console feeds UTF-8 — a real TTY is a later milestone. The shell-relevant
/// AltGr symbols are here: `@ \ | ~ { } [ ]`.
fn ascii(code: u8, shift: bool, altgr: bool) -> u8 {
    let (lo, hi, ag): (u8, u8, u8) = match code {
        // letters — physical Y/Z swapped vs. US (QWERTZ)
        0x1C => (b'z', b'Z', 0),
        0x1D => (b'y', b'Y', 0),
        0x14 => (b'q', b'Q', b'@'), // AltGr+Q = @
        0x04..=0x1D => {
            let c = b'a' + (code - 0x04);
            (c, c - 32, 0)
        }
        // number row
        0x1E => (b'1', b'!', 0),
        0x1F => (b'2', b'"', 0),
        0x20 => (b'3', 0, 0), // shift = §  (non-ASCII)
        0x21 => (b'4', b'$', 0),
        0x22 => (b'5', b'%', 0),
        0x23 => (b'6', b'&', 0),
        0x24 => (b'7', b'/', b'{'), // AltGr+7 = {
        0x25 => (b'8', b'(', b'['), // AltGr+8 = [
        0x26 => (b'9', b')', b']'), // AltGr+9 = ]
        0x27 => (b'0', b'=', b'}'), // AltGr+0 = }
        0x2D => (0, b'?', b'\\'),   // ß key: AltGr = backslash
        0x2E => (0, 0, 0),          // ´ ` dead keys
        // control
        0x28 => (b'\n', b'\n', 0),
        0x2A => (0x08, 0x08, 0), // backspace
        0x2B => (b'\t', b'\t', 0),
        0x2C => (b' ', b' ', 0),
        // right-hand cluster
        0x2F => (0, 0, 0),         // ü / Ü
        0x30 => (b'+', b'*', b'~'), // AltGr = ~
        0x31 => (b'#', b'\'', 0),
        0x33 => (0, 0, 0),         // ö / Ö
        0x34 => (0, 0, 0),         // ä / Ä
        0x35 => (0, 0, 0),         // ^ ° dead key
        0x36 => (b',', b';', 0),
        0x37 => (b'.', b':', 0),
        0x38 => (b'-', b'_', 0),    // German "-" lives on the US "/?" key
        0x64 => (b'<', b'>', b'|'), // ISO key left of Y: AltGr = |
        _ => return 0,
    };
    if altgr {
        ag
    } else if shift {
        hi
    } else {
        lo
    }
}

/// Feed one HID boot keyboard report (`[modifiers, reserved, k0..k5]`).
pub fn feed_report(rpt: &[u8; 8]) {
    let shift = rpt[0] & 0b0010_0010 != 0; // L/R Shift
    let altgr = rpt[0] & 0b0100_0000 != 0; // Right Alt (AltGr)
    let keys = [rpt[2], rpt[3], rpt[4], rpt[5], rpt[6], rpt[7]];
    let mut prev = PREV.lock();
    let mut pushed = false;

    for &k in &keys {
        if k == 0 || prev.contains(&k) {
            continue; // held or empty — only act on new key-down
        }
        let c = ascii(k, shift, altgr);
        if c == 0 {
            continue;
        }
        let echo = ECHO.load(Ordering::Relaxed);
        if c == 0x08 {
            let mut ll = LINE_LEN.lock();
            if *ll > 0 {
                *ll -= 1;
                let mut q = QUEUE.lock();
                q.pop_back();
                if echo != 2 {
                    serial::write_bytes(b"\x08 \x08");
                }
            }
        } else {
            QUEUE.lock().push_back(c);
            pushed = true;
            let mut ll = LINE_LEN.lock();
            if c == b'\n' {
                *ll = 0;
            } else {
                *ll += 1;
            }
            match echo {
                1 if c != b'\n' => serial::write_bytes(b"*"),
                2 => {}
                _ => serial::write_bytes(&[c]),
            }
        }
    }
    *prev = keys;
    drop(prev);
    if pushed {
        INPUT_WQ.wake_all();
    }
}

/// Non-blocking read into `buf`; returns bytes moved.
pub fn read(buf: &mut [u8]) -> usize {
    let mut q = QUEUE.lock();
    let n = buf.len().min(q.len());
    for b in buf.iter_mut().take(n) {
        *b = q.pop_front().unwrap();
    }
    n
}

/// Block the current thread until at least one byte is available for `read`.
pub fn wait_for_input() {
    INPUT_WQ.wait_if(|| QUEUE.lock().is_empty());
}

#[allow(dead_code)] // used by an interactive line-reader
pub fn has_input() -> bool {
    !QUEUE.lock().is_empty()
}
