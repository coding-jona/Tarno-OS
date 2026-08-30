// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — the Executive object + handle manager.
//!
//! One generic kernel object type (`Arc<dyn Any + Send + Sync>`) and one handle
//! table. Later, the POSIX personality's file descriptors and the NT
//! personality's `HANDLE`s are both just indices into a table like this — the
//! "one object, many views" idea from docs/thos/architecture.md.
//!
//! For now there is a single global table; it becomes per-process with `execve`
//! / `NtCreateUserProcess` in Phase 2/3.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::any::Any;

use spin::Mutex;

/// Opaque handle value. `0` is never valid (mirrors NULL / -1 conventions).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(pub u32);

pub type Object = Arc<dyn Any + Send + Sync>;

pub struct HandleTable {
    map: BTreeMap<u32, Object>,
    next: u32,
}

impl HandleTable {
    const fn new() -> Self {
        Self { map: BTreeMap::new(), next: 1 }
    }

    pub fn insert(&mut self, obj: Object) -> Handle {
        let id = self.next;
        self.next += 1;
        self.map.insert(id, obj);
        Handle(id)
    }

    /// Look a handle up and downcast it to a concrete object type.
    pub fn get<T: Any + Send + Sync>(&self, h: Handle) -> Option<Arc<T>> {
        let obj = self.map.get(&h.0)?.clone();
        obj.downcast::<T>().ok()
    }

    pub fn close(&mut self, h: Handle) -> bool {
        self.map.remove(&h.0).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

static TABLE: Mutex<HandleTable> = Mutex::new(HandleTable::new());

pub fn insert(obj: Object) -> Handle {
    TABLE.lock().insert(obj)
}

pub fn get<T: Any + Send + Sync>(h: Handle) -> Option<Arc<T>> {
    TABLE.lock().get::<T>(h)
}

pub fn close(h: Handle) -> bool {
    TABLE.lock().close(h)
}

pub fn open_count() -> usize {
    TABLE.lock().len()
}
