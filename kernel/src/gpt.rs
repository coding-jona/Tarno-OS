// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — GUID Partition Table, read-only.
//!
//! Just enough to locate a partition by type GUID (the EFI System Partition) on
//! a GPT-formatted disk. `base_lba` is the disk LBA the GPT's own "LBA 0" sits
//! at — 0 for a whole disk, non-zero when a GPT image is embedded in a larger
//! one. All LBAs inside the header / entries are relative to that base.

use alloc::vec;

use crate::ahci::{self, SECTOR};

/// EFI System Partition type GUID `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`, in the
/// on-disk mixed-endian byte order (first three fields little-endian).
const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn read_sectors(lba: u64, sectors: u32) -> vec::Vec<u8> {
    let total = sectors as usize;
    let mut buf = vec![0u8; total * SECTOR];
    let mut done = 0usize;
    while done < total {
        let n = (total - done).min(64);
        ahci::read(lba + done as u64, &mut buf[done * SECTOR..(done + n) * SECTOR])
            .expect("gpt disk read");
        done += n;
    }
    buf
}

/// Absolute start LBA of the first partition whose type GUID is `want`, on the
/// GPT whose LBA 0 is `base_lba`.
pub fn find_partition(base_lba: u64, want: &[u8; 16]) -> Option<u64> {
    let hdr = read_sectors(base_lba + 1, 1);
    if &hdr[0..8] != b"EFI PART" {
        return None;
    }
    let entry_lba = le64(&hdr[72..]);
    let count = le32(&hdr[80..]) as usize;
    let esize = le32(&hdr[84..]) as usize;
    if esize < 128 || count == 0 || count > 512 {
        return None;
    }

    let bytes = count * esize;
    let sectors = bytes.div_ceil(SECTOR) as u32;
    let arr = read_sectors(base_lba + entry_lba, sectors);

    for i in 0..count {
        let e = &arr[i * esize..i * esize + esize];
        if &e[0..16] == want {
            let start = le64(&e[32..]);
            if start != 0 {
                return Some(base_lba + start);
            }
        }
    }
    None
}

/// Absolute start LBA of the EFI System Partition, if present.
pub fn find_esp(base_lba: u64) -> Option<u64> {
    find_partition(base_lba, &ESP_TYPE_GUID)
}
