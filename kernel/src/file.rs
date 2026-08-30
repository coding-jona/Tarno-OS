// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — open files behind a descriptor.
//!
//! A tiny `FileOps` trait with two implementations for now: `ConsoleFile`
//! (stdin/stdout/stderr over the serial console) and `MemFile` (a whole file
//! slurped into memory — our ext2 is read-only, so this is enough for `cat`).
//! Pipes, real streaming ext2, `/dev`, sockets all slot in here later.

use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// errno values the syscall layer also uses.
const EBADF: i64 = -9;
const ESPIPE: i64 = -29;
const EINVAL: i64 = -22;

pub trait FileOps: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> i64;
    fn write(&self, buf: &[u8]) -> i64;
    fn seek(&self, offset: i64, whence: u32) -> i64;
    /// (mode bits for `st_mode`, size for `st_size`).
    fn stat(&self) -> (u32, u64);
}

// --- console ---

pub struct ConsoleFile {
    pub writable: bool,
}

const S_IFCHR: u32 = 0o020000;
const S_IFREG: u32 = 0o100000;

impl FileOps for ConsoleFile {
    fn read(&self, _buf: &mut [u8]) -> i64 {
        0 // EOF on stdin for now
    }
    fn write(&self, buf: &[u8]) -> i64 {
        if !self.writable {
            return EBADF;
        }
        crate::serial::write_bytes(buf);
        buf.len() as i64
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        ESPIPE
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFCHR | 0o620, 0)
    }
}

// --- in-memory regular file ---

pub struct MemFile {
    data: Vec<u8>,
    pos: Mutex<usize>,
}

impl MemFile {
    pub fn new(data: Vec<u8>) -> Arc<Self> {
        Arc::new(Self { data, pos: Mutex::new(0) })
    }
}

impl FileOps for MemFile {
    fn read(&self, buf: &mut [u8]) -> i64 {
        let mut pos = self.pos.lock();
        if *pos >= self.data.len() {
            return 0;
        }
        let n = buf.len().min(self.data.len() - *pos);
        buf[..n].copy_from_slice(&self.data[*pos..*pos + n]);
        *pos += n;
        n as i64
    }
    fn write(&self, _buf: &[u8]) -> i64 {
        EBADF // read-only
    }
    fn seek(&self, offset: i64, whence: u32) -> i64 {
        let mut pos = self.pos.lock();
        let base = match whence {
            SEEK_SET => 0i64,
            SEEK_CUR => *pos as i64,
            SEEK_END => self.data.len() as i64,
            _ => return EINVAL,
        };
        let np = base + offset;
        if np < 0 {
            return EINVAL;
        }
        *pos = np as usize;
        np
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFREG | 0o644, self.data.len() as u64)
    }
}
