// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — open files behind a descriptor.
//!
//! A tiny `FileOps` trait with several implementations: `ConsoleFile`
//! (stdin/stdout/stderr over the serial console) and `Ext2File` (a real ext2
//! regular file — read/write, whole-file-rewrite-on-write). Pipes, `/dev`,
//! sockets all slot in here later.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
const EIO: i64 = -5;
const EROFS: i64 = -30;

pub trait FileOps: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> i64;
    fn write(&self, buf: &[u8]) -> i64;
    fn seek(&self, offset: i64, whence: u32) -> i64;
    /// (mode bits for `st_mode`, size for `st_size`).
    fn stat(&self) -> (u32, u64);
    /// `(atime, mtime, ctime)` in seconds since the epoch; zeros if unknown.
    fn times(&self) -> (u32, u32, u32) {
        (0, 0, 0)
    }
    /// Fill `buf` with `struct linux_dirent64` records; `0` at end-of-dir,
    /// `-EINVAL` if `buf` is too small for even one record. Not a directory by
    /// default.
    fn getdents64(&self, _buf: &mut [u8]) -> i64 {
        ENOTDIR
    }
    /// `poll(2)` readiness for the events in `want` (`POLLIN`=1, `POLLOUT`=4, ...). Plain
    /// files and devices are always ready.
    fn poll_mask(&self, want: u16) -> u16 {
        want & (POLLIN | POLLOUT)
    }
    /// `st_ino`: the inode number for files on the root filesystem (0 elsewhere). Programs —
    /// `ld.so` above all — use (dev, ino) to tell whether two paths are the same file.
    fn ino(&self) -> u64 {
        0
    }
    /// `fsync`: make sure everything written reached the disk.
    fn sync(&self) -> i64 {
        0
    }
    /// The socket behind this file, if it is one.
    fn as_socket(&self) -> Option<&crate::net_sock::SockFile> {
        None
    }
    /// `O_NONBLOCK` via `fcntl(F_SETFL)` / `FIONBIO`; only sockets honour it for now.
    fn set_nonblock(&self, _on: bool) {}
    fn is_nonblock(&self) -> bool {
        false
    }
}

pub const POLLIN: u16 = 0x1;
pub const POLLOUT: u16 = 0x4;
pub const POLLERR: u16 = 0x8;
pub const POLLHUP: u16 = 0x10;

// --- /dev ---

/// The few character devices every Unix program expects under `/dev`. They are virtual:
/// `open` recognises the paths before it looks at the filesystem, so they exist even on
/// a root image that has no `/dev` directory.
pub enum DevKind {
    Null,
    Zero,
    /// `/dev/random` and `/dev/urandom` — both the CSPRNG (never blocks once seeded).
    Random,
    /// `/dev/input/mice`: raw PS/2 mouse packets.
    Mice,
}

pub struct DevFile(pub DevKind);

impl FileOps for DevFile {
    fn read(&self, buf: &mut [u8]) -> i64 {
        match self.0 {
            DevKind::Null => 0, // always EOF
            DevKind::Zero => {
                buf.fill(0);
                buf.len() as i64
            }
            DevKind::Random => {
                crate::random::fill(buf);
                buf.len() as i64
            }
            DevKind::Mice => crate::ps2::mouse_read(buf),
        }
    }
    fn poll_mask(&self, want: u16) -> u16 {
        match self.0 {
            DevKind::Mice => {
                if want & POLLIN != 0 && crate::ps2::mouse_ready() { POLLIN } else { 0 }
            }
            _ => want & (POLLIN | POLLOUT),
        }
    }
    fn write(&self, buf: &[u8]) -> i64 {
        // Everything is accepted. Writes to the random devices would only stir the pool;
        // THOS ignores them rather than trusting user-supplied "entropy".
        buf.len() as i64
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        0
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFCHR | 0o666, 0)
    }
}

