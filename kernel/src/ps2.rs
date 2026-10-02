// SPDX-License-Identifier: GPL-2.0-or-later
//! i8042 PS/2 keyboard (polled) — the laptop path.
//!
//! The Acer Aspire 5742G (HM55) has no xHCI: its internal keyboard is PS/2
//! behind the i8042. This reads scancodes (set 1, which the controller's
//! translation gives us regardless of the keyboard's native set) and turns them
//! into the same 8-byte HID boot reports the xHCI path produces, so
//! `console::feed_report` — line discipline, SAK and all — is shared.

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64; // read
const CMD: u16 = 0x64; // write

const ST_OUT_FULL: u8 = 1 << 0;
const ST_IN_FULL: u8 = 1 << 1;
const ST_AUX: u8 = 1 << 5;

unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

fn wait_in_empty() -> bool {
    for _ in 0..100_000 {
        if unsafe { inb(STATUS) } & ST_IN_FULL == 0 {
            return true;
        }
    }
    false
}

fn wait_out_full() -> bool {
    for _ in 0..100_000 {
        if unsafe { inb(STATUS) } & ST_OUT_FULL != 0 {
            return true;
        }
    }
    false
}

fn cmd(c: u8) -> bool {
    if !wait_in_empty() {
        return false;
    }
    unsafe { outb(CMD, c) };
    true
}

/// Bring up port 1 for polling: IRQs off, clock on, set-1 translation on.
pub fn init() -> Result<(), &'static str> {
    // 0xFF on the status port = floating bus, no controller at all.
    if unsafe { inb(STATUS) } == 0xFF {
        return Err("no i8042 controller");
    }
    // Flush stale output.
    for _ in 0..32 {
        if unsafe { inb(STATUS) } & ST_OUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA) };
    }
    if !cmd(0x20) || !wait_out_full() {
        return Err("i8042 config read timed out");
    }
    let mut cfg = unsafe { inb(DATA) };
    cfg &= !(1 << 0); // no keyboard IRQ — we poll
    cfg &= !(1 << 4); // keyboard clock enabled
    cfg |= 1 << 6; // translate to set 1
    if !cmd(0x60) || !wait_in_empty() {
        return Err("i8042 config write timed out");
    }
    unsafe { outb(DATA, cfg) };
    if !cmd(0xAE) {
        return Err("i8042 enable port 1 timed out");
    }
    // Enable scanning; the ACK (0xFA) is flushed by the poll loop below.
    if wait_in_empty() {
        unsafe { outb(DATA, 0xF4) };
    }
    Ok(())
}

/// Set-1 make code -> HID usage (plain keys).
fn hid_plain(sc: u8) -> u8 {
    match sc {
        0x01 => 0x29,
        0x02..=0x0A => 0x1E + (sc - 0x02), // 1..9
        0x0B => 0x27,                      // 0
        0x0C => 0x2D,
        0x0D => 0x2E,
        0x0E => 0x2A,
        0x0F => 0x2B,
        0x10..=0x19 => [0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13][(sc - 0x10) as usize],
        0x1A => 0x2F,
        0x1B => 0x30,
        0x1C => 0x28,
        0x1E..=0x26 => [0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F][(sc - 0x1E) as usize],
        0x27 => 0x33,
        0x28 => 0x34,
        0x29 => 0x35,
        0x2B => 0x31,
        0x2C..=0x32 => [0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10][(sc - 0x2C) as usize],
        0x33 => 0x36,
        0x34 => 0x37,
        0x35 => 0x38,
        0x39 => 0x2C,
        0x3A => 0x39,
        0x3B..=0x44 => 0x3A + (sc - 0x3B), // F1..F10
        0x56 => 0x64,                      // ISO <>| key
        0x57 => 0x44,
        0x58 => 0x45,
        _ => 0,
    }
}

/// Set-1 `E0`-prefixed make code -> HID usage.
fn hid_ext(sc: u8) -> u8 {
    match sc {
        0x1C => 0x58, // keypad enter
        0x47 => 0x4A, // home
        0x48 => 0x52, // up
        0x49 => 0x4B, // pgup
        0x4B => 0x50, // left
        0x4D => 0x4F, // right
        0x4F => 0x4D, // end
        0x50 => 0x51, // down
        0x51 => 0x4E, // pgdn
        0x52 => 0x49, // insert
        0x53 => 0x4C, // delete
        _ => 0,
    }
}

/// Scancode stream -> HID boot reports.
pub struct Decoder {
    ext: bool,
    mods: u8,
    keys: [u8; 6],
}

impl Decoder {
    pub const fn new() -> Self {
        Decoder { ext: false, mods: 0, keys: [0; 6] }
    }

    /// Feed one scancode byte; `Some(report)` when the key state changed.
    pub fn feed(&mut self, b: u8) -> Option<[u8; 8]> {
        match b {
            0xE0 => {
                self.ext = true;
                return None;
            }
            0xE1 | 0xFA | 0xFE | 0x00 | 0xFF => {
                self.ext = false;
                return None; // pause prefix / ACK / resend / overrun (0xAA is Left Shift release, keep it)
            }
            _ => {}
        }
        let ext = core::mem::take(&mut self.ext);
        let release = b & 0x80 != 0;
        let sc = b & 0x7F;

        let modbit = match (ext, sc) {
            (false, 0x1D) => 1 << 0,
            (false, 0x2A) => 1 << 1,
            (false, 0x38) => 1 << 2,
            (true, 0x1D) => 1 << 4,
            (false, 0x36) => 1 << 5,
            (true, 0x38) => 1 << 6,
            _ => 0,
        };
        if modbit != 0 {
            if release {
                self.mods &= !modbit;
            } else {
                self.mods |= modbit;
            }
            return Some(self.report());
        }

        let usage = if ext { hid_ext(sc) } else { hid_plain(sc) };
        if usage == 0 {
            return None;
        }
        if release {
            if let Some(p) = self.keys.iter().position(|&k| k == usage) {
                self.keys[p] = 0;
            }
        } else if !self.keys.contains(&usage) {
            if let Some(p) = self.keys.iter().position(|&k| k == 0) {
                self.keys[p] = usage;
            }
        } else {
            return None; // typematic repeat: the line discipline repeats on its own
        }
        Some(self.report())
    }

    fn report(&self) -> [u8; 8] {
        let k = &self.keys;
        [self.mods, 0, k[0], k[1], k[2], k[3], k[4], k[5]]
    }
}

/// Next keyboard scancode byte if one is waiting (mouse bytes are skipped).
pub fn read_scancode() -> Option<u8> {
    let st = unsafe { inb(STATUS) };
    if st & ST_OUT_FULL == 0 {
        return None;
    }
    let b = unsafe { inb(DATA) };
    if st & ST_AUX != 0 {
        return None;
    }
    Some(b)
}
