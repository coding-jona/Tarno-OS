// SPDX-License-Identifier: GPL-2.0-or-later
//! An NT-style configuration registry, grown to persisted hives.
//!
//! One global key tree of typed values, addressed by a `\`-separated path
//! (`\Registry\Machine\Software\...`). Backs `NtCreateKey`, `NtOpenKey`,
//! `NtSetValueKey`, `NtQueryValueKey`, `NtDeleteKey`, `NtEnumerateKey` and
//! `NtEnumerateValueKey`.
//!
//! Each top-level root in [`HIVES`] is a **hive**: its own backing file under
//! `/etc/thos/registry/`, loaded once at boot ([`load_hives`]) and rewritten
//! on every mutation under it ([`create`] / [`set_value`] / [`delete_key`]) —
//! durable by default, no explicit flush needed (the tradeoff: every write is
//! an ext2 write; fine for a registry, which isn't a hot path). Not yet
//! transactional (a crash mid-write can still lose that one write), no
//! per-key security, no change-notify.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::ext2::Ext2;

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
    persist(&comps);
    true
}

/// `true` if `path` names an existing key.
pub fn open(path: &str) -> bool {
    let comps = components(path);
    run(|root| find(root, &comps).is_some())
}

/// The name of the `index`-th direct subkey of `path` (natural — sorted —
/// order, stable as long as the key set doesn't change mid-enumeration).
/// `None` once `index` runs past the last one, for `NtEnumerateKey`.
pub fn enumerate_key(path: &str, index: usize) -> Option<String> {
    let comps = components(path);
    run(|root| find(root, &comps)?.subkeys.keys().nth(index).cloned())
}

/// The `(name, type, byte length)` of the `index`-th value directly on
/// `path`. `None` past the last one, for `NtEnumerateValueKey`.
pub fn enumerate_value(path: &str, index: usize) -> Option<(String, u32, usize)> {
    let comps = components(path);
    run(|root| {
        find(root, &comps)?
            .values
            .iter()
            .nth(index)
            .map(|(n, v)| (n.clone(), v.ty, v.data.len()))
    })
}

/// Set (or replace) a value on an existing key. `false` if the key is missing.
pub fn set_value(path: &str, name: &str, ty: u32, data: &[u8]) -> bool {
    let comps = components(path);
    let ok = run(|root| match find_mut(root, &comps) {
        Some(k) => {
            k.values
                .insert(name.to_ascii_lowercase(), Value { ty, data: data.to_vec() });
            true
        }
        None => false,
    });
    if ok {
        persist(&comps);
    }
    ok
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
    let ok = run(|root| match find_mut(root, parent) {
        Some(p) => p.subkeys.remove(&leaf[0]).is_some(),
        None => false,
    });
    if ok {
        persist(&comps);
    }
    ok
}

// --- hives: persistence to ext2 ---------------------------------------

/// Top-level hive roots and their backing file. A path under one of these
/// gets rewritten to disk on every mutation; a path outside all of them
/// (there is none today — every seeded root is hived) just stays in memory.
const HIVES: &[(&[&str], &str)] = &[
    (&["machine", "software"], "/etc/thos/registry/software.hiv"),
    (&["machine", "system"], "/etc/thos/registry/system.hiv"),
    (&["user"], "/etc/thos/registry/user.hiv"),
];

fn under_hive(comps: &[String], root: &[&str]) -> bool {
    comps.len() >= root.len() && comps.iter().zip(root.iter()).all(|(a, b)| a == b)
}

/// Rewrite whichever hive (if any) `comps` falls under. Best-effort: a
/// mid-boot ext2 hiccup must not make the in-memory registry unusable, so
/// failures are silently swallowed — the change still lives in RAM.
fn persist(comps: &[String]) {
    let Some((root, path)) = HIVES.iter().find(|(root, _)| under_hive(comps, root)) else {
        return;
    };
    let Ok(fs) = crate::ext2::open() else { return };
    let root_comps: Vec<String> = root.iter().map(|s| s.to_string()).collect();
    let Some(bytes) = run(|r| find(r, &root_comps).map(serialize_hive)) else {
        return;
    };
    let _ = fs.mkdir_path("/etc");
    let _ = fs.mkdir_path("/etc/thos");
    let _ = fs.mkdir_path("/etc/thos/registry");
    let _ = fs.write_path(path, &bytes);
}

