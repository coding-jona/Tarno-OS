// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — read-only ext2.
//!
//! Just enough to mount the SATA disk, walk a path from the root inode, and
//! read a file's data blocks (12 direct + single indirect). Writes, the block
//! group allocator, htree dirs, 64-bit sizes, and journalling come later; this
//! is here to load an ELF off disk.

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
    })
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
}