/// A character device by absolute path, if `path` names one. `/dev/tty` and
/// `/dev/console` are the console: read = the keyboard line discipline, write = the
/// screen (the same objects fds 0/1 are made of).
pub fn open_device(path: &str, want_read: bool, want_write: bool) -> Option<Arc<dyn FileOps>> {
    Some(match path {
        "/dev/null" => Arc::new(DevFile(DevKind::Null)),
        "/dev/zero" => Arc::new(DevFile(DevKind::Zero)),
        "/dev/urandom" | "/dev/random" => Arc::new(DevFile(DevKind::Random)),
        "/dev/input/mice" | "/dev/input/mouse0" => Arc::new(DevFile(DevKind::Mice)),
        "/dev/tty" | "/dev/console" => {
            if want_read && !want_write {
                Arc::new(KeyboardFile)
            } else if want_write && !want_read {
                Arc::new(ConsoleFile { writable: true })
            } else {
                Arc::new(TtyFile)
            }
        }
        _ => return None,
    })
}

/// `/dev/tty` opened read-write: keyboard in, screen out.
pub struct TtyFile;

impl FileOps for TtyFile {
    fn read(&self, buf: &mut [u8]) -> i64 {
        KeyboardFile.read(buf)
    }
    fn write(&self, buf: &[u8]) -> i64 {
        ConsoleFile { writable: true }.write(buf)
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        ESPIPE
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFCHR | 0o666, 0)
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
            if crate::console::take_eof() {
                return 0; // Ctrl+D on an empty line
            }
            if crate::signal::interrupted() {
                return -4; // EINTR
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
    fn poll_mask(&self, want: u16) -> u16 {
        let mut r = want & POLLOUT;
        if want & POLLIN != 0 && (crate::console::has_input() || crate::console::eof_pending()) {
            r |= POLLIN;
        }
        r
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

// --- ext2-backed regular file, read/write ---

/// A real ext2 file, opened by path. Like `MemFile`, the whole file is
/// slurped into memory on open — but `write()` mutates that buffer *and*
/// persists the whole thing back to ext2 via `write_path` — **write-back**: on `fsync`, on
/// close, and every 64 KiB written, not on every `write` call (the old rewrite-everything-per-
/// call was quadratic for big files). No dirty-range tracking — simple and correct.
///
/// This is what makes a file-backed [`crate::process::Section`] able to
/// actually flush to disk: `Section::flush` writes through whatever
/// `FileOps` the handle passed to `NtCreateSection` carries, and this is
/// the one implementation where that write reaches ext2.
pub struct Ext2File {
    path: String,
    buf: Mutex<Vec<u8>>,
    pos: AtomicUsize,
    ino: core::sync::atomic::AtomicU64,
    /// Write-back state: bytes written since the last flush, and whether the file on disk
    /// is behind the buffer. Writes go to the buffer; the disk is written on close, `fsync`
    /// and every 64 KiB — not on every `write` call.
    unflushed: AtomicUsize,
    dirty: AtomicBool,
}

/// Bytes of writes after which the buffer is written to disk without waiting for a close.
const FLUSH_EVERY: usize = 64 * 1024;

impl Ext2File {
    /// Write the buffer to disk if it is behind (the caller holds the buffer lock).
    fn flush_locked(&self, data: &[u8]) -> i64 {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return 0;
        }
        self.unflushed.store(0, Ordering::Relaxed);
        let Some(fs) = crate::ext2::open().ok() else { return EIO };
        if fs.write_path(&self.path, data).is_err() {
            self.dirty.store(true, Ordering::Release);
            return EIO;
        }
        0
    }

    pub fn new(path: String, data: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            path,
            buf: Mutex::new(data),
            pos: AtomicUsize::new(0),
            ino: core::sync::atomic::AtomicU64::new(0),
            unflushed: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
        })
    }
    pub fn with_ino(self: Arc<Self>, ino: u64) -> Arc<Self> {
        self.ino.store(ino, Ordering::Relaxed);
        self
    }
}

impl Drop for Ext2File {
    fn drop(&mut self) {
        // The last close: whatever was written but not yet flushed goes to disk now.
        let data = self.buf.lock();
        let _ = self.flush_locked(&data);
    }
}

