// SPDX-License-Identifier: GPL-2.0-or-later
//! Power off / reboot / halt.
//!
//! No AML interpreter: the S5 (soft-off) sleep type is pulled out of the DSDT's
//! `\_S5_` package by a byte scan (the standard trick), the PM1 control block
//! comes from the FADT, and ACPI mode is switched on through `SMI_CMD` first —
//! a legacy-BIOS boot (the Acer) may not have done it. Reboot tries the FADT
//! reset register, then the 0xCF9 reset port, then an i8042 pulse, then a triple
//! fault. Both paths end with QEMU/VirtualBox shutdown ports as a fallback.
//!
//! The ext2 write path flushes every write (`ahci::write` is durable), so there
//! is nothing to sync here.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use crate::mm::hhdm_offset;

static PM1A: AtomicU32 = AtomicU32::new(0);
static PM1B: AtomicU32 = AtomicU32::new(0);
static SLP_TYP_A: AtomicU16 = AtomicU16::new(0);
static SLP_TYP_B: AtomicU16 = AtomicU16::new(0);
static HAVE_S5: AtomicBool = AtomicBool::new(false);
/// FADT reset register: I/O port (0 = none) and the value to write.
static RESET_PORT: AtomicU16 = AtomicU16::new(0);
static RESET_VALUE: AtomicU16 = AtomicU16::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);

unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}
unsafe fn outw(port: u16, v: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags));
}
unsafe fn outb(port: u16, v: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags));
}
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

fn rd32(b: *const u8, off: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(b.add(off) as *const u32) }
}

/// Read the FADT and DSDT. Call once, after the HHDM is known.
///
/// # Safety
/// `rsdp` is the HHDM virtual address of the RSDP.
pub unsafe fn init(rsdp: *const u8) {
    let Some((fadt, len)) = crate::acpi::find_table_ptr(rsdp, b"FACP") else {
        crate::kprintln!("THOS: power            no FADT — only emulator shutdown ports available");
        return;
    };
    if len < 132 {
        return;
    }
    let smi_cmd = rd32(fadt, 48);
    let acpi_enable = *fadt.add(52);
    let pm1a = rd32(fadt, 64);
    let pm1b = rd32(fadt, 68);
    PM1A.store(pm1a, Ordering::Relaxed);
    PM1B.store(pm1b, Ordering::Relaxed);

    // ACPI mode: SCI_EN is bit 0 of PM1a_CNT. Flip it via the SMI command port
    // if the firmware left the machine in legacy mode.
    if pm1a != 0 && smi_cmd != 0 && acpi_enable != 0 && inw(pm1a as u16) & 1 == 0 {
        outb(smi_cmd as u16, acpi_enable);
        for _ in 0..300 {
            if inw(pm1a as u16) & 1 != 0 {
                break;
            }
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
        }
    }

    // Reset register (ACPI 2.0+): flags bit 10 = RESET_REG_SUP; GAS at 116,
    // value at 128. Only the system-I/O address space (id 1) is used.
    if len >= 129 && rd32(fadt, 112) & (1 << 10) != 0 && *fadt.add(116) == 1 {
        let port = core::ptr::read_unaligned(fadt.add(120) as *const u64);
        if port != 0 && port <= 0xFFFF {
            RESET_PORT.store(port as u16, Ordering::Relaxed);
            RESET_VALUE.store(*fadt.add(128) as u16, Ordering::Relaxed);
        }
    }

    // DSDT: prefer X_DSDT (offset 140) on ACPI 2.0+, else DSDT (offset 40).
    let mut dsdt_pa = rd32(fadt, 40) as u64;
    if len >= 148 {
        let x = core::ptr::read_unaligned(fadt.add(140) as *const u64);
        if x != 0 {
            dsdt_pa = x;
        }
    }
    if dsdt_pa == 0 {
        return;
    }
    let dsdt = (dsdt_pa + hhdm_offset()) as *const u8;
    let dlen = rd32(dsdt, 4) as usize;
    if !(36..=0x400000).contains(&dlen) {
        return;
    }
    let body = core::slice::from_raw_parts(dsdt, dlen);
    if let Some((a, b)) = find_s5(body) {
        SLP_TYP_A.store(a, Ordering::Relaxed);
        SLP_TYP_B.store(b, Ordering::Relaxed);
        HAVE_S5.store(true, Ordering::Relaxed);
        crate::kprintln!(
            "THOS: power ok         ACPI S5 (PM1a {:#x}, SLP_TYP {}/{}), reset {}",
            pm1a, a, b,
            if RESET_PORT.load(Ordering::Relaxed) != 0 { "via FADT" } else { "via 0xCF9/i8042" }
        );
    } else {
        crate::kprintln!("THOS: power            no \\_S5_ in the DSDT — emulator ports only");
    }
}

