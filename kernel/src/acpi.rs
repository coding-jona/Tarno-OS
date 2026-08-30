// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — minimal ACPI static-table parsing.
//!
//! Just enough to walk the RSDP → RSDT/XSDT → **MADT** and learn:
//!   * the Local APIC MMIO base,
//!   * every usable CPU's APIC ID (the target's 24 threads),
//!   * the IO APIC(s) and their GSI base,
//!   * the legacy IRQ → GSI interrupt source overrides.
//!
//! No AML here. The DSDT/SSDT interpreter (power, dynamic PCI routing) is a
//! later ACPICA port; see docs/thos/architecture.md.
//!
//! All physical addresses are read through Limine's HHDM (`phys + hhdm_offset`),
//! which already maps the ACPI regions — we do no page-table work.

use alloc::vec::Vec;

use crate::mm::hhdm_offset;

fn phys<T>(pa: u64) -> *const T {
    (pa + hhdm_offset()) as *const T
}

#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_addr: u32,
    // ACPI 2.0+
    length: u32,
    xsdt_addr: u64,
    ext_checksum: u8,
    _reserved: [u8; 3],
}

#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // apic_id lands with SMP bring-up (1f)
pub struct Cpu {
    pub apic_id: u32,
    /// `true` = usable now; `false` = present but must be brought online first.
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct IoApic {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

/// legacy ISA IRQ `source` is delivered on global system interrupt `gsi`.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // consumed when IO APIC redirection entries are set up (Phase 2)
pub struct IrqOverride {
    pub source: u8,
    pub gsi: u32,
    pub flags: u16,
}

pub struct AcpiInfo {
    pub revision: u8,
    pub local_apic_addr: u64,
    pub cpus: Vec<Cpu>,
    pub io_apics: Vec<IoApic>,
    pub overrides: Vec<IrqOverride>,
}

/// # Safety
/// `rsdp_virt` must be the (HHDM) virtual address Limine reported for the RSDP.
pub unsafe fn parse(rsdp_virt: *const u8) -> AcpiInfo {
    let rsdp = &*(rsdp_virt as *const Rsdp);
    assert_eq!(&rsdp.signature, b"RSD PTR ", "bad RSDP signature");
    let revision = rsdp.revision;

    let mut info = AcpiInfo {
        revision,
        local_apic_addr: 0,
        cpus: Vec::new(),
        io_apics: Vec::new(),
        overrides: Vec::new(),
    };

    // Prefer the 64-bit XSDT (ACPI 2.0+); fall back to the 32-bit RSDT.
    let madt = if revision >= 2 && rsdp.xsdt_addr != 0 {
        find_table(rsdp.xsdt_addr, 8)
    } else {
        find_table(rsdp.rsdt_addr as u64, 4)
    };
    let madt = madt.expect("no MADT (APIC) table found");

    parse_madt(madt, &mut info);
    info
}

/// Walk an RSDT (`ptr_size == 4`) or XSDT (`ptr_size == 8`) for the `APIC` table.
unsafe fn find_table(sdt_phys: u64, ptr_size: usize) -> Option<*const SdtHeader> {
    let head = &*phys::<SdtHeader>(sdt_phys);
    let entries = (head.length as usize - core::mem::size_of::<SdtHeader>()) / ptr_size;
    let array = phys::<u8>(sdt_phys).add(core::mem::size_of::<SdtHeader>());

    for i in 0..entries {
        let entry_phys = if ptr_size == 8 {
            core::ptr::read_unaligned(array.add(i * 8) as *const u64)
        } else {
            core::ptr::read_unaligned(array.add(i * 4) as *const u32) as u64
        };
        let hdr = phys::<SdtHeader>(entry_phys);
        if (*hdr).signature == *b"APIC" {
            return Some(hdr);
        }
    }
    None
}

const MADT_LAPIC: u8 = 0;
const MADT_IOAPIC: u8 = 1;
const MADT_IRQ_OVERRIDE: u8 = 2;
const MADT_LAPIC_ADDR_OVERRIDE: u8 = 5;
const MADT_X2APIC: u8 = 9;

unsafe fn parse_madt(madt: *const SdtHeader, info: &mut AcpiInfo) {
    let total = (*madt).length as usize;
    let base = madt as *const u8;

    // MADT: SdtHeader, then u32 local_apic_addr, u32 flags, then entries.
    info.local_apic_addr =
        core::ptr::read_unaligned(base.add(core::mem::size_of::<SdtHeader>()) as *const u32) as u64;

    let mut off = core::mem::size_of::<SdtHeader>() + 8;
    while off + 2 <= total {
        let etype = *base.add(off);
        let elen = *base.add(off + 1) as usize;
        if elen < 2 || off + elen > total {
            break;
        }
        let e = base.add(off);

        match etype {
            MADT_LAPIC => {
                let apic_id = *e.add(3) as u32;
                let flags = core::ptr::read_unaligned(e.add(4) as *const u32);
                let enabled = flags & 0b01 != 0;
                let online_capable = flags & 0b10 != 0;
                if enabled || online_capable {
                    info.cpus.push(Cpu { apic_id, enabled });
                }
            }
            MADT_X2APIC => {
                let apic_id = core::ptr::read_unaligned(e.add(4) as *const u32);
                let flags = core::ptr::read_unaligned(e.add(8) as *const u32);
                let enabled = flags & 0b01 != 0;
                let online_capable = flags & 0b10 != 0;
                if enabled || online_capable {
                    info.cpus.push(Cpu { apic_id, enabled });
                }
            }
            MADT_IOAPIC => {
                info.io_apics.push(IoApic {
                    id: *e.add(2),
                    address: core::ptr::read_unaligned(e.add(4) as *const u32),
                    gsi_base: core::ptr::read_unaligned(e.add(8) as *const u32),
                });
            }
            MADT_IRQ_OVERRIDE => {
                info.overrides.push(IrqOverride {
                    source: *e.add(3),
                    gsi: core::ptr::read_unaligned(e.add(4) as *const u32),
                    flags: core::ptr::read_unaligned(e.add(8) as *const u16),
                });
            }
            MADT_LAPIC_ADDR_OVERRIDE => {
                info.local_apic_addr = core::ptr::read_unaligned(e.add(4) as *const u64);
            }
            _ => {}
        }
        off += elen;
    }
}
