// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — minimal PCI config-space access (legacy 0xCF8/0xCFC ports).
//!
//! Enough to find the AHCI controller and read its BARs. ECAM/MCFG and full
//! enumeration come with the wider driver work; on the target machine the
//! devices we care about all sit on bus 0.

use x86_64::instructions::port::Port;

const CONFIG_ADDR: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

#[derive(Clone, Copy, Debug)]
pub struct Location {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl Location {
    fn addr(self, offset: u8) -> u32 {
        0x8000_0000
            | ((self.bus as u32) << 16)
            | ((self.dev as u32) << 11)
            | ((self.func as u32) << 8)
            | ((offset as u32) & 0xFC)
    }
}

pub fn read32(loc: Location, offset: u8) -> u32 {
    unsafe {
        Port::new(CONFIG_ADDR).write(loc.addr(offset));
        Port::<u32>::new(CONFIG_DATA).read()
    }
}

pub fn write32(loc: Location, offset: u8, value: u32) {
    unsafe {
        Port::new(CONFIG_ADDR).write(loc.addr(offset));
        Port::<u32>::new(CONFIG_DATA).write(value);
    }
}

pub fn read16(loc: Location, offset: u8) -> u16 {
    (read32(loc, offset) >> ((offset as u32 & 2) * 8)) as u16
}

/// 16-bit config write (read-modify-write of the containing dword).
pub fn write16(loc: Location, offset: u8, value: u16) {
    let shift = (offset as u32 & 2) * 8;
    let d = read32(loc, offset & 0xFC);
    write32(loc, offset & 0xFC, (d & !(0xFFFF << shift)) | ((value as u32) << shift));
}

/// Config offset of the capability with `id` (0x05 = MSI, 0x11 = MSI-X), if any.
pub fn find_cap(loc: Location, id: u8) -> Option<u8> {
    if read16(loc, 0x06) & (1 << 4) == 0 {
        return None; // status register: capability list present
    }
    let mut ptr = read32(loc, 0x34) as u8 & 0xFC;
    for _ in 0..48 {
        if ptr < 0x40 {
            return None;
        }
        let hdr = read32(loc, ptr);
        if hdr as u8 == id {
            return Some(ptr);
        }
        ptr = (hdr >> 8) as u8 & 0xFC;
        if ptr == 0 {
            return None;
        }
    }
    None
}

/// class (0x0B), subclass (0x0A), prog-if (0x09).
fn class_code(loc: Location) -> (u8, u8, u8) {
    let v = read32(loc, 0x08);
    ((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8)
}

/// Scan bus 0 for the first device with the given (class, subclass, prog-if).
pub fn find_class(class: u8, subclass: u8, progif: u8) -> Option<Location> {
    for dev in 0..32u8 {
        for func in 0..8u8 {
            let loc = Location { bus: 0, dev, func };
            if read16(loc, 0x00) == 0xFFFF {
                if func == 0 {
                    break;
                }
                continue;
            }
            if class_code(loc) == (class, subclass, progif) {
                return Some(loc);
            }
        }
    }
    None
}

/// AHCI SATA controller (0x01 / 0x06 / 0x01).
pub fn find_ahci() -> Option<Location> {
    find_class(0x01, 0x06, 0x01)
}

/// xHCI USB controller (0x0C / 0x03 / 0x30).
pub fn find_xhci() -> Option<Location> {
    find_class(0x0C, 0x03, 0x30)
}

/// 64-bit-aware BAR read. Returns the memory base address (mask off flag bits).
pub fn bar(loc: Location, index: u8) -> u64 {
    let off = 0x10 + index * 4;
    let low = read32(loc, off);
    if low & 0b111 == 0b100 {
        // 64-bit memory BAR
        let high = read32(loc, off + 4);
        (((high as u64) << 32) | (low as u64)) & !0xF
    } else {
        (low as u64) & !0xF
    }
}

/// Set bus-master + memory-space enable in the command register.
pub fn enable_bus_master(loc: Location) {
    let cmd = read16(loc, 0x04);
    write32(loc, 0x04, (cmd | 0b110) as u32);
}
