// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — FAT (FAT16 / FAT32), read-only.
//!
//! Enough to read a file out of an EFI System Partition: parse the BPB, walk
//! the FAT cluster chain, traverse 8.3 directory entries. Long-name (VFAT)
//! entries are skipped, so only short names match. No writes, no FAT12, no
//! timestamps. Reads go through the AHCI driver; the caller passes the
//! partition's starting LBA (0 for a whole-disk "super-floppy" volume).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::ahci::{self, SECTOR};

const EOC16: u32 = 0xFFF8;
const EOC32: u32 = 0x0FFF_FFF8;
const ATTR_LFN: u8 = 0x0F;
const ATTR_DIR: u8 = 0x10;

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Read `sectors` sectors starting at absolute `lba` into a fresh buffer.
fn read_sectors(lba: u64, sectors: u32) -> Vec<u8> {
    let total = sectors as usize;
    let mut buf = vec![0u8; total * SECTOR];
    let mut done = 0usize;
    while done < total {
        let n = (total - done).min(64);
        ahci::read(lba + done as u64, &mut buf[done * SECTOR..(done + n) * SECTOR])
            .expect("fat disk read");
        done += n;
    }
    buf
}

pub struct Fat {
    sec_per_clus: u32,
    /// Absolute LBA of the first FAT.
    fat_start: u64,
    /// FAT16 only: absolute LBA + length of the fixed root directory region.
    root_dir_lba: u64,
    root_dir_sectors: u32,
    /// Absolute LBA of cluster 2.
    data_start_lba: u64,
    /// FAT32 only: first cluster of the root directory.
    root_cluster: u32,
    is_fat32: bool,
}

impl Fat {
    /// Parse the BPB at `part_lba` (the partition's first sector).
    pub fn open(part_lba: u64) -> Result<Fat, &'static str> {
        let bpb = read_sectors(part_lba, 1);
        if le16(&bpb[510..]) != 0xAA55 {
            return Err("FAT: no boot signature");
        }
        let bytes_per_sec = le16(&bpb[11..]) as u32;
        let sec_per_clus = bpb[13] as u32;
        let rsvd = le16(&bpb[14..]) as u32;
        let num_fats = bpb[16] as u32;
        let root_entries = le16(&bpb[17..]) as u32;
        let tot16 = le16(&bpb[19..]) as u32;
        let fatsz16 = le16(&bpb[22..]) as u32;
        let tot32 = le32(&bpb[32..]);
        let fatsz32 = le32(&bpb[36..]);

        if bytes_per_sec != SECTOR as u32 {
            return Err("FAT: unsupported sector size");
        }
        if sec_per_clus == 0 || num_fats == 0 {
            return Err("FAT: bad BPB");
        }

        let fatsz = if fatsz16 != 0 { fatsz16 } else { fatsz32 };
        let total_sec = if tot16 != 0 { tot16 } else { tot32 };
        let root_dir_sectors = (root_entries * 32).div_ceil(bytes_per_sec);
        let first_data_sec = rsvd + num_fats * fatsz + root_dir_sectors;
        if total_sec <= first_data_sec {
            return Err("FAT: BPB sizes inconsistent");
        }
        let clusters = (total_sec - first_data_sec) / sec_per_clus;
        let is_fat32 = clusters >= 65525;

        Ok(Fat {
            sec_per_clus,
            fat_start: part_lba + rsvd as u64,
            root_dir_lba: part_lba + (rsvd + num_fats * fatsz) as u64,
            root_dir_sectors,
            data_start_lba: part_lba + first_data_sec as u64,
            root_cluster: if is_fat32 { le32(&bpb[44..]) } else { 0 },
            is_fat32,
        })
    }

    fn clus_lba(&self, clus: u32) -> u64 {
        self.data_start_lba + (clus as u64 - 2) * self.sec_per_clus as u64
    }

    /// The next cluster in the chain, or `None` at end-of-chain / free / bad.
    fn next_clus(&self, clus: u32) -> Option<u32> {
        let (ent, eoc) = if self.is_fat32 { (4u64, EOC32) } else { (2u64, EOC16) };
        let byte = clus as u64 * ent;
        let buf = read_sectors(self.fat_start + byte / SECTOR as u64, 1);
        let off = (byte % SECTOR as u64) as usize;
        let val = if self.is_fat32 {
            le32(&buf[off..]) & 0x0FFF_FFFF
        } else {
            le16(&buf[off..]) as u32
        };
        if val < 2 || val >= eoc {
            None
        } else {
            Some(val)
        }
    }

    /// Concatenate the cluster chain from `start`. Stops once `at_least` bytes
    /// are collected (`0` = follow to end-of-chain).
    fn read_chain(&self, start: u32, at_least: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut clus = start;
        loop {
            out.extend_from_slice(&read_sectors(self.clus_lba(clus), self.sec_per_clus));
            if at_least != 0 && out.len() >= at_least {
                break;
            }
            match self.next_clus(clus) {
                Some(n) => clus = n,
                None => break,
            }
        }
        out
    }

    /// Raw 32-byte directory entries. `first_clus == 0` selects the FAT16 root.
    fn read_dir(&self, first_clus: u32) -> Vec<u8> {
        if !self.is_fat32 && first_clus == 0 {
            read_sectors(self.root_dir_lba, self.root_dir_sectors)
        } else {
            self.read_chain(first_clus, 0)
        }
    }

    /// Find `name` (case-insensitive 8.3) in a directory. Returns
    /// `(first_cluster, size, is_dir)`.
    fn lookup(&self, dir_clus: u32, name: &str) -> Option<(u32, u32, bool)> {
        let data = self.read_dir(dir_clus);
        for ent in data.chunks_exact(32) {
            match ent[0] {
                0x00 => break,    // end of directory
                0xE5 => continue, // deleted
                _ => {}
            }
            if ent[11] & ATTR_LFN == ATTR_LFN {
                continue; // long-name component
            }
            let base = core::str::from_utf8(&ent[0..8]).ok()?.trim_end();
            let ext = core::str::from_utf8(&ent[8..11]).ok()?.trim_end();
            let mut sname = String::from(base);
            if !ext.is_empty() {
                sname.push('.');
                sname.push_str(ext);
            }
            if sname.eq_ignore_ascii_case(name) {
                let clus = ((le16(&ent[20..]) as u32) << 16) | le16(&ent[26..]) as u32;
                return Some((clus, le32(&ent[28..]), ent[11] & ATTR_DIR != 0));
            }
        }
        None
    }

    /// Read the file at an absolute `/`- or `\`-separated path. `None` if any
    /// component is missing, or the leaf is a directory.
    pub fn read_path(&self, path: &str) -> Option<Vec<u8>> {
        let comps: Vec<&str> =
            path.split(|c| c == '/' || c == '\\').filter(|s| !s.is_empty()).collect();
        if comps.is_empty() {
            return None;
        }
        let mut clus = if self.is_fat32 { self.root_cluster } else { 0 };
        for (i, comp) in comps.iter().enumerate() {
            let (fc, size, is_dir) = self.lookup(clus, comp)?;
            let last = i + 1 == comps.len();
            if last {
                if is_dir {
                    return None;
                }
                if size == 0 || fc < 2 {
                    return Some(Vec::new());
                }
                let mut data = self.read_chain(fc, size as usize);
                data.truncate(size as usize);
                return Some(data);
            }
            if !is_dir {
                return None;
            }
            clus = fc;
        }
        None
    }
}
