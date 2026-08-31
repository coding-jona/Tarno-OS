// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — open files behind a descriptor.
//!
//! A tiny `FileOps` trait with two implementations for now: `ConsoleFile`
//! (stdin/stdout/stderr over the serial console) and `MemFile` (a whole file
//! slurped into memory — our ext2 is read-only, so this is enough for `cat`).
//! Pipes, real streaming ext2, `/dev`, sockets all slot in here later.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use spin::Mutex;

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// errno values the syscall layer also uses.
const EBADF: i64 = -9;
const ESPIPE: i64 = -29;
const EINVAL: i64 = -22;
const EISDIR: i64 = -21;
const ENOTDIR: i64 = -20;
const EPIPE: i64 = -32;

pub trait FileOps: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> i64;
    fn write(&self, buf: &[u8]) -> i64;
    fn seek(&self, offset: i64, whence: u32) -> i64;
    /// (mode bits for `st_mode`, size for `st_size`).
    fn stat(&self) -> (u32, u64);
    /// Fill `buf` with `struct linux_dirent64` records; `0` at end-of-dir,
    /// `-EINVAL` if `buf` is too small for even one record. Not a directory by
    /// default.
    fn getdents64(&self, _buf: &mut [u8]) -> i64 {
        ENOTDIR
    }
}

// --- console ---

pub struct ConsoleFile {
    pub writable: bool,
}

const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

/// stdin backed by the keyboard console: a blocking line read.
pub struct KeyboardFile;

impl FileOps for KeyboardFile {
    fn read(&self, buf: &mut [u8]) -> i64 {
        loop {
            let n = crate::console::read(buf);
            if n > 0 {
                return n as i64;
            }
            crate::console::wait_for_input();
        }
    }
    fn write(&self, _buf: &[u8]) -> i64 {
        EBADF
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        ESPIPE
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFCHR | 0o620, 0)
    }
}

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

// --- directory: a pre-rendered `getdents64` blob ---

/// The 19-byte fixed head of `struct linux_dirent64`
/// (`d_ino`, `d_off`, `d_reclen`, `d_type`), before the NUL-terminated name.
const DIRENT_HEAD: usize = 8 + 8 + 2 + 1;

pub struct DirFile {
    /// Back-to-back `linux_dirent64` records, each `d_reclen`-aligned to 8.
    blob: Vec<u8>,
    pos: Mutex<usize>,
}

impl DirFile {
    /// `entries`: `(inode, d_type, name)` — `d_type` already in Linux `DT_*`.
    pub fn new(entries: &[(u64, u8, String)]) -> Arc<Self> {
        let mut blob = Vec::new();
        for (ino, dtype, name) in entries {
            let reclen = (DIRENT_HEAD + name.len() + 1 + 7) & !7;
            let s = blob.len();
            blob.resize(s + reclen, 0);
            blob[s..s + 8].copy_from_slice(&ino.to_le_bytes());
            blob[s + 8..s + 16].copy_from_slice(&((s + reclen) as i64).to_le_bytes()); // d_off
            blob[s + 16..s + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
            blob[s + 18] = *dtype;
            blob[s + DIRENT_HEAD..s + DIRENT_HEAD + name.len()].copy_from_slice(name.as_bytes());
        }
        Arc::new(Self { blob, pos: Mutex::new(0) })
    }
}

impl FileOps for DirFile {
    fn read(&self, _buf: &mut [u8]) -> i64 {
        EISDIR
    }
    fn write(&self, _buf: &[u8]) -> i64 {
        EISDIR
    }
    fn seek(&self, offset: i64, whence: u32) -> i64 {
        // Only a rewind (`SEEK_SET 0`) is meaningful for a dir stream.
        let mut pos = self.pos.lock();
        match whence {
            SEEK_SET => {
                *pos = offset.max(0) as usize;
                *pos as i64
            }
            _ => EINVAL,
        }
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFDIR | 0o755, self.blob.len() as u64)
    }
    fn getdents64(&self, buf: &mut [u8]) -> i64 {
        let mut pos = self.pos.lock();
        let mut n = 0;
        while *pos + n < self.blob.len() {
            let rl = self.blob[*pos + n + 16] as usize | (self.blob[*pos + n + 17] as usize) << 8;
            if n + rl > buf.len() {
                break;
            }
            buf[n..n + rl].copy_from_slice(&self.blob[*pos + n..*pos + n + rl]);
            n += rl;
        }
        if n == 0 {
            return if *pos < self.blob.len() { EINVAL } else { 0 };
        }
        *pos += n;
        n as i64
    }
}

