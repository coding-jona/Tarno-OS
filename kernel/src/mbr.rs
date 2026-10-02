// SPDX-License-Identifier: GPL-2.0-or-later
//! Legacy MBR (MS-DOS) partition table, read-only.
//!
//! The Acer Aspire 5742G is BIOS-only (no UEFI, no CSM switch): its disks carry
//! an MBR, not a GPT. Four primary entries at 0x1BE; extended partitions are not
//! followed (THOS needs one Linux-type partition for the root FS).

use crate::ahci::{self, SECTOR};

pub const TYPE_LINUX: u8 = 0x83;
const SIG: [u8; 2] = [0x55, 0xAA];
/// A protective-MBR entry (GPT disk) — not a real partition table for us.
const TYPE_GPT_PROTECTIVE: u8 = 0xEE;

#[derive(Clone, Copy)]
pub struct Entry {
    pub ptype: u8,
    pub start_lba: u64,
    pub sectors: u64,
}

/// The primary entries of the MBR at disk LBA 0 (empty slots skipped). Empty if
/// the sector has no 0x55AA signature or is a GPT protective MBR.
pub fn entries() -> alloc::vec::Vec<Entry> {
    let mut s = [0u8; SECTOR];
    let mut out = alloc::vec::Vec::new();
    if ahci::read(0, &mut s).is_err() || s[510..512] != SIG {
        return out;
    }
    for i in 0..4 {
        let e = &s[0x1BE + i * 16..0x1BE + i * 16 + 16];
        let ptype = e[4];
        let start = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64;
        let count = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
        if ptype == TYPE_GPT_PROTECTIVE {
            return alloc::vec::Vec::new();
        }
        if ptype != 0 && start != 0 && count != 0 {
            out.push(Entry { ptype, start_lba: start, sectors: count });
        }
    }
    out
}
