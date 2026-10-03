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
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::ahci::{self, SECTOR};

const SB_OFFSET: u64 = 1024;

/// Disk LBA where the ext2 filesystem starts: 0 for a bare-image disk (the QEMU
/// test disk), the partition's first sector on a real MBR disk.
static PART_LBA: AtomicU64 = AtomicU64::new(0);
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

pub struct Inode {
    pub mode: u16,
    pub size: u64,
    pub block: [u32; 15],
    /// `i_atime` / `i_ctime` / `i_mtime`, seconds since the epoch.
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    /// `i_uid`/`i_gid` — the classic 16-bit ext2 fields (not the Linux
    /// high-16-bits-in-`i_osd2` extension; THOS's own uid space is small
    /// enough that this doesn't matter yet). Every file THOS itself creates
    /// today (registry hives, the credential store, the integrity baseline,
    /// the write-path test fixtures) goes through `write_path`/`mkdir_path`
    /// without an explicit owner, so it's `0` — conceptually the system
    /// account, matching real Unix's read of an unset owner, and consistent
    /// with THOS having no interactive root login to actually confuse this
    /// with (see `cred.rs`).
    pub uid: u32,
    pub gid: u32,
}

impl Inode {
    /// The DAC permission check every file open goes through: real Unix
    /// owner/group/other bits, three tiers. THOS's identity model has no
    /// supplementary groups (yet) — every task carries exactly one primary
    /// gid, the "user private group" scheme (gid == uid, same convention
    /// `cred::save` already uses naming `/home/<name>`'s owner) — so the
    /// group tier here is "does the caller's single primary gid match the
    /// file's gid", not a membership-list lookup. uid `0` (the system
    /// account) always passes, matching real Unix root semantics —
    /// consistent with THOS having no interactive root login to actually
    /// confuse this with (`cred.rs`).
    pub fn access_ok(&self, uid: u32, gid: u32, want_write: bool) -> bool {
        if uid == 0 {
            return true;
        }
        let perm = self.mode & 0o777;
        let bits = if uid == self.uid {
            (perm >> 6) & 0o7
        } else if gid == self.gid {
            (perm >> 3) & 0o7
        } else {
            perm & 0o7
        };
        let need = if want_write { 0o2 } else { 0o4 };
        bits & need == need
    }
}

