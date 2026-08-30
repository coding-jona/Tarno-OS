// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — VFS scaffold.
//!
//! A `Vnode` trait, an in-memory filesystem, and open-file objects that live in
//! the same [`object`](crate::object) handle table the personalities will hand
//! out as POSIX fds / NT `HANDLE`s. Path handling is flat for now (single root,
//! no directories); a real namei() + mount table + on-disk filesystems come
//! next, backed by [`ahci`](crate::ahci).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::{Mutex, Once};

use crate::object::{self, Handle};

#[allow(dead_code)] // full vnode/vfs surface; the POSIX personality consumes the rest
pub trait Vnode: Send + Sync {
    fn len(&self) -> usize;
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize;
    fn write_at(&self, offset: usize, buf: &[u8]) -> usize;
}

/// In-memory regular file.
pub struct RamFile {
    data: Mutex<Vec<u8>>,
}

impl RamFile {
    fn new() -> Arc<Self> {
        Arc::new(Self { data: Mutex::new(Vec::new()) })
    }
}

impl Vnode for RamFile {
    fn len(&self) -> usize {
        self.data.lock().len()
    }

    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let data = self.data.lock();
        if offset >= data.len() {
            return 0;
        }
        let n = buf.len().min(data.len() - offset);
        buf[..n].copy_from_slice(&data[offset..offset + n]);
        n
    }

    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut data = self.data.lock();
        if offset + buf.len() > data.len() {
            data.resize(offset + buf.len(), 0);
        }
        data[offset..offset + buf.len()].copy_from_slice(buf);
        buf.len()
    }
}

struct RamFs {
    files: Mutex<BTreeMap<String, Arc<dyn Vnode>>>,
}

static ROOT: Once<RamFs> = Once::new();

fn root() -> &'static RamFs {
    ROOT.get().expect("vfs::init not called")
}

pub fn init() {
    ROOT.call_once(|| RamFs { files: Mutex::new(BTreeMap::new()) });
}

/// Create (or truncate) a file and return its vnode.
pub fn create(path: &str) -> Arc<dyn Vnode> {
    let v = RamFile::new();
    root().files.lock().insert(path.to_string(), v.clone() as Arc<dyn Vnode>);
    v
}

pub fn lookup(path: &str) -> Option<Arc<dyn Vnode>> {
    root().files.lock().get(path).cloned()
}

pub fn list() -> Vec<String> {
    root().files.lock().keys().cloned().collect()
}

/// An open file: a vnode plus a cursor. Handed out via the handle table.
pub struct OpenFile {
    vnode: Arc<dyn Vnode>,
    offset: Mutex<usize>,
}

/// Open an existing path, returning a handle (fd-equivalent).
pub fn open(path: &str) -> Option<Handle> {
    let vnode = lookup(path)?;
    let of = Arc::new(OpenFile { vnode, offset: Mutex::new(0) });
    Some(object::insert(of))
}

pub fn read(h: Handle, buf: &mut [u8]) -> Option<usize> {
    let of = object::get::<OpenFile>(h)?;
    let mut off = of.offset.lock();
    let n = of.vnode.read_at(*off, buf);
    *off += n;
    Some(n)
}

#[allow(dead_code)]
pub fn write(h: Handle, buf: &[u8]) -> Option<usize> {
    let of = object::get::<OpenFile>(h)?;
    let mut off = of.offset.lock();
    let n = of.vnode.write_at(*off, buf);
    *off += n;
    Some(n)
}

pub fn close(h: Handle) -> bool {
    object::close(h)
}
