// SPDX-License-Identifier: GPL-2.0-or-later
//! The I/O APIC: routes legacy ISA interrupts (the PS/2 keyboard and mouse) to a vector on
//! the boot CPU. Only what the input devices need — fixed delivery, one destination, the
//! MADT's interrupt source overrides honoured. (PCI devices use MSI instead.)

use alloc::vec::Vec;

use spin::Once;

use crate::acpi::AcpiInfo;

/// `(register window physical address, first GSI it serves)` of the first I/O APIC.
static IOAPIC: Once<(u64, u32)> = Once::new();
/// MADT interrupt source overrides: `(isa irq, gsi, flags)`.
static OVERRIDES: Once<Vec<(u8, u32, u16)>> = Once::new();
/// The mapped register window.
static WINDOW: Once<u64> = Once::new();

/// Remember what ACPI told us (call once, after `acpi::parse`).
pub fn remember(info: &AcpiInfo) {
    if let Some(io) = info.io_apics.first() {
        IOAPIC.call_once(|| (io.address as u64, io.gsi_base));
    }
    OVERRIDES.call_once(|| info.overrides.iter().map(|o| (o.source, o.gsi, o.flags)).collect());
}

fn window() -> Option<u64> {
    let (phys, _) = *IOAPIC.get()?;
    Some(*WINDOW.call_once(|| crate::vmm::map_mmio(phys, 0x1000)))
}

fn read(w: u64, reg: u32) -> u32 {
    unsafe {
        (w as *mut u32).write_volatile(reg);
        ((w + 0x10) as *const u32).read_volatile()
    }
}

fn write(w: u64, reg: u32, val: u32) {
    unsafe {
        (w as *mut u32).write_volatile(reg);
        ((w + 0x10) as *mut u32).write_volatile(val);
    }
}

/// Silence the legacy 8259 PICs so an interrupt is delivered once, through the I/O APIC.
fn mask_pics() {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0x21u16, in("al") 0xFFu8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") 0xA1u16, in("al") 0xFFu8, options(nomem, nostack));
    }
}

/// Deliver ISA interrupt `irq` as `vector` to the CPU with local APIC id `dest`. `false` if
/// there is no I/O APIC or the line is not inside its range.
pub fn route_isa(irq: u8, vector: u8, dest: u8) -> bool {
    let Some(w) = window() else { return false };
    let (_, gsi_base) = *IOAPIC.get().unwrap();
    // ISA default: edge-triggered, active high; the MADT may say otherwise.
    let (gsi, flags) = OVERRIDES
        .get()
        .and_then(|o| o.iter().find(|x| x.0 == irq).map(|x| (x.1, x.2)))
        .unwrap_or((irq as u32, 0));
    if gsi < gsi_base {
        return false;
    }
    let pin = gsi - gsi_base;
    let max_pin = (read(w, 1) >> 16) & 0xFF;
    if pin > max_pin {
        return false;
    }
    let mut low = vector as u32; // fixed delivery, physical destination, unmasked
    if flags & 0b11 == 0b11 {
        low |= 1 << 13; // active low
    }
    if (flags >> 2) & 0b11 == 0b11 {
        low |= 1 << 15; // level triggered
    }
    mask_pics();
    write(w, 0x11 + 2 * pin, (dest as u32) << 24);
    write(w, 0x10 + 2 * pin, low);
    read(w, 0x10 + 2 * pin) & 0xFF == vector as u32 // the entry took
}
