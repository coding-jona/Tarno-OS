// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — ext2 (read + basic write).
//!
//! Mount the SATA disk, walk a path from the root inode, read a file's data
//! blocks (12 direct + single + double indirect). Write side: block / inode
//! bitmap allocators, `write_path` (create-or-overwrite a regular file) and
//! `mkdir_path`, enough for an installer to lay a tree down. Still missing:
//! htree dirs, growing a directory past 12 direct blocks, backup superblock /
//! group-descriptor copies (fine for a single-group image, not for the real
//! multi-group SSD yet), 64-bit sizes, journalling, timestamps.

use alloc::vec;
use alloc::vec::Vec;

use crate::ahci::{self, SECTOR};

const SB_OFFSET: u64 = 1024;
const EXT2_MAGIC: u16 = 0xEF53;
pub const ROOT_INO: u32 = 2;

pub struct Ext2 {
    block_size: u32,
    inode_size: u32,
    inodes_per_group: u32,
    first_data_block: u32,
    blocks_per_group: u32,
    block_count: u32,
    /// `s_feature_incompat & FILETYPE` — dir entries carry a file-type byte.
    filetype: bool,
}

#[allow(dead_code)] // mode used once we honour permissions / file types
pub struct Inode {
    pub mode: u16,
    pub size: u64,
    pub block: [u32; 15],
}

/// Read an arbitrary byte range off the disk (sector-granular under the hood).
fn disk_range(off: u64, len: usize) -> Vec<u8> {
    let start_lba = off / SECTOR as u64;
    let end_lba = (off + len as u64 + SECTOR as u64 - 1) / SECTOR as u64;
    let sectors = (end_lba - start_lba) as usize;
    let mut raw = vec![0u8; sectors * SECTOR];
    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(8);
        ahci::read(start_lba + done as u64, &mut raw[done * SECTOR..(done + n) * SECTOR])
            .expect("ext2 disk read");
        done += n;
    }
    let skip = (off - start_lba * SECTOR as u64) as usize;
    raw[skip..skip + len].to_vec()
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}

pub fn open() -> Result<Ext2, &'static str> {
    let sb = disk_range(SB_OFFSET, 1024);
    if le16(&sb[56..]) != EXT2_MAGIC {
        return Err("ext2 magic not found");
    }
    let block_size = 1024u32 << le32(&sb[24..]);
    let rev = le32(&sb[76..]);
    let inode_size = if rev >= 1 { le16(&sb[88..]) as u32 } else { 128 };
    Ok(Ext2 {
        block_size,
        inode_size,
        inodes_per_group: le32(&sb[40..]),
        first_data_block: le32(&sb[20..]),
        blocks_per_group: le32(&sb[32..]),
        block_count: le32(&sb[4..]),
        filetype: le32(&sb[96..]) & 0x0002 != 0, // EXT2_FEATURE_INCOMPAT_FILETYPE
    })
}

/// Write an arbitrary byte range to the disk (sector-granular RMW under the
/// hood). Every `ahci::write` flushes, so a returned `disk_write` is durable.
fn disk_write(off: u64, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let start_lba = off / SECTOR as u64;
    let end_lba = (off + data.len() as u64 + SECTOR as u64 - 1) / SECTOR as u64;
    let sectors = (end_lba - start_lba) as usize;
    let mut raw = vec![0u8; sectors * SECTOR];

    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(8);
        ahci::read(start_lba + done as u64, &mut raw[done * SECTOR..(done + n) * SECTOR])
            .expect("ext2 rmw read");
        done += n;
    }

    let skip = (off - start_lba * SECTOR as u64) as usize;
    raw[skip..skip + data.len()].copy_from_slice(data);

    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(8);
        ahci::write(start_lba + done as u64, &raw[done * SECTOR..(done + n) * SECTOR])
            .expect("ext2 rmw write");
        done += n;
    }
}

fn round4(n: usize) -> usize {
    (n + 3) & !3
}

/// `("/a/b/c") -> ("/a/b", "c")`, `("/c") -> ("/", "c")`.
fn split_parent(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_end_matches('/');
    let idx = path.rfind('/')?;
    let name = &path[idx + 1..];
    if name.is_empty() {
        return None;
    }
    Some((if idx == 0 { "/" } else { &path[..idx] }, name))
}

