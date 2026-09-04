// SPDX-License-Identifier: GPL-2.0-or-later
//! A minimal NT-style configuration registry.
//!
//! One global key tree of typed values, addressed by a `\`-separated path
//! (`\Registry\Machine\Software\...`). Enough to back `NtCreateKey`,
//! `NtOpenKey`, `NtSetValueKey`, `NtQueryValueKey` and `NtDeleteKey`. Not yet
//! transactional, not persisted to disk, no enumeration / security / notify —
//! those come with the real registry hive work.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// One stored value: an `REG_*` type tag plus its raw bytes.
pub struct Value {
    pub ty: u32,
    pub data: Vec<u8>,
}

struct Key {
    subkeys: BTreeMap<String, Key>,
    values: BTreeMap<String, Value>,
}
impl Key {
    const fn new() -> Self {
        Self { subkeys: BTreeMap::new(), values: BTreeMap::new() }
    }
}

static ROOT: Mutex<Key> = Mutex::new(Key::new());
static SEEDED: AtomicBool = AtomicBool::new(false);

/// Split a path into normalised components (lowercased, non-empty). A leading
/// `registry` element is dropped, so `\Registry\Machine` and `Machine` name the
/// same key.
fn components(path: &str) -> Vec<String> {
    let mut c: Vec<String> = path
        .split(|ch| ch == '\\' || ch == '/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if c.first().map(|s| s.as_str() == "registry").unwrap_or(false) {
        c.remove(0);
    }
    c
}

/// The canonical `\`-joined form of `path` — what a key HANDLE stores.
pub fn canon(path: &str) -> String {
    components(path).join("\\")
}

/// Lock the tree, seeding the standard hive roots on first use.
fn run<R>(f: impl FnOnce(&mut Key) -> R) -> R {
    let mut root = ROOT.lock();
    if !SEEDED.swap(true, Ordering::Relaxed) {
        for h in ["machine", "user", "machine\\software", "machine\\system"] {
            make(&mut root, &components(h));
        }
    }
    f(&mut root)
}

fn make<'a>(root: &'a mut Key, comps: &[String]) -> &'a mut Key {
    let mut k = root;
    for c in comps {
        k = k.subkeys.entry(c.clone()).or_insert_with(Key::new);
    }
    k
}
fn find<'a>(root: &'a Key, comps: &[String]) -> Option<&'a Key> {
    let mut k = root;
    for c in comps {
        k = k.subkeys.get(c)?;
    }
    Some(k)
}
fn find_mut<'a>(root: &'a mut Key, comps: &[String]) -> Option<&'a mut Key> {
    let mut k = root;
    for c in comps {
        k = k.subkeys.get_mut(c)?;
    }
    Some(k)
}

/// Create `path` (and any missing ancestors). `false` only for an empty path.
pub fn create(path: &str) -> bool {
    let comps = components(path);
    if comps.is_empty() {
        return false;
    }
    run(|root| {
        make(root, &comps);
    });
    true
}

/// `true` if `path` names an existing key.
pub fn open(path: &str) -> bool {
    let comps = components(path);
    run(|root| find(root, &comps).is_some())
}

/// Set (or replace) a value on an existing key. `false` if the key is missing.
pub fn set_value(path: &str, name: &str, ty: u32, data: &[u8]) -> bool {
    let comps = components(path);
    run(|root| match find_mut(root, &comps) {
        Some(k) => {
            k.values
                .insert(name.to_ascii_lowercase(), Value { ty, data: data.to_vec() });
            true
        }
        None => false,
    })
}

/// Read a value back as `(type, bytes)`.
pub fn query_value(path: &str, name: &str) -> Option<(u32, Vec<u8>)> {
    let comps = components(path);
    let name = name.to_ascii_lowercase();
    run(|root| {
        let k = find(root, &comps)?;
        let v = k.values.get(&name)?;
        Some((v.ty, v.data.clone()))
    })
}

/// Remove a leaf key from its parent. `false` if the path is empty, the parent
/// is missing, or the key does not exist.
pub fn delete_key(path: &str) -> bool {
    let comps = components(path);
    if comps.is_empty() {
        return false;
    }
    let (parent, leaf) = comps.split_at(comps.len() - 1);
    run(|root| match find_mut(root, parent) {
        Some(p) => p.subkeys.remove(&leaf[0]).is_some(),
        None => false,
    })
}
