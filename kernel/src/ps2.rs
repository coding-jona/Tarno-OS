// SPDX-License-Identifier: GPL-2.0-or-later
//! i8042 PS/2 keyboard (polled) — the laptop path.
//!
//! The Acer Aspire 5742G (HM55) has no xHCI: its internal keyboard is PS/2
//! behind the i8042. This reads scancodes (set 1, which the controller's
//! translation gives us regardless of the keyboard's native set) and turns them
//! into the same 8-byte HID boot reports the xHCI path produces, so
//! `console::feed_report` — line discipline, SAK and all — is shared.
//!
//! The same controller's second port carries the mouse / touchpad (standard 3-byte PS/2
//! packets; the Synaptics/ELAN pads on laptops of that era speak this by default). Packets
//! are decoded into a pointer position + buttons (`mouse_state`, for the desktop) and queued
//! raw for `/dev/input/mice`.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};

use spin::Mutex;

use crate::wait::WaitQueue;

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

/// Give a pending device response a moment to land in the output buffer.
fn wait_out_full_short() {
    for _ in 0..20_000 {
        if unsafe { inb(STATUS) } & ST_OUT_FULL != 0 {
            return;
        }
    }
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

// ---------------------------------------------------------------------------
//  Mouse (aux port)
// ---------------------------------------------------------------------------

static MOUSE_PRESENT: AtomicBool = AtomicBool::new(false);
static MOUSE_X: AtomicI32 = AtomicI32::new(0);
static MOUSE_Y: AtomicI32 = AtomicI32::new(0);
static MOUSE_BTN: AtomicU8 = AtomicU8::new(0);
/// Complete raw packets for `/dev/input/mice` (bounded: the oldest are dropped).
static MOUSE_Q: Mutex<VecDeque<[u8; 3]>> = Mutex::new(VecDeque::new());
static MOUSE_WQ: WaitQueue = WaitQueue::new();
/// Packet assembly: bytes so far.
static MOUSE_PKT: Mutex<([u8; 3], usize)> = Mutex::new(([0; 3], 0));

fn aux_write(b: u8) -> bool {
    cmd(0xD4) && wait_in_empty() && {
        unsafe { outb(DATA, b) };
        true
    }
}

/// Wait for the device's ACK (0xFA) after an aux command.
fn aux_ack() -> bool {
    for _ in 0..8 {
        if !wait_out_full() {
            return false;
        }
        let b = unsafe { inb(DATA) };
        if b == 0xFA {
            return true;
        }
        if b == 0xFE || b == 0xFC {
            return false; // resend / error
        }
    }
    false
}

/// Enable the second port and start the mouse streaming (defaults, 100 Hz, reporting on).
/// Call after [`init`], before the poll thread starts.
pub fn init_mouse() -> Result<(), &'static str> {
    // The keyboard's ACK to its enable-scanning command (0xFA) may still sit in the output
    // buffer; reading it as the controller config byte below would disable the keyboard.
    for _ in 0..8 {
        wait_out_full_short();
        if unsafe { inb(STATUS) } & ST_OUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA) };
    }
    if !cmd(0xA8) {
        return Err("i8042 aux enable timed out");
    }
    // Config: aux clock on (bit 5 clear), aux IRQ stays off (we poll).
    if !cmd(0x20) || !wait_out_full() {
        return Err("i8042 config read timed out");
    }
    let mut cfg = unsafe { inb(DATA) };
    cfg &= !(1 << 5);
    cfg &= !(1 << 1);
    if !cmd(0x60) || !wait_in_empty() {
        return Err("i8042 config write timed out");
    }
    unsafe { outb(DATA, cfg) };
    if !aux_write(0xF6) || !aux_ack() {
        return Err("no PS/2 mouse (no answer to set-defaults)");
    }
    if !aux_write(0xF4) || !aux_ack() {
        return Err("PS/2 mouse refused enable-reporting");
    }
    MOUSE_PRESENT.store(true, Ordering::Release);
    Ok(())
}

/// Pointer position (clamped to the screen) and button bits (1 = left, 2 = right, 4 = middle).
pub fn mouse_state() -> (i32, i32, u8) {
    (MOUSE_X.load(Ordering::Relaxed), MOUSE_Y.load(Ordering::Relaxed), MOUSE_BTN.load(Ordering::Relaxed))
}

fn mouse_byte(b: u8) {
    if !MOUSE_PRESENT.load(Ordering::Acquire) {
        return;
    }
    let mut p = MOUSE_PKT.lock();
    let n = p.1;
    if n == 0 && b & 0x08 == 0 {
        return; // not a first byte (bit 3 is always set): resynchronise
    }
    p.0[n] = b;
    p.1 += 1;
    if p.1 < 3 {
        return;
    }
    let pkt = p.0;
    p.1 = 0;
    drop(p);
    if pkt[0] & 0xC0 != 0 {
        return; // x/y overflow: discard
    }
    let dx = pkt[1] as i32 - if pkt[0] & 0x10 != 0 { 256 } else { 0 };
    let dy = pkt[2] as i32 - if pkt[0] & 0x20 != 0 { 256 } else { 0 };
    let (w, h) = crate::gdi::screen_size();
    let (w, h) = (w.max(1) as i32, h.max(1) as i32);
    let x = (MOUSE_X.load(Ordering::Relaxed) + dx).clamp(0, w - 1);
    let y = (MOUSE_Y.load(Ordering::Relaxed) - dy).clamp(0, h - 1); // PS/2 y grows upward
    MOUSE_X.store(x, Ordering::Relaxed);
    MOUSE_Y.store(y, Ordering::Relaxed);
    MOUSE_BTN.store(pkt[0] & 7, Ordering::Relaxed);
    {
        let mut q = MOUSE_Q.lock();
        if q.len() >= 128 {
            q.pop_front();
        }
        q.push_back(pkt);
    }
    MOUSE_WQ.wake_all();
}