/// Read an arbitrary byte range off the disk (sector-granular under the hood).
fn disk_range(off: u64, len: usize) -> Vec<u8> {
    let start_lba = off / SECTOR as u64;
    let end_lba = (off + len as u64 + SECTOR as u64 - 1) / SECTOR as u64;
    let sectors = (end_lba - start_lba) as usize;
    let part = PART_LBA.load(Ordering::Relaxed);
    let mut raw = vec![0u8; sectors * SECTOR];
    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(64);
        ahci::read(part + start_lba + done as u64, &mut raw[done * SECTOR..(done + n) * SECTOR])
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

/// True when the root FS was found inside an MBR partition (a real disk), not a
/// bare QEMU test image. Fixed-LBA scratch tests must not run on such a disk.
pub fn on_partition() -> bool {
    PART_LBA.load(Ordering::Relaxed) != 0
}

pub fn open() -> Result<Ext2, &'static str> {
    let mut sb = disk_range(SB_OFFSET, 1024);
    if le16(&sb[56..]) != EXT2_MAGIC {
        // Not a bare-image disk: look for a Linux partition in the MBR.
        let found = crate::mbr::entries()
            .into_iter()
            .filter(|e| e.ptype == crate::mbr::TYPE_LINUX)
            .find(|e| {
                PART_LBA.store(e.start_lba, Ordering::Relaxed);
                le16(&disk_range(SB_OFFSET, 1024)[56..]) == EXT2_MAGIC
            });
        match found {
            Some(e) => {
                crate::kprintln!("THOS: ext2 part        root FS in MBR partition at LBA {} ({} MiB)", e.start_lba, e.sectors / 2048);
                sb = disk_range(SB_OFFSET, 1024);
            }
            None => {
                PART_LBA.store(0, Ordering::Relaxed);
                return Err("ext2 magic not found");
            }
        }
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
    let part = PART_LBA.load(Ordering::Relaxed);
    let mut raw = vec![0u8; sectors * SECTOR];

    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(64);
        ahci::read(part + start_lba + done as u64, &mut raw[done * SECTOR..(done + n) * SECTOR])
            .expect("ext2 rmw read");
        done += n;
    }

    let skip = (off - start_lba * SECTOR as u64) as usize;
    raw[skip..skip + data.len()].copy_from_slice(data);

    let mut done = 0;
    while done < sectors {
        let n = (sectors - done).min(64);
        ahci::write(part + start_lba + done as u64, &raw[done * SECTOR..(done + n) * SECTOR])
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
#[allow(clippy::too_many_arguments)]
fn set_inode(
    raw: &mut [u8],
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
    links: u16,
    blocks512: u32,
    block: &[u32; 15],
) {
    raw[0..2].copy_from_slice(&mode.to_le_bytes());
    raw[2..4].copy_from_slice(&(uid as u16).to_le_bytes());
    raw[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    raw[24..26].copy_from_slice(&(gid as u16).to_le_bytes());
    raw[26..28].copy_from_slice(&links.to_le_bytes());
    raw[28..32].copy_from_slice(&blocks512.to_le_bytes()); // i_blocks (512-byte units)
    // Timestamps: every inode write through here is a create or a content rewrite.
    let now = (crate::timer::unix_secs() as u32).to_le_bytes();
    raw[8..12].copy_from_slice(&now); // i_atime
    raw[12..16].copy_from_slice(&now); // i_ctime
    raw[16..20].copy_from_slice(&now); // i_mtime
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
            uid: le16(&raw[2..]) as u32,
            gid: le16(&raw[24..]) as u32,
            atime: le32(&raw[8..]),
            ctime: le32(&raw[12..]),
            mtime: le32(&raw[16..]),
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

    /// The block numbers of a file (0 = hole), for [`Ext2::read_at`].
    pub fn file_blocks(&self, inode: &Inode) -> Vec<u32> {
        self.block_map(inode)
    }

    /// Read up to `out.len()` bytes at byte offset `off` of a file of `size` bytes whose
    /// block map is `blocks` — only the blocks that are needed, in runs of consecutive disk
    /// blocks (at most 64 per disk read). Returns the number of bytes read (0 at EOF).
    pub fn read_at(&self, blocks: &[u32], size: u64, off: u64, out: &mut [u8]) -> usize {
        if off >= size {
            return 0;
        }
        let n = out.len().min((size - off) as usize);
        let bs = self.block_size as u64;
        let mut done = 0usize;
        while done < n {
            let pos = off + done as u64;
            let bi = (pos / bs) as usize;
            let in_off = (pos % bs) as usize;
            let remaining = n - done;
            if bi >= blocks.len() || blocks[bi] == 0 {
                // a hole (or past the map): reads as zeros up to the end of this block
                let take = (bs as usize - in_off).min(remaining);
                out[done..done + take].fill(0);
                done += take;
                continue;
            }
            let mut j = bi + 1;
            while j < blocks.len()
                && j - bi < 64
                && blocks[j] == blocks[j - 1] + 1
                && (j - bi) * (bs as usize) < in_off + remaining
            {
                j += 1;
            }
            let take = ((j - bi) * bs as usize - in_off).min(remaining);
            let data = disk_range(blocks[bi] as u64 * bs + in_off as u64, take);
            out[done..done + take].copy_from_slice(&data);
            done += take;
        }
        n
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

    /// Every live entry of a directory as `(inode, d_type, name)` with `d_type`
    /// already translated to the Linux `DT_*` namespace (`getdents64`). Falls
    /// back to the entry inode's `i_mode` when the on-disk filetype byte is 0.
    pub fn read_dir(&self, dir_ino: u32) -> Vec<(u32, u8, alloc::string::String)> {
        let data = self.read_file(&self.read_inode(dir_ino));
        let mut out = Vec::new();
        let mut off = 0;
        while off + 8 <= data.len() {
            let ino = le32(&data[off..]);
            let rec_len = le16(&data[off + 4..]) as usize;
            let name_len = data[off + 6] as usize;
            let ftype = data[off + 7];
            if rec_len == 0 {
                break;
            }
            if ino != 0 && off + 8 + name_len <= data.len() {
                if let Ok(name) = core::str::from_utf8(&data[off + 8..off + 8 + name_len]) {
                    let dt = match ftype {
                        1 => 8,  // reg
                        2 => 4,  // dir
                        3 => 2,  // chr
                        4 => 6,  // blk
                        5 => 1,  // fifo
                        6 => 12, // sock
                        7 => 10, // symlink
                        _ => (self.read_inode(ino).mode >> 12) as u8,
                    };
                    out.push((ino, dt, name.into()));
                }
            }
            off += rec_len;
        }
        out
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

    /// Start a batched block-allocation transaction (see [`BlockTx`]).
    fn tx(&self) -> BlockTx<'_> {
        BlockTx { fs: self, bitmaps: BTreeMap::new(), free: BTreeMap::new(), delta: BTreeMap::new() }
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
        let mut tx = self.tx();
        for &b in &node.block[..12] {
            if b != 0 {
                tx.release(b);
            }
        }
        if node.block[12] != 0 {
            for c in self.block(node.block[12]).chunks_exact(4) {
                if le32(c) != 0 {
                    tx.release(le32(c));
                }
            }
            tx.release(node.block[12]);
        }
        if node.block[13] != 0 {
            for c in self.block(node.block[13]).chunks_exact(4) {
                let sib = le32(c);
                if sib == 0 {
                    continue;
                }
                for d in self.block(sib).chunks_exact(4) {
                    if le32(d) != 0 {
                        tx.release(le32(d));
                    }
                }
                tx.release(sib);
            }
            tx.release(node.block[13]);
        }
        tx.commit();
    }

    /// Allocate + fill data blocks for `data`. Returns `(block[15], i_blocks)`
    /// where `i_blocks` counts data + indirect blocks in 512-byte units.
    fn lay_out_data(&self, data: &[u8]) -> Option<([u32; 15], u32)> {
        let bs = self.block_size as usize;
        let per = bs / 4; // pointers per indirect block
        let nblocks = data.len().div_ceil(bs);
        let mut tx = self.tx();
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
            let b = tx.alloc()?;
            self.write_block(b, &chunk(bi));
            block[bi] = b;
            bi += 1;
        }

        if bi < nblocks {
            let ind = tx.alloc()?;
            meta += 1;
            let mut buf = vec![0u8; bs];
            let mut k = 0;
            while bi < nblocks && k < per {
                let b = tx.alloc()?;
                self.write_block(b, &chunk(bi));
                buf[k * 4..k * 4 + 4].copy_from_slice(&b.to_le_bytes());
                bi += 1;
                k += 1;
            }
            self.write_block(ind, &buf);
            block[12] = ind;
        }

        if bi < nblocks {
            let dind = tx.alloc()?;
            meta += 1;
            let mut dbuf = vec![0u8; bs];
            let mut j = 0;
            while bi < nblocks && j < per {
                let ind = tx.alloc()?;
                meta += 1;
                let mut buf = vec![0u8; bs];
                let mut k = 0;
                while bi < nblocks && k < per {
                    let b = tx.alloc()?;
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
        tx.commit();
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

    /// Create `path` (or overwrite it) as a regular file holding `data`,
    /// owned by the system account (`0`/`0`) — every existing kernel-internal
    /// caller (registry hives, credential store, integrity baseline, the
    /// ext2 write tests) wants exactly that, unchanged. [`Self::write_path_owned`]
    /// is the version a real syscall-driven creation (`open(O_CREAT, ...)`)
    /// goes through instead, to give the creating task's own uid.
    pub fn write_path(&self, path: &str, data: &[u8]) -> Result<(), &'static str> {
        self.write_path_owned(path, data, 0, 0)
    }

    /// [`Self::write_path`], but a newly *created* file is owned by
    /// `uid`/`gid` instead of always `0`/`0`. Overwriting an *existing* file
    /// never changes its owner — matches real Unix: truncating a file you
    /// have write access to doesn't let you take it over.
    pub fn write_path_owned(&self, path: &str, data: &[u8], uid: u32, gid: u32) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path).ok_or("bad path")?;
        let parent_ino = self.path_lookup(parent).ok_or("parent dir missing")?;
        if name.len() > 255 {
            return Err("name too long");
        }
        let (block, blocks512) = self.lay_out_data(data).ok_or("no space / file too large")?;

        match self.lookup(parent_ino, name) {
            Some(ino) => {
                let old = self.read_inode(ino);
                let (mode, old_uid, old_gid) = (old.mode, old.uid, old.gid);
                // Commit the new blocks *before* freeing the old ones — the
                // inode patch below is a single-sector write (one ext2 inode
                // never straddles a sector: `inode_size` divides `SECTOR`
                // evenly, and offsets are inode-aligned), so it's the one
                // truly atomic step in this whole operation: a crash before
                // it, the file is still exactly its old self; a crash after
                // it, exactly its new self. Freeing first (the old order)
                // opened a real corruption window — a crash between "mark
                // old blocks free" and "repoint the inode" left the bitmap
                // and the inode disagreeing about who owns those blocks,
                // not just "lost this write". The only residual cost of the
                // new order is a leaked (never-freed) set of blocks if a
                // crash lands between the two writes below — recoverable by
                // fsck, not corruption.
                self.patch_inode(ino, |raw| {
                    let links = le16(&raw[26..]);
                    set_inode(raw, mode, old_uid, old_gid, data.len() as u64, links, blocks512, &block);
                });
                #[cfg(feature = "regcrashtest")]
                if path == "/regcrash-test.bin" {
                    crate::kprintln!(
                        "THOS: regcrash         simulating a crash: inode committed, blocks not yet freed"
                    );
                    crate::exit_qemu(crate::ExitCode::Success);
                    crate::hcf();
                }
                self.free_all_blocks(&old);
            }
            None => {
                let ino = self.alloc_inode(false).ok_or("no free inode")?;
                self.patch_inode(ino, |raw| {
                    set_inode(raw, 0o100_644, uid, gid, data.len() as u64, 1, blocks512, &block);
                });
                self.dir_insert(parent_ino, name, ino, false)?;
            }
        }
        self.sync_backups();
        Ok(())
    }

    /// [`Self::mkdir_path`], owned by the system account — see
    /// `write_path`/`write_path_owned`'s split for why.
    pub fn mkdir_path(&self, path: &str) -> Result<(), &'static str> {
        self.mkdir_path_owned(path, 0, 0)
    }

    /// Create directory `path`, owned by `uid`/`gid`. The parent must
    /// exist; `path` must not.
    pub fn mkdir_path_owned(&self, path: &str, uid: u32, gid: u32) -> Result<(), &'static str> {
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
            set_inode(raw, 0o040_755, uid, gid, bs as u64, 2, self.block_size / 512, &block);
        });
        self.dir_insert(parent_ino, name, ino, true)?;
        self.bump_links(parent_ino, 1); // the child's ".."
        self.sync_backups();
        Ok(())
    }

    /// `chmod(2)`: replace `path`'s permission bits (the low 12 bits —
    /// permissions plus setuid/setgid/sticky) with `perm`, leaving the file
    /// type (the upper nibble `read_inode`/`Inode::access_ok` key off) alone.
    /// The caller (`syscall::sys_chmod`) is responsible for the "only the
    /// owner or root may do this" check — this just writes the bits.
    pub fn chmod_path(&self, path: &str, perm: u16) -> Result<(), &'static str> {
        let ino = self.path_lookup(path).ok_or("no such file")?;
        let file_type = self.read_inode(ino).mode & 0xF000;
        self.patch_inode(ino, |raw| {
            let mode = file_type | (perm & 0o7777);
            raw[0..2].copy_from_slice(&mode.to_le_bytes());
        });
        Ok(())
    }

    /// `chown(2)`: replace `path`'s owner/group. Same split as `chmod_path`
    /// — the permission check (real `chown` is stricter: owner alone isn't
    /// enough, it's root-only, matching modern Unix) lives in the caller.
    pub fn chown_path(&self, path: &str, uid: u32, gid: u32) -> Result<(), &'static str> {
        let ino = self.path_lookup(path).ok_or("no such file")?;
        self.patch_inode(ino, |raw| {
            raw[2..4].copy_from_slice(&(uid as u16).to_le_bytes());
            raw[24..26].copy_from_slice(&(gid as u16).to_le_bytes());
        });
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


