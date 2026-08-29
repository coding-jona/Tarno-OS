// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — Local APIC + APIC timer (BSP).
//!
//! xAPIC MMIO mode (QEMU q35 default; the target's Raptor Lake also supports it).
//! x2APIC and the IO APIC redirection entries come later — the IO APIC is only
//! needed once we wire up legacy IRQs (keyboard) in Phase 2.
//!
//! The APIC timer has no fixed frequency, so we calibrate it once against the
//! 8254 PIT, then arm it periodic at ~100 Hz.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use crate::mm::hhdm_offset;

// --- LAPIC register offsets ---
const REG_ID: u32 = 0x20;
#[allow(dead_code)]
const REG_VERSION: u32 = 0x30;
const REG_TPR: u32 = 0x80;
const REG_EOI: u32 = 0xB0;
const REG_SVR: u32 = 0xF0;
const REG_LVT_TIMER: u32 = 0x320;
const REG_LVT_LINT0: u32 = 0x350;
const REG_LVT_LINT1: u32 = 0x360;
const REG_LVT_ERROR: u32 = 0x370;
const REG_TIMER_INITCNT: u32 = 0x380;
const REG_TIMER_CURRCNT: u32 = 0x390;
const REG_TIMER_DIV: u32 = 0x3E0;

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
const TIMER_DIV_16: u32 = 0b0011;

pub const SPURIOUS_VECTOR: u8 = 0xFF;
pub const TIMER_VECTOR: u8 = 0x20;

/// ~100 Hz.
const TIMER_HZ: u32 = 100;

static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
/// APIC-timer counts per millisecond, from PIT calibration.
static COUNTS_PER_MS: AtomicU64 = AtomicU64::new(0);
static BSP_APIC_ID: AtomicU8 = AtomicU8::new(0);

fn base() -> *mut u8 {
    LAPIC_BASE.load(Ordering::Relaxed) as *mut u8
}

fn read(reg: u32) -> u32 {
    unsafe { core::ptr::read_volatile(base().add(reg as usize) as *const u32) }
}

fn write(reg: u32, val: u32) {
    unsafe { core::ptr::write_volatile(base().add(reg as usize) as *mut u32, val) }
}

pub fn eoi() {
    write(REG_EOI, 0);
}

pub fn on_timer_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn bsp_apic_id() -> u8 {
    BSP_APIC_ID.load(Ordering::Relaxed)
}

pub fn timer_hz() -> u32 {
    TIMER_HZ
}

/// Bring the BSP's Local APIC up and arm the calibrated periodic timer.
///
/// # Safety
/// `local_apic_addr` must be the physical LAPIC base from the MADT, and the IDT
/// must already carry [`TIMER_VECTOR`] / [`SPURIOUS_VECTOR`] handlers.
pub unsafe fn init_bsp(local_apic_addr: u64) {
    LAPIC_BASE.store(local_apic_addr + hhdm_offset(), Ordering::Relaxed);

    BSP_APIC_ID.store((read(REG_ID) >> 24) as u8, Ordering::Relaxed);

    // Accept all priorities; software-enable with the spurious vector.
    write(REG_TPR, 0);
    write(REG_SVR, SVR_ENABLE | SPURIOUS_VECTOR as u32);

    // Nothing wired to LINT0/1 or the error LVT yet — mask them.
    write(REG_LVT_LINT0, LVT_MASKED);
    write(REG_LVT_LINT1, LVT_MASKED);
    write(REG_LVT_ERROR, LVT_MASKED);

    let per_ms = calibrate_against_pit();
    COUNTS_PER_MS.store(per_ms, Ordering::Relaxed);

    let initial = (per_ms as u32).saturating_mul(1000 / TIMER_HZ).max(1);
    write(REG_TIMER_DIV, TIMER_DIV_16);
    write(REG_LVT_TIMER, TIMER_VECTOR as u32 | LVT_TIMER_PERIODIC);
    write(REG_TIMER_INITCNT, initial);
}

pub fn counts_per_ms() -> u64 {
    COUNTS_PER_MS.load(Ordering::Relaxed)
}

// --- 8254 PIT, channel 2, for one-shot calibration ---

const PIT_CH2_DATA: u16 = 0x42;
const PIT_CMD: u16 = 0x43;
const PIT_CH2_GATE: u16 = 0x61;
const PIT_FREQ: u32 = 1_193_182;
const CALIB_MS: u32 = 10;

unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

/// Run the APIC timer freely for `CALIB_MS` gated by the PIT, return
/// APIC-timer counts per millisecond.
unsafe fn calibrate_against_pit() -> u64 {
    let count: u16 = (PIT_FREQ * CALIB_MS / 1000) as u16;

    // Enable gate 2, keep the speaker off.
    let g = (inb(PIT_CH2_GATE) & !0x02) | 0x01;
    outb(PIT_CH2_GATE, g);

    // Channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count).
    outb(PIT_CMD, 0xB0);
    outb(PIT_CH2_DATA, (count & 0xFF) as u8);
    outb(PIT_CH2_DATA, (count >> 8) as u8);

    // Restart the gate to (re)load the counter.
    let g = inb(PIT_CH2_GATE) & !0x01;
    outb(PIT_CH2_GATE, g);
    let g = inb(PIT_CH2_GATE) | 0x01;
    outb(PIT_CH2_GATE, g);

    // Start the APIC timer from max, one-shot, masked (we poll CURRCNT).
    write(REG_TIMER_DIV, TIMER_DIV_16);
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_TIMER_INITCNT, u32::MAX);

    // Wait for PIT OUT2 (bit 5) to go high == counter hit 0.
    while inb(PIT_CH2_GATE) & 0x20 == 0 {}

    let elapsed = u32::MAX - read(REG_TIMER_CURRCNT);
    write(REG_TIMER_INITCNT, 0); // stop
    (elapsed / CALIB_MS) as u64
}