/// Load every hive's backing file (if present — first boot has none) into the
/// in-memory tree. Call once, after ext2 is mounted, before anything else
/// touches the registry (the seeded defaults are harmless to overwrite: a
/// loaded hive fully replaces its root's subtree).
static HIVES_LOADED: AtomicBool = AtomicBool::new(false);

/// Returns how many of [`HIVES`] had a backing file to load (0 on first boot).
pub fn load_hives(fs: &Ext2) -> usize {
    if HIVES_LOADED.swap(true, Ordering::Relaxed) {
        return 0;
    }
    let mut n = 0;
    for (root, path) in HIVES {
        let Some(bytes) = fs.read_path(path) else { continue };
        n += 1;
        let root_comps: Vec<String> = root.iter().map(|s| s.to_string()).collect();
        run(|r| {
            let base = make(r, &root_comps);
            *base = Key::new(); // the file is authoritative — drop the seed
            deserialize_hive(base, &bytes);
        });
    }
    n
}

/// `thos-hive v1` text format: one record per line, all string fields
/// hex-encoded so any byte (a name or data from a hostile PE) round-trips
/// with no escaping rules to get wrong.
///   K <hex relpath>                        -- a subkey exists
///   V <hex relpath> <hex name> <type> <hex data>  -- a value on that key
/// `relpath` is `\`-joined, relative to the hive root (empty = the root
/// itself); re-parsed with the same `components()` as every other path here.
fn serialize_hive(base: &Key) -> Vec<u8> {
    let mut out = String::from("thos-hive v1\n");
    fn walk(k: &Key, prefix: &str, out: &mut String) {
        for (name, v) in &k.values {
            let _ = writeln!(
                out,
                "V {} {} {} {}",
                hex(prefix.as_bytes()),
                hex(name.as_bytes()),
                v.ty,
                hex(&v.data)
            );
        }
        for (name, sub) in &k.subkeys {
            let child = if prefix.is_empty() { name.clone() } else { format!("{prefix}\\{name}") };
            let _ = writeln!(out, "K {}", hex(child.as_bytes()));
            walk(sub, &child, out);
        }
    }
    walk(base, "", &mut out);
    out.into_bytes()
}

fn deserialize_hive(base: &mut Key, bytes: &[u8]) {
    let Ok(text) = core::str::from_utf8(bytes) else { return };
    for line in text.lines() {
        let mut it = line.split(' ');
        match it.next() {
            Some("K") => {
                let Some(rp) = it.next().and_then(unhex_string) else { continue };
                make(base, &components(&rp));
            }
            Some("V") => {
                let (Some(rp), Some(name), Some(ty), Some(data)) = (
                    it.next().and_then(unhex_string),
                    it.next().and_then(unhex_string),
                    it.next().and_then(|s| s.parse::<u32>().ok()),
                    it.next().and_then(unhex_bytes),
                ) else {
                    continue;
                };
                let k = make(base, &components(&rp));
                k.values.insert(name.to_ascii_lowercase(), Value { ty, data });
            }
            _ => {} // blank line, the "thos-hive v1" header, or garbage — skip
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xF) as u32, 16).unwrap());
    }
    if s.is_empty() {
        s.push('-'); // a would-be-empty field, so `split(' ')` still sees a token
    }
    s
}

fn unhex_bytes(s: &str) -> Option<Vec<u8>> {
    if s == "-" {
        return Some(Vec::new());
    }
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn unhex_string(s: &str) -> Option<String> {
    String::from_utf8(unhex_bytes(s)?).ok()
}