/// A batch of block allocations / frees. The bitmaps and free counters each group needs are
/// read **once**, edited in memory and written **once** at [`BlockTx::commit`] — instead of
/// a read-modify-write of the bitmap, the group descriptor and the superblock for every single
/// block, which made writing a megabyte take dozens of seconds. Dropping the transaction
/// without committing leaves the disk's bitmaps untouched (blocks a failed allocation had
/// picked simply stay free).
struct BlockTx<'a> {
    fs: &'a Ext2,
    /// group -> (bitmap block number, bitmap bytes, modified)
    bitmaps: BTreeMap<u32, (u32, Vec<u8>, bool)>,
    /// group -> free-block count as read from its descriptor
    free: BTreeMap<u32, i64>,
    /// group -> change in free blocks to write back
    delta: BTreeMap<u32, i64>,
}

impl BlockTx<'_> {
    fn load(&mut self, g: u32) {
        if !self.bitmaps.contains_key(&g) {
            let bgd = self.fs.read_bgd(g);
            let bmb = le32(&bgd[0..]);
            self.free.insert(g, le16(&bgd[12..]) as i64);
            self.bitmaps.insert(g, (bmb, self.fs.block(bmb), false));
        }
    }

    /// The first free block (first-fit over the groups), marked used in the cached bitmap.
    fn alloc(&mut self) -> Option<u32> {
        let fs = self.fs;
        let groups = fs.group_count();
        for g in 0..groups {
            self.load(g);
            if self.free[&g] + self.delta.get(&g).copied().unwrap_or(0) <= 0 {
                continue;
            }
            let in_group = if g == groups - 1 {
                fs.block_count - fs.first_data_block - g * fs.blocks_per_group
            } else {
                fs.blocks_per_group
            };
            let bm = self.bitmaps.get_mut(&g).unwrap();
            for i in 0..in_group as usize {
                if bm.1[i / 8] & (1 << (i % 8)) == 0 {
                    bm.1[i / 8] |= 1 << (i % 8);
                    bm.2 = true;
                    *self.delta.entry(g).or_insert(0) -= 1;
                    return Some(fs.first_data_block + g * fs.blocks_per_group + i as u32);
                }
            }
        }
        None
    }

    /// Mark a block free (a no-op if its bit is already clear).
    fn release(&mut self, bno: u32) {
        let rel = bno - self.fs.first_data_block;
        let g = rel / self.fs.blocks_per_group;
        let i = (rel % self.fs.blocks_per_group) as usize;
        self.load(g);
        let bm = self.bitmaps.get_mut(&g).unwrap();
        if bm.1[i / 8] & (1 << (i % 8)) != 0 {
            bm.1[i / 8] &= !(1 << (i % 8));
            bm.2 = true;
            *self.delta.entry(g).or_insert(0) += 1;
        }
    }

    /// Write the changed bitmaps and counters.
    fn commit(self) {
        let mut total = 0i64;
        for (g, (bmb, bytes, dirty)) in &self.bitmaps {
            if *dirty {
                self.fs.write_block(*bmb, bytes);
            }
            let d = self.delta.get(g).copied().unwrap_or(0);
            if d != 0 {
                self.fs.bgd_add16(*g, 12, d);
                total += d;
            }
        }
        if total != 0 {
            self.fs.sb_add32(12, total);
        }
    }
}
