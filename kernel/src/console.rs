// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — a line-disciplined console.
//!
//! The xHCI keyboard thread feeds 8-byte HID boot reports here; this decodes
//! them (with a US layout + shift), echoes to the serial console, does minimal
//! line editing (backspace), and queues bytes for `read` on fd 0.

use alloc::collections::VecDeque;

use spin::Mutex;

use crate::serial;

static QUEUE: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
static PREV: Mutex<[u8; 6]> = Mutex::new([0; 6]);
/// Bytes on the current line not yet consumed by a reader — for backspace.
static LINE_LEN: Mutex<usize> = Mutex::new(0);

/// HID Usage ID (0x04..) → (unshifted, shifted) ASCII. 0 = ignore.
fn ascii(code: u8, shift: bool) -> u8 {
    let (lo, hi): (u8, u8) = match code {
        0x04..=0x1D => {
            let c = b'a' + (code - 0x04);
            (c, c - 32)
        }
        0x1E => (b'1', b'!'),
        0x1F => (b'2', b'@'),
        0x20 => (b'3', b'#'),
        0x21 => (b'4', b'$'),
        0x22 => (b'5', b'%'),
        0x23 => (b'6', b'^'),
        0x24 => (b'7', b'&'),
        0x25 => (b'8', b'*'),
        0x26 => (b'9', b'('),
        0x27 => (b'0', b')'),
        0x28 => (b'\n', b'\n'),
        0x2A => (0x08, 0x08), // backspace
        0x2B => (b'\t', b'\t'),
        0x2C => (b' ', b' '),
        0x2D => (b'-', b'_'),
        0x2E => (b'=', b'+'),
        0x2F => (b'[', b'{'),
        0x30 => (b']', b'}'),
        0x31 => (b'\\', b'|'),
        0x33 => (b';', b':'),
        0x34 => (b'\'', b'"'),
        0x35 => (b'`', b'~'),
        0x36 => (b',', b'<'),
        0x37 => (b'.', b'>'),
        0x38 => (b'/', b'?'),
        _ => return 0,
    };
    if shift {
        hi
    } else {
        lo
    }
}

/// Feed one HID boot keyboard report (`[modifiers, reserved, k0..k5]`).
pub fn feed_report(rpt: &[u8; 8]) {
    let shift = rpt[0] & 0b0010_0010 != 0;
    let keys = [rpt[2], rpt[3], rpt[4], rpt[5], rpt[6], rpt[7]];
    let mut prev = PREV.lock();

    for &k in &keys {
        if k == 0 || prev.contains(&k) {
            continue; // held or empty — only act on new key-down
        }
        let c = ascii(k, shift);
        if c == 0 {
            continue;
        }
        if c == 0x08 {
            let mut ll = LINE_LEN.lock();
            if *ll > 0 {
                *ll -= 1;
                let mut q = QUEUE.lock();
                q.pop_back();
                serial::write_bytes(b"\x08 \x08");
            }
        } else {
            QUEUE.lock().push_back(c);
            let mut ll = LINE_LEN.lock();
            if c == b'\n' {
                *ll = 0;
            } else {
                *ll += 1;
            }
            serial::write_bytes(&[c]);
        }
    }
    *prev = keys;
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

#[allow(dead_code)] // used by an interactive line-reader
pub fn has_input() -> bool {
    !QUEUE.lock().is_empty()
}