/// A large read-only file on the root filesystem, read on demand in 64 KiB chunks — opening
/// a multi-gigabyte file costs its block map (4 bytes per 4 KiB), not its size.
pub struct Ext2Stream {
    ino: u64,
    fs: crate::ext2::Ext2,
    blocks: Vec<u32>,
    size: u64,
    pos: AtomicU64,
    /// The last chunk read: `(file offset, bytes)`.
    cache: Mutex<(u64, Vec<u8>)>,
}

const STREAM_CHUNK: usize = 64 * 1024;
/// Files larger than this are streamed when opened read-only.
pub const STREAM_THRESHOLD: u64 = 256 * 1024;

impl Ext2Stream {
    pub fn new(fs: crate::ext2::Ext2, ino: u32, inode: &crate::ext2::Inode) -> Arc<Self> {
        let blocks = fs.file_blocks(inode);
        Arc::new(Self {
            ino: ino as u64,
            fs,
            blocks,
            size: inode.size,
            pos: AtomicU64::new(0),
            cache: Mutex::new((0, Vec::new())),
        })
    }
}

impl FileOps for Ext2Stream {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn read(&self, buf: &mut [u8]) -> i64 {
        let pos = self.pos.load(Ordering::Relaxed);
        if pos >= self.size || buf.is_empty() {
            return 0;
        }
        let n = buf.len().min((self.size - pos) as usize);
        if n >= STREAM_CHUNK {
            // big read: straight into the caller's buffer, no copy through the cache
            let got = self.fs.read_at(&self.blocks, self.size, pos, &mut buf[..n]);
            self.pos.store(pos + got as u64, Ordering::Relaxed);
            return got as i64;
        }
        let mut done = 0usize;
        let mut cache = self.cache.lock();
        while done < n {
            let p = pos + done as u64;
            let (start, ref data) = *cache;
            if !(p >= start && p < start + data.len() as u64) {
                let aligned = p & !(STREAM_CHUNK as u64 - 1);
                let want = STREAM_CHUNK.min((self.size - aligned) as usize);
                let mut chunk = alloc::vec![0u8; want];
                let got = self.fs.read_at(&self.blocks, self.size, aligned, &mut chunk);
                chunk.truncate(got);
                *cache = (aligned, chunk);
                continue;
            }
            let off = (p - start) as usize;
            let take = (data.len() - off).min(n - done);
            buf[done..done + take].copy_from_slice(&data[off..off + take]);
            done += take;
        }
        self.pos.store(pos + done as u64, Ordering::Relaxed);
        done as i64
    }
    fn write(&self, _buf: &[u8]) -> i64 {
        EBADF // opened read-only
    }
    fn seek(&self, offset: i64, whence: u32) -> i64 {
        let pos = self.pos.load(Ordering::Relaxed) as i64;
        let base = match whence {
            SEEK_SET => 0i64,
            SEEK_CUR => pos,
            SEEK_END => self.size as i64,
            _ => return EINVAL,
        };
        let np = base + offset;
        if np < 0 {
            return EINVAL;
        }
        self.pos.store(np as u64, Ordering::Relaxed);
        np
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFREG | 0o644, self.size)
    }
    fn times(&self) -> (u32, u32, u32) {
        let n = self.fs.read_inode(self.ino as u32);
        (n.atime, n.mtime, n.ctime)
    }
}