/// `Name(_S5_, Package(){ a, b, ... })` -> (SLP_TYPa, SLP_TYPb).
fn find_s5(d: &[u8]) -> Option<(u16, u16)> {
    let at = d.windows(4).position(|w| w == b"_S5_")?;
    // Skip "_S5_", then PackageOp (0x12) — it may be preceded by NameOp (0x08).
    let mut i = at + 4;
    if *d.get(i)? != 0x12 {
        return None;
    }
    i += 1;
    let lead = *d.get(i)?;
    i += 1 + (lead >> 6) as usize; // PkgLength: 1 + extra bytes
    i += 1; // NumElements
    let val = |i: &mut usize| -> Option<u16> {
        let mut b = *d.get(*i)?;
        if b == 0x0A {
            *i += 1; // BytePrefix
            b = *d.get(*i)?;
        }
        *i += 1;
        Some(b as u16)
    };
    let a = val(&mut i)?;
    let b = val(&mut i)?;
    Some((a, b))
}

fn pause() {
    for _ in 0..2_000_000 {
        core::hint::spin_loop();
    }
}

fn halt_forever() -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// Switch the machine off. Never returns.
pub fn poweroff() -> ! {
    if BUSY.swap(true, Ordering::SeqCst) {
        halt_forever();
    }
    crate::kprintln!("THOS: powering off");
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
        if HAVE_S5.load(Ordering::Relaxed) {
            let a = SLP_TYP_A.load(Ordering::Relaxed);
            let b = SLP_TYP_B.load(Ordering::Relaxed);
            let (p1a, p1b) = (PM1A.load(Ordering::Relaxed), PM1B.load(Ordering::Relaxed));
            if p1a != 0 {
                outw(p1a as u16, (a << 10) | (1 << 13));
            }
            if p1b != 0 {
                outw(p1b as u16, (b << 10) | (1 << 13));
            }
            pause();
        }
        // Emulator fallbacks: QEMU (new + old PIIX), VirtualBox.
        outw(0x604, 0x2000);
        outw(0xB004, 0x2000);
        outw(0x4004, 0x3400);
    }
    crate::kprintln!("THOS: it is now safe to switch the machine off");
    halt_forever()
}

/// Halt: stop everything but leave the power on. Never returns.
pub fn halt() -> ! {
    crate::kprintln!("THOS: system halted — it is safe to switch the machine off");
    halt_forever()
}

/// Reset the machine. Never returns.
pub fn reboot() -> ! {
    if BUSY.swap(true, Ordering::SeqCst) {
        halt_forever();
    }
    crate::kprintln!("THOS: rebooting");
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
        let rp = RESET_PORT.load(Ordering::Relaxed);
        if rp != 0 {
            outb(rp, RESET_VALUE.load(Ordering::Relaxed) as u8);
            pause();
        }
        outb(0xCF9, 0x02); // hard-reset request: RST_CPU low, then...
        outb(0xCF9, 0x06); // ...SYS_RST + RST_CPU
        pause();
        // i8042: wait for the input buffer to drain, then pulse the reset line.
        for _ in 0..100_000 {
            if inb(0x64) & 2 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
        pause();
        // Last resort: an empty IDT and a fault -> triple fault.
        let idt = [0u8; 10];
        core::arch::asm!("lidt [{0}]", "int3", in(reg) idt.as_ptr(), options(noreturn));
    }
}
