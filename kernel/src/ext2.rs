// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — ext2 (read + basic write).
//!
//! Mount the SATA disk, walk a path from the root inode, read a file's data
//! blocks (12 direct + single + double indirect). Write side: block / inode
//! bitmap allocators, `write_path` (create-or-overwrite a regular file),
//! `mkdir_path`, `unlink_path`, `rmdir_path`; the backup superblock + group
//! descriptors are re-synced from the primary after each change (`sparse_super`
//! honoured) so `e2fsck` stays clean on a multi-group filesystem. Still missing:
//! htree dirs, growing a directory past 12 direct blocks, 64-bit sizes,
//! journalling, timestamps, hard links.

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
    /// `s_feature_ro_compat & SPARSE_SUPER` — backup SB/GDT only in groups
    /// 0, 1, and powers of 3/5/7, not every group.
    sparse_super: bool,
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
        let n = (sectors - done).min(64);
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
        filetype: le32(&sb[96..]) & 0x0002 != 0,      // FEATURE_INCOMPAT_FILETYPE
        sparse_super: le32(&sb[100..]) & 0x0001 != 0, // FEATURE_RO_COMPAT_SPARSE_SUPER
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
        let n = (sectors - done).min(64);
        ahci::read(start_lba + done as u64, &mut raw[done * SECTOR..(done + n) * SECTOR])
            .expect("ext2 rmw read");
        done += n;
    }

    let skip = (off - start_lba * SECTOR as u64) as usize;
    raw[skip..skip + data.len()].copy_from_slice(data);

    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(64);
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

    /// Every data-block number of `inode` in file order, capped at the file's
    /// block count. `0` = a sparse hole.
    fn block_map(&self, inode: &Inode) -> Vec<u32> {
        let bs = self.block_size as usize;
        let per = bs / 4;
        let nblocks = (inode.size as usize).div_ceil(bs).max(1);
        let mut out: Vec<u32> = Vec::with_capacity(nblocks);

        out.extend_from_slice(&inode.block[..12.min(nblocks)]);

        // one single-indirect block's worth of pointers (or a run of holes)
        let single = |bn: u32, out: &mut Vec<u32>| {
            if out.len() >= nblocks {
                return;
            }
            let take = per.min(nblocks - out.len());
            if bn == 0 {
                out.resize(out.len() + take, 0);
            } else {
                let ind = self.block(bn);
                out.extend(ind.chunks_exact(4).take(take).map(le32));
            }
        };

        if nblocks > 12 {
            single(inode.block[12], &mut out);
        }
        if out.len() < nblocks {
            if inode.block[13] == 0 {
                for _ in 0..per {
                    if out.len() >= nblocks {
                        break;
                    }
                    single(0, &mut out);
                }
            } else {
                for dc in self.block(inode.block[13]).chunks_exact(4) {
                    if out.len() >= nblocks {
                        break;
                    }
                    single(le32(dc), &mut out);
                }
            }
        }
        out
    }

    pub fn read_file(&self, inode: &Inode) -> Vec<u8> {
        let total = inode.size as usize;
        let bs = self.block_size as usize;
        let blocks = self.block_map(inode);

        // Emit consecutive runs of block numbers in one disk read each.
        let mut out = Vec::with_capacity(total);
        let mut i = 0;
        while i < blocks.len() {
            if blocks[i] == 0 {
                let j = blocks[i..].iter().take_while(|&&b| b == 0).count() + i;
                out.resize(out.len() + (j - i) * bs, 0);
                i = j;
            } else {
                let mut j = i + 1;
                while j < blocks.len() && blocks[j] == blocks[j - 1] + 1 {
                    j += 1;
                }
                out.extend_from_slice(&disk_range(blocks[i] as u64 * bs as u64, (j - i) * bs));
                i = j;
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
            }
            None => {
                let ino = self.alloc_inode(false).ok_or("no free inode")?;
                self.patch_inode(ino, |raw| {
                    set_inode(raw, 0o100_644, data.len() as u64, 1, blocks512, &block);
                });
                self.dir_insert(parent_ino, name, ino, false)?;
            }
        }
        self.sync_backups();
        Ok(())
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
        self.sync_backups();
        Ok(())
    }

    /// Remove `name` from directory `dir_ino`: splice its record out (merge into
    /// the previous entry, or tombstone it with `inode = 0` if it is first in
    /// its block). Returns the child inode number.
    fn dir_remove(&self, dir_ino: u32, name: &str) -> Option<u32> {
        let bs = self.block_size as usize;
        let dir = self.read_inode(dir_ino);
        for slot in 0..12 {
            let bno = dir.block[slot];
            if bno == 0 {
                continue;
            }
            let mut blk = self.block(bno);
            let mut off = 0;
            let mut prev: Option<usize> = None;
            while off + 8 <= bs {
                let ino = le32(&blk[off..]);
                let rec_len = le16(&blk[off + 4..]) as usize;
                let name_len = blk[off + 6] as usize;
                if rec_len == 0 || off + rec_len > bs {
                    break;
                }
                if ino != 0
                    && name_len == name.len()
                    && &blk[off + 8..off + 8 + name_len] == name.as_bytes()
                {
                    match prev {
                        Some(p) => {
                            let merged = le16(&blk[p + 4..]) as usize + rec_len;
                            blk[p + 4..p + 6].copy_from_slice(&(merged as u16).to_le_bytes());
                        }
                        None => blk[off..off + 4].copy_from_slice(&0u32.to_le_bytes()),
                    }
                    self.write_block(bno, &blk);
                    return Some(ino);
                }
                prev = Some(off);
                off += rec_len;
            }
        }
        None
    }

    /// Mark an inode free in the bitmap + counts, and stamp it deleted.
    fn free_inode(&self, ino: u32, is_dir: bool) {
        let g = (ino - 1) / self.inodes_per_group;
        let i = ((ino - 1) % self.inodes_per_group) as usize;
        let bmb = le32(&self.read_bgd(g)[4..]);
        let mut bm = self.block(bmb);
        if bm[i / 8] & (1 << (i % 8)) != 0 {
            bm[i / 8] &= !(1 << (i % 8));
            self.write_block(bmb, &bm);
            self.bgd_add16(g, 14, 1); // free inodes ++
            self.sb_add32(16, 1);
            if is_dir {
                self.bgd_add16(g, 16, -1); // used dirs --
            }
        }
        self.patch_inode(ino, |raw| {
            raw[26..28].copy_from_slice(&0u16.to_le_bytes()); // i_links_count
            // i_dtime: a plausible timestamp (2025-01-01). A tiny value like 1
            // is mistaken by e2fsck for an orphan-list "next inode" pointer.
            raw[20..24].copy_from_slice(&1_735_689_600u32.to_le_bytes());
            raw[28..32].copy_from_slice(&0u32.to_le_bytes()); // i_blocks
        });
    }

    /// Unlink a regular file: drop its last link and free it.
    pub fn unlink_path(&self, path: &str) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path).ok_or("bad path")?;
        let parent_ino = self.path_lookup(parent).ok_or("parent dir missing")?;
        let ino = self.lookup(parent_ino, name).ok_or("no such file")?;
        let node = self.read_inode(ino);
        if node.mode & 0xF000 == 0x4000 {
            return Err("is a directory");
        }
        self.dir_remove(parent_ino, name).ok_or("dirent vanished")?;
        let links = le16(&disk_range(self.inode_off(ino) + 26, 2)); // i_links_count @ 26
        if links <= 1 {
            self.free_all_blocks(&node);
            self.free_inode(ino, false);
        } else {
            self.bump_links(ino, -1);
        }
        self.sync_backups();
        Ok(())
    }

    /// Remove an empty directory.
    pub fn rmdir_path(&self, path: &str) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path).ok_or("bad path")?;
        let parent_ino = self.path_lookup(parent).ok_or("parent dir missing")?;
        let ino = self.lookup(parent_ino, name).ok_or("no such directory")?;
        let node = self.read_inode(ino);
        if node.mode & 0xF000 != 0x4000 {
            return Err("not a directory");
        }
        let data = self.read_file(&node);
        let mut off = 0;
        while off + 8 <= data.len() {
            let e_ino = le32(&data[off..]);
            let rl = le16(&data[off + 4..]) as usize;
            let nl = data[off + 6] as usize;
            if rl == 0 {
                break;
            }
            let nm = &data[off + 8..(off + 8 + nl).min(data.len())];
            if e_ino != 0 && nm != b"." && nm != b".." {
                return Err("directory not empty");
            }
            off += rl;
        }
        self.dir_remove(parent_ino, name).ok_or("dirent vanished")?;
        self.free_all_blocks(&node);
        self.free_inode(ino, true);
        self.bump_links(parent_ino, -1); // the child's ".." is gone
        self.sync_backups();
        Ok(())
    }

    /// Does group `g` (>= 1) hold a backup superblock + GDT? (`sparse_super`.)
    fn has_backup(&self, g: u32) -> bool {
        if !self.sparse_super || g <= 1 {
            return true;
        }
        [3u32, 5, 7].iter().any(|&base| {
            let mut p = base;
            while p < g {
                match p.checked_mul(base) {
                    Some(v) => p = v,
                    None => return false,
                }
            }
            p == g
        })
    }

    /// Re-write every backup superblock + group-descriptor table from the
    /// primary, so `e2fsck` stays happy after a mutation on a multi-group fs.
    fn sync_backups(&self) {
        let groups = self.group_count();
        if groups <= 1 {
            return;
        }
        let sb = disk_range(SB_OFFSET, 1024);
        let gdt = disk_range(self.bgd_off(0), groups as usize * 32);
        for g in 1..groups {
            if !self.has_backup(g) {
                continue;
            }
            let sb_blk = self.first_data_block + g * self.blocks_per_group;
            let mut sbc = sb.clone();
            sbc[90..92].copy_from_slice(&(g as u16).to_le_bytes()); // s_block_group_nr
            disk_write(sb_blk as u64 * self.block_size as u64, &sbc);
            disk_write((sb_blk + 1) as u64 * self.block_size as u64, &gdt);
        }
    }
}