// --- pipe: a bounded in-memory byte stream with two typed endpoints ---

const PIPE_CAP: usize = 64 * 1024;

struct PipeInner {
    buf: Mutex<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    /// Woken on every state change: data added, space freed, an end closed.
    wq: crate::wait::WaitQueue,
}

/// Read end. EOF (`read` returns 0) once every write end is dropped.
pub struct PipeReadEnd(Arc<PipeInner>);
/// Write end. `write` returns `-EPIPE` once every read end is dropped.
pub struct PipeWriteEnd(Arc<PipeInner>);

/// A fresh pipe: `(read end, write end)`. Endpoint counts track distinct
/// endpoint objects (an fd shared by `fork`/`dup` is one object), so EOF and
/// `EPIPE` fire when the last holder of a side goes away.
pub fn pipe() -> (Arc<PipeReadEnd>, Arc<PipeWriteEnd>) {
    let inner = Arc::new(PipeInner {
        buf: Mutex::new(VecDeque::new()),
        readers: AtomicUsize::new(1),
        writers: AtomicUsize::new(1),
        wq: crate::wait::WaitQueue::new(),
    });
    (Arc::new(PipeReadEnd(inner.clone())), Arc::new(PipeWriteEnd(inner)))
}

impl Drop for PipeReadEnd {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::Release);
        self.0.wq.wake_all(); // let blocked writers see -EPIPE
    }
}
impl Drop for PipeWriteEnd {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::Release);
        self.0.wq.wake_all(); // let blocked readers see EOF
    }
}

impl FileOps for PipeReadEnd {
    fn read(&self, buf: &mut [u8]) -> i64 {
        loop {
            {
                let mut q = self.0.buf.lock();
                if !q.is_empty() {
                    let n = buf.len().min(q.len());
                    for b in buf.iter_mut().take(n) {
                        *b = q.pop_front().unwrap();
                    }
                    drop(q);
                    self.0.wq.wake_all(); // space freed — wake blocked writers
                    return n as i64;
                }
                if self.0.writers.load(Ordering::Acquire) == 0 {
                    return 0; // EOF — no writers left
                }
            }
            self.0.wq.wait_if(|| {
                self.0.buf.lock().is_empty() && self.0.writers.load(Ordering::Acquire) != 0
            });
        }
    }
    fn write(&self, _buf: &[u8]) -> i64 {
        EBADF
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        ESPIPE
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFIFO | 0o600, 0)
    }
}

impl FileOps for PipeWriteEnd {
    fn read(&self, _buf: &mut [u8]) -> i64 {
        EBADF
    }
    fn write(&self, data: &[u8]) -> i64 {
        let mut done = 0;
        while done < data.len() {
            if self.0.readers.load(Ordering::Acquire) == 0 {
                return if done == 0 { EPIPE } else { done as i64 };
            }
            {
                let mut q = self.0.buf.lock();
                let space = PIPE_CAP - q.len();
                if space > 0 {
                    let n = space.min(data.len() - done);
                    q.extend(data[done..done + n].iter().copied());
                    done += n;
                    drop(q);
                    self.0.wq.wake_all(); // data available — wake blocked readers
                    continue;
                }
            }
            self.0.wq.wait_if(|| {
                self.0.buf.lock().len() == PIPE_CAP && self.0.readers.load(Ordering::Acquire) != 0
            });
        }
        done as i64
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        ESPIPE
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFIFO | 0o600, 0)
    }
}