/// `/dev/input/mice`: block until a raw 3-byte packet is available.
pub fn mouse_read(buf: &mut [u8]) -> i64 {
    if buf.is_empty() {
        return 0;
    }
    loop {
        if let Some(p) = MOUSE_Q.lock().pop_front() {
            let n = buf.len().min(3);
            buf[..n].copy_from_slice(&p[..n]);
            return n as i64;
        }
        if !MOUSE_PRESENT.load(Ordering::Acquire) {
            return -19; // ENODEV
        }
        if crate::signal::interrupted() {
            return -4;
        }
        MOUSE_WQ.wait_if_intr(|| MOUSE_Q.lock().is_empty());
    }
}

/// A mouse packet is half-received: the next byte is about a millisecond away, so the poll
/// thread should not go to sleep yet (the controller holds only one byte).
pub fn mouse_mid_packet() -> bool {
    MOUSE_PKT.lock().1 > 0
}

pub fn mouse_ready() -> bool {
    !MOUSE_Q.lock().is_empty()
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

// ---------------------------------------------------------------------------
//  Input bytes: from the controller into a queue (interrupt or poll), then decoded
// ---------------------------------------------------------------------------

/// Bytes the controller delivered, not yet decoded: `0x100 | byte` for the mouse (aux port),
/// the plain byte for the keyboard. The interrupt handler and the input thread both feed it,
/// always with interrupts off, so a handler can never land inside the queue's own lock.
static RING: Mutex<VecDeque<u16>> = Mutex::new(VecDeque::new());
/// Serialises reads of the controller's data port between the interrupt handler (on one CPU)
/// and a polling pass of the input thread (on another).
static HW: Mutex<()> = Mutex::new(());
static INPUT_WQ: WaitQueue = WaitQueue::new();
static IRQ_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Move every byte the controller has into the queue. Interrupts must be off.
fn pump_locked() {
    let _hw = HW.lock();
    let mut ring = RING.lock();
    loop {
        let st = unsafe { inb(STATUS) };
        if st & ST_OUT_FULL == 0 {
            break;
        }
        let b = unsafe { inb(DATA) } as u16;
        if ring.len() >= 512 {
            ring.pop_front();
        }
        ring.push_back(if st & ST_AUX != 0 { 0x100 | b } else { b });
    }
}

/// The keyboard/mouse interrupt: collect the byte(s) and wake the input thread.
pub fn irq() {
    pump_locked(); // already in an interrupt: interrupts are off
    INPUT_WQ.wake_one();
}

fn hw_pending() -> bool {
    (unsafe { inb(STATUS) }) & ST_OUT_FULL != 0
}

/// Next keyboard scancode byte if one is waiting. Mouse bytes found on the way go to the
/// mouse decoder.
pub fn read_scancode() -> Option<u8> {
    loop {
        let v = x86_64::instructions::interrupts::without_interrupts(|| {
            pump_locked();
            RING.lock().pop_front()
        })?;
        if v & 0x100 != 0 {
            mouse_byte(v as u8);
            continue;
        }
        return Some(v as u8);
    }
}

/// Sleep until there is input to read. With the interrupt routed this is a real block (woken
/// by the handler; the 50 ms timeout is only a safety net); without it, a short poll sleep.
pub fn wait_input() {
    if IRQ_ACTIVE.load(Ordering::Acquire) {
        let deadline = crate::timer::deadline_after_ns(50_000_000);
        INPUT_WQ.wait_if_until(deadline, || RING.lock().is_empty() && !hw_pending());
    } else {
        crate::timer::sleep_ns(8_000_000);
    }
}

/// Turn the controller's keyboard and mouse interrupts on and route them through the I/O APIC.
/// `false` (and everything keeps working by polling) if there is no I/O APIC to route them.
pub fn enable_irqs() -> bool {
    // read the config byte — first drain anything the polling path left in the output buffer
    x86_64::instructions::interrupts::without_interrupts(pump_locked);
    if !cmd(0x20) || !wait_out_full() {
        return false;
    }
    let cfg = unsafe { inb(DATA) };
    let dest = crate::apic::bsp_apic_id();
    if !crate::ioapic::route_isa(1, crate::apic::KBD_VECTOR, dest)
        || !crate::ioapic::route_isa(12, crate::apic::MOUSE_VECTOR, dest)
    {
        return false;
    }
    if !cmd(0x60) || !wait_in_empty() {
        return false;
    }
    unsafe { outb(DATA, cfg | 0b11) }; // keyboard + aux interrupts on
    IRQ_ACTIVE.store(true, Ordering::Release);
    true
}