/// Patch the managed fields of a raw inode buffer in place.
fn set_inode(raw: &mut [u8], mode: u16, size: u64, links: u16, blocks512: u32, block: &[u32; 15]) {
    raw[0..2].copy_from_slice(&mode.to_le_bytes());
    raw[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    raw[26..28].copy_from_slice(&links.to_le_bytes());
    raw[28..32].copy_from_slice(&blocks512.to_le_bytes()); // i_blocks (512-byte units)
    for (i, b) in block.iter().enumerate() {
        raw[40 + i * 4..44 + i * 4].copy_from_slice(&b.to_le_bytes());
    }
}

impl Ext2 {
    fn block(&self, n: u32) -> Vec<u8> {
        disk_range(n as u64 * self.block_size as u64, self.block_size as usize)
    }

    pub fn read_inode(&self, ino: u32) -> Inode {
        let group = (ino - 1) / self.inodes_per_group;
        let idx = (ino - 1) % self.inodes_per_group;

        let bgd_off = (self.first_data_block + 1) as u64 * self.block_size as u64 + group as u64 * 32;
        let bgd = disk_range(bgd_off, 32);
        let inode_table = le32(&bgd[8..]); // bg_inode_table

        let off = inode_table as u64 * self.block_size as u64 + idx as u64 * self.inode_size as u64;
        let raw = disk_range(off, self.inode_size as usize);

        let mut block = [0u32; 15];
        for (i, b) in block.iter_mut().enumerate() {
            *b = le32(&raw[40 + i * 4..]);
        }
        Inode {
            mode: le16(&raw[0..]),
            size: le32(&raw[4..]) as u64,
            block,
        }
    }

    /// Append one data block (a `0` pointer is a hole → zeros).
    fn feed_block(&self, out: &mut Vec<u8>, bn: u32) {
        if bn == 0 {
            let bs = self.block_size as usize;
            out.resize(out.len() + bs, 0);
        } else {
            out.extend_from_slice(&self.block(bn));
        }
    }

    /// Follow a single-indirect block: `bs/4` data-block pointers.
    fn feed_indirect(&self, out: &mut Vec<u8>, ind_bn: u32, total: usize) {
        if ind_bn == 0 {
            let span = (self.block_size as usize / 4) * self.block_size as usize;
            let n = span.min(total.saturating_sub(out.len()));
            out.resize(out.len() + n, 0);
            return;
        }
        let ind = self.block(ind_bn);
        for c in ind.chunks_exact(4) {
            if out.len() >= total {
                return;
            }
            self.feed_block(out, le32(c));
        }
    }

    pub fn read_file(&self, inode: &Inode) -> Vec<u8> {
        let total = inode.size as usize;
        let mut out = Vec::with_capacity(total);

        for i in 0..12 {
            if out.len() >= total {
                break;
            }
            self.feed_block(&mut out, inode.block[i]);
        }

        if out.len() < total {
            self.feed_indirect(&mut out, inode.block[12], total);
        }

        // Double indirect: bs/4 single-indirect blocks.
        if out.len() < total && inode.block[13] != 0 {
            let dind = self.block(inode.block[13]);
            for c in dind.chunks_exact(4) {
                if out.len() >= total {
                    break;
                }
                self.feed_indirect(&mut out, le32(c), total);
            }
        }

        out.truncate(total);
        out
    }

    fn lookup(&self, dir_ino: u32, name: &str) -> Option<u32> {
        let data = self.read_file(&self.read_inode(dir_ino));
        let mut off = 0;
        while off + 8 <= data.len() {
            let ino = le32(&data[off..]);
            let rec_len = le16(&data[off + 4..]) as usize;
            let name_len = data[off + 6] as usize;
            if rec_len == 0 {
                break;
            }
            if ino != 0 && off + 8 + name_len <= data.len() && &data[off + 8..off + 8 + name_len] == name.as_bytes() {
                return Some(ino);
            }
            off += rec_len;
        }
        None
    }

    pub fn path_lookup(&self, path: &str) -> Option<u32> {
        let mut ino = ROOT_INO;
        for comp in path.split('/').filter(|s| !s.is_empty()) {
            ino = self.lookup(ino, comp)?;
        }
        Some(ino)
    }

    pub fn read_path(&self, path: &str) -> Option<Vec<u8>> {
        let ino = self.path_lookup(path)?;
        Some(self.read_file(&self.read_inode(ino)))
    }

    // ---- write side ----

    fn write_block(&self, n: u32, data: &[u8]) {
        disk_write(n as u64 * self.block_size as u64, data);
    }

    fn group_count(&self) -> u32 {
        (self.block_count - self.first_data_block).div_ceil(self.blocks_per_group)
    }

    fn bgd_off(&self, g: u32) -> u64 {
        (self.first_data_block + 1) as u64 * self.block_size as u64 + g as u64 * 32
    }
    fn read_bgd(&self, g: u32) -> Vec<u8> {
        disk_range(self.bgd_off(g), 32)
    }

    /// Add `delta` to a little-endian u32 field at `SB_OFFSET + off`.
    fn sb_add32(&self, off: u64, delta: i64) {
        let cur = le32(&disk_range(SB_OFFSET + off, 4)) as i64;
        disk_write(SB_OFFSET + off, &((cur + delta) as u32).to_le_bytes());
    }
    /// Add `delta` to a little-endian u16 field at `bgd(g) + off`.
    fn bgd_add16(&self, g: u32, off: u64, delta: i64) {
        let o = self.bgd_off(g) + off;
        let cur = le16(&disk_range(o, 2)) as i64;
        disk_write(o, &((cur + delta) as u16).to_le_bytes());
    }

    /// First clear bit in `bitmap_blk` below `count`; sets it and writes back.
    fn take_bit(&self, bitmap_blk: u32, count: u32) -> Option<u32> {
        let mut bm = self.block(bitmap_blk);
        for i in 0..count as usize {
            if bm[i / 8] & (1 << (i % 8)) == 0 {
                bm[i / 8] |= 1 << (i % 8);
                self.write_block(bitmap_blk, &bm);
                return Some(i as u32);
            }
        }
        None
    }

    /// Allocate one zeroed data block; updates the group + superblock counts.
    fn alloc_block(&self) -> Option<u32> {
        let groups = self.group_count();
        for g in 0..groups {
            let bgd = self.read_bgd(g);
            if le16(&bgd[12..]) == 0 {
                continue;
            }
            let in_group = if g == groups - 1 {
                self.block_count - self.first_data_block - g * self.blocks_per_group
            } else {
                self.blocks_per_group
            };
            if let Some(i) = self.take_bit(le32(&bgd[0..]), in_group) {
                self.bgd_add16(g, 12, -1);
                self.sb_add32(12, -1);
                let bno = self.first_data_block + g * self.blocks_per_group + i;
                self.write_block(bno, &vec![0u8; self.block_size as usize]);
                return Some(bno);
            }
        }
        None
    }

    fn free_block(&self, bno: u32) {
        let rel = bno - self.first_data_block;
        let g = rel / self.blocks_per_group;
        let i = (rel % self.blocks_per_group) as usize;
        let bgd = self.read_bgd(g);
        let bmb = le32(&bgd[0..]);
        let mut bm = self.block(bmb);
        if bm[i / 8] & (1 << (i % 8)) != 0 {
            bm[i / 8] &= !(1 << (i % 8));
            self.write_block(bmb, &bm);
            self.bgd_add16(g, 12, 1);
            self.sb_add32(12, 1);
        }
    }

    /// Allocate one inode; zeroes its table slot, bumps `bg_used_dirs_count`
    /// when `is_dir`.
    fn alloc_inode(&self, is_dir: bool) -> Option<u32> {
        for g in 0..self.group_count() {
            let bgd = self.read_bgd(g);
            if le16(&bgd[14..]) == 0 {
                continue;
            }
            if let Some(i) = self.take_bit(le32(&bgd[4..]), self.inodes_per_group) {
                self.bgd_add16(g, 14, -1);
                self.sb_add32(16, -1);
                if is_dir {
                    self.bgd_add16(g, 16, 1);
                }
                let ino = g * self.inodes_per_group + i + 1;
                self.patch_inode(ino, |raw| raw.fill(0));
                return Some(ino);
            }
        }
        None
    }

    fn inode_off(&self, ino: u32) -> u64 {
        let group = (ino - 1) / self.inodes_per_group;
        let idx = (ino - 1) % self.inodes_per_group;
        let table = le32(&self.read_bgd(group)[8..]);
        table as u64 * self.block_size as u64 + idx as u64 * self.inode_size as u64
    }

    fn patch_inode(&self, ino: u32, f: impl FnOnce(&mut [u8])) {
        let off = self.inode_off(ino);
        let mut raw = disk_range(off, self.inode_size as usize);
        f(&mut raw);
        disk_write(off, &raw);
    }

    fn bump_links(&self, ino: u32, delta: i32) {
        self.patch_inode(ino, |raw| {
            let v = (le16(&raw[26..]) as i32 + delta) as u16;
            raw[26..28].copy_from_slice(&v.to_le_bytes());
        });
    }

    /// Free every data / indirect / double-indirect block an inode owns.
    fn free_all_blocks(&self, node: &Inode) {
        for &b in &node.block[..12] {
            if b != 0 {
                self.free_block(b);
            }
        }
        if node.block[12] != 0 {
            for c in self.block(node.block[12]).chunks_exact(4) {
                if le32(c) != 0 {
                    self.free_block(le32(c));
                }
            }
            self.free_block(node.block[12]);
        }
        if node.block[13] != 0 {
            for c in self.block(node.block[13]).chunks_exact(4) {
                let sib = le32(c);
                if sib == 0 {
                    continue;
                }
                for d in self.block(sib).chunks_exact(4) {
                    if le32(d) != 0 {
                        self.free_block(le32(d));
                    }
                }
                self.free_block(sib);
            }
            self.free_block(node.block[13]);
        }
    }

    /// Allocate + fill data blocks for `data`. Returns `(block[15], i_blocks)`
    /// where `i_blocks` counts data + indirect blocks in 512-byte units.
    fn lay_out_data(&self, data: &[u8]) -> Option<([u32; 15], u32)> {
        let bs = self.block_size as usize;
        let per = bs / 4; // pointers per indirect block
        let nblocks = data.len().div_ceil(bs);
        let mut block = [0u32; 15];
        let mut meta = 0u32;
        let mut bi = 0usize;

        let chunk = |i: usize| -> Vec<u8> {
            let s = i * bs;
            let mut v = data[s..(s + bs).min(data.len())].to_vec();
            v.resize(bs, 0);
            v
        };

        while bi < nblocks && bi < 12 {
            let b = self.alloc_block()?;
            self.write_block(b, &chunk(bi));
            block[bi] = b;
            bi += 1;
        }

        if bi < nblocks {
            let ind = self.alloc_block()?;
            meta += 1;
            let mut buf = vec![0u8; bs];
            let mut k = 0;
            while bi < nblocks && k < per {
                let b = self.alloc_block()?;
                self.write_block(b, &chunk(bi));
                buf[k * 4..k * 4 + 4].copy_from_slice(&b.to_le_bytes());
                bi += 1;
                k += 1;
            }
            self.write_block(ind, &buf);
            block[12] = ind;
        }

        if bi < nblocks {
            let dind = self.alloc_block()?;
            meta += 1;
            let mut dbuf = vec![0u8; bs];
            let mut j = 0;
            while bi < nblocks && j < per {
                let ind = self.alloc_block()?;
                meta += 1;
                let mut buf = vec![0u8; bs];
                let mut k = 0;
                while bi < nblocks && k < per {
                    let b = self.alloc_block()?;
                    self.write_block(b, &chunk(bi));
                    buf[k * 4..k * 4 + 4].copy_from_slice(&b.to_le_bytes());
                    bi += 1;
                    k += 1;
                }
                self.write_block(ind, &buf);
                dbuf[j * 4..j * 4 + 4].copy_from_slice(&ind.to_le_bytes());
                j += 1;
            }
            self.write_block(dind, &dbuf);
            block[13] = dind;
        }

        if bi < nblocks {
            return None; // would need triple-indirect
        }
        Some((block, (nblocks as u32 + meta) * (self.block_size / 512)))
    }

    /// Add a `(name -> child)` entry to directory `dir_ino` (direct blocks only).
    fn dir_insert(&self, dir_ino: u32, name: &str, child: u32, is_dir: bool) -> Result<(), &'static str> {
        let ft: u8 = if !self.filetype {
            0
        } else if is_dir {
            2
        } else {
            1
        };
        let need = round4(8 + name.len());
        let bs = self.block_size as usize;
        let dir = self.read_inode(dir_ino);

        for slot in 0..12 {
            let bno = dir.block[slot];
            if bno == 0 {
                continue;
            }
            let mut blk = self.block(bno);
            let mut off = 0;
            while off + 8 <= bs {
                let ino = le32(&blk[off..]);
                let rec_len = le16(&blk[off + 4..]) as usize;
                let name_len = blk[off + 6] as usize;
                if rec_len == 0 || off + rec_len > bs {
                    break;
                }
                let used = if ino == 0 { 0 } else { round4(8 + name_len) };
                if rec_len - used >= need {
                    let no = off + used;
                    let nrec = rec_len - used;
                    if used != 0 {
                        blk[off + 4..off + 6].copy_from_slice(&(used as u16).to_le_bytes());
                    }
                    blk[no..no + 4].copy_from_slice(&child.to_le_bytes());
                    blk[no + 4..no + 6].copy_from_slice(&(nrec as u16).to_le_bytes());
                    blk[no + 6] = name.len() as u8;
                    blk[no + 7] = ft;
                    blk[no + 8..no + 8 + name.len()].copy_from_slice(name.as_bytes());
                    self.write_block(bno, &blk);
                    return Ok(());
                }
                off += rec_len;
            }
        }

        // No gap anywhere — hang a fresh block off the next free direct slot.
        let slot = (0..12).find(|&s| dir.block[s] == 0).ok_or("directory full")?;
        let bno = self.alloc_block().ok_or("no free block for dir")?;
        let mut blk = vec![0u8; bs];
        blk[0..4].copy_from_slice(&child.to_le_bytes());
        blk[4..6].copy_from_slice(&(bs as u16).to_le_bytes());
        blk[6] = name.len() as u8;
        blk[7] = ft;
        blk[8..8 + name.len()].copy_from_slice(name.as_bytes());
        self.write_block(bno, &blk);

        let mut nb = dir.block;
        nb[slot] = bno;
        let add512 = self.block_size / 512;
        self.patch_inode(dir_ino, |raw| {
            let size = le32(&raw[4..]) + bs as u32;
            raw[4..8].copy_from_slice(&size.to_le_bytes());
            let blk512 = le32(&raw[28..]) + add512;
            raw[28..32].copy_from_slice(&blk512.to_le_bytes());
            for (i, b) in nb.iter().enumerate() {
                raw[40 + i * 4..44 + i * 4].copy_from_slice(&b.to_le_bytes());
            }
        });
        Ok(())
    }

    /// Create `path` (or overwrite it) as a regular file holding `data`. The
    /// parent directory must already exist.
    pub fn write_path(&self, path: &str, data: &[u8]) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path).ok_or("bad path")?;
        let parent_ino = self.path_lookup(parent).ok_or("parent dir missing")?;
        if name.len() > 255 {
            return Err("name too long");
        }
        let (block, blocks512) = self.lay_out_data(data).ok_or("no space / file too large")?;

        match self.lookup(parent_ino, name) {
            Some(ino) => {
                let old = self.read_inode(ino);
                self.free_all_blocks(&old);
                let mode = old.mode;
                self.patch_inode(ino, |raw| {
                    let links = le16(&raw[26..]);
                    set_inode(raw, mode, data.len() as u64, links, blocks512, &block);
                });
                Ok(())
            }
            None => {
                let ino = self.alloc_inode(false).ok_or("no free inode")?;
                self.patch_inode(ino, |raw| {
                    set_inode(raw, 0o100_644, data.len() as u64, 1, blocks512, &block);
                });
                self.dir_insert(parent_ino, name, ino, false)
            }
        }
    }

    /// Create directory `path`. The parent must exist; `path` must not.
    pub fn mkdir_path(&self, path: &str) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path).ok_or("bad path")?;
        let parent_ino = self.path_lookup(parent).ok_or("parent dir missing")?;
        if self.lookup(parent_ino, name).is_some() {
            return Err("already exists");
        }
        let ino = self.alloc_inode(true).ok_or("no free inode")?;
        let bno = self.alloc_block().ok_or("no free block")?;
        let bs = self.block_size as usize;
        let dft: u8 = if self.filetype { 2 } else { 0 };

        let mut blk = vec![0u8; bs];
        blk[0..4].copy_from_slice(&ino.to_le_bytes()); // "."
        blk[4..6].copy_from_slice(&12u16.to_le_bytes());
        blk[6] = 1;
        blk[7] = dft;
        blk[8] = b'.';
        blk[12..16].copy_from_slice(&parent_ino.to_le_bytes()); // ".."
        blk[16..18].copy_from_slice(&((bs - 12) as u16).to_le_bytes());
        blk[18] = 2;
        blk[19] = dft;
        blk[20] = b'.';
        blk[21] = b'.';
        self.write_block(bno, &blk);

        let mut block = [0u32; 15];
        block[0] = bno;
        self.patch_inode(ino, |raw| {
            set_inode(raw, 0o040_755, bs as u64, 2, self.block_size / 512, &block);
        });
        self.dir_insert(parent_ino, name, ino, true)?;
        self.bump_links(parent_ino, 1); // the child's ".."
        Ok(())
    }
}