impl FileOps for Ext2File {
    fn ino(&self) -> u64 {
        self.ino.load(Ordering::Relaxed)
    }
    fn read(&self, buf: &mut [u8]) -> i64 {
        let data = self.buf.lock();
        let pos = self.pos.load(Ordering::Relaxed);
        if pos >= data.len() {
            return 0;
        }
        let n = buf.len().min(data.len() - pos);
        buf[..n].copy_from_slice(&data[pos..pos + n]);
        self.pos.store(pos + n, Ordering::Relaxed);
        n as i64
    }
    fn write(&self, src: &[u8]) -> i64 {
        let mut data = self.buf.lock();
        let pos = self.pos.load(Ordering::Relaxed);
        if pos + src.len() > data.len() {
            data.resize(pos + src.len(), 0);
        }
        data[pos..pos + src.len()].copy_from_slice(src);
        self.pos.store(pos + src.len(), Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
        let total = self.unflushed.fetch_add(src.len(), Ordering::Relaxed) + src.len();
        // Flush after 64 KiB, or after half the file's size once it is bigger — so the number
        // of whole-file rewrites grows with log(size), not with size.
        if total >= FLUSH_EVERY.max(data.len() / 2) && self.flush_locked(&data) < 0 {
            return EIO;
        }
        src.len() as i64
    }
    fn sync(&self) -> i64 {
        let data = self.buf.lock();
        self.flush_locked(&data)
    }
    fn seek(&self, offset: i64, whence: u32) -> i64 {
        let len = self.buf.lock().len() as i64;
        let pos = self.pos.load(Ordering::Relaxed) as i64;
        let base = match whence {
            SEEK_SET => 0i64,
            SEEK_CUR => pos,
            SEEK_END => len,
            _ => return EINVAL,
        };
        let np = base + offset;
        if np < 0 {
            return EINVAL;
        }
        self.pos.store(np as usize, Ordering::Relaxed);
        np
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFREG | 0o644, self.buf.lock().len() as u64)
    }
    fn times(&self) -> (u32, u32, u32) {
        let Some(fs) = crate::ext2::open().ok() else { return (0, 0, 0) };
        match fs.path_lookup(&self.path) {
            Some(ino) => {
                let n = fs.read_inode(ino);
                (n.atime, n.mtime, n.ctime)
            }
            None => (0, 0, 0),
        }
    }
}

// --- a real, read-only FAT-backed file (the boot ISO's ESP) ---

/// A file read off the FAT-formatted ESP (`\Device\CdRom0` in the NT object
/// namespace — see `device.rs`) — same whole-file-buffer shape as
/// `Ext2File`, but genuinely read-only: `fat.rs` has no write side at all,
/// and a real CD-ROM device wouldn't accept one either.
pub struct FatFile {
    buf: Vec<u8>,
    pos: AtomicUsize,
}

impl FatFile {
    pub fn new(data: Vec<u8>) -> Arc<Self> {
        Arc::new(Self { buf: data, pos: AtomicUsize::new(0) })
    }
}

impl FileOps for FatFile {
    fn read(&self, buf: &mut [u8]) -> i64 {
        let pos = self.pos.load(Ordering::Relaxed);
        if pos >= self.buf.len() {
            return 0;
        }
        let n = buf.len().min(self.buf.len() - pos);
        buf[..n].copy_from_slice(&self.buf[pos..pos + n]);
        self.pos.store(pos + n, Ordering::Relaxed);
        n as i64
    }
    fn write(&self, _src: &[u8]) -> i64 {
        EROFS
    }
    fn seek(&self, offset: i64, whence: u32) -> i64 {
        let len = self.buf.len() as i64;
        let pos = self.pos.load(Ordering::Relaxed) as i64;
        let base = match whence {
            SEEK_SET => 0i64,
            SEEK_CUR => pos,
            SEEK_END => len,
            _ => return EINVAL,
        };
        let np = base + offset;
        if np < 0 {
            return EINVAL;
        }
        self.pos.store(np as usize, Ordering::Relaxed);
        np
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFREG | 0o444, self.buf.len() as u64)
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
            if crate::signal::interrupted() {
                return -4; // EINTR
            }
            self.0.wq.wait_if_intr(|| {
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
    fn poll_mask(&self, want: u16) -> u16 {
        let mut r = 0;
        if want & POLLIN != 0 && !self.0.buf.lock().is_empty() {
            r |= POLLIN;
        }
        if self.0.writers.load(Ordering::Acquire) == 0 {
            r |= POLLHUP | (want & POLLIN); // EOF is "readable" (read returns 0)
        }
        r
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
            if crate::signal::interrupted() {
                return if done == 0 { -4 } else { done as i64 };
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
            self.0.wq.wait_if_intr(|| {
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
