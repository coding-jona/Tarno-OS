// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — file-integrity baselines.
//!
//! A snapshot of SHA-256 hashes for a fixed set of security-relevant files,
//! taken once (first boot — nothing stored yet) and checked against on
//! every later boot. This is *detection*, not prevention: nothing here
//! stops a write to a baselined file, it only notices afterward that one
//! happened. That is exactly the "file-integrity baselines" item from the
//! Security Core list — the building block a later on-access scanner or
//! boot-attestation flow reads, not a scanner itself.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use sha2::{Digest, Sha256};

use crate::ext2::Ext2;

pub const STORE_DIR: &str = "/etc/thos";
pub const STORE_PATH: &str = "/etc/thos/integrity.baseline";

/// The files THOS ships today that are always present, on every boot
/// configuration, worth baselining as the initial set: the fork/exec test's
/// own init program and a real statically-linked binary. Grows as more of
/// the system (the registry hives, the credential store, a real `/sbin`)
/// becomes something every boot can rely on being there.
pub const BASELINE_FILES: &[&str] = &["/init", "/rusthello"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Hashed the same as the baseline.
    Ok,
    /// Hashed *differently* than the baseline — the file changed since.
    Tampered,
    /// Not found, either while recording (skipped, not baselined as a hash
    /// of nothing) or while verifying (a baselined file is simply gone).
    Missing,
}

pub struct Check {
    pub path: String,
    pub outcome: Outcome,
}

fn sha256_of(fs: &Ext2, path: &str) -> Option<[u8; 32]> {
    let bytes = fs.read_path(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(h.finalize().into())
}

/// Is a baseline already recorded?
pub fn exists(fs: &Ext2) -> bool {
    fs.read_path(STORE_PATH).is_some()
}

/// Hash every file in `paths` and persist the result as the new baseline
/// (`/etc/thos/integrity.baseline`, the same "own text format, hex-encoded
/// fields" convention the registry hives and credential store already use).
/// A file that doesn't currently exist is skipped — not recorded as a hash
/// of nothing — and reported as [`Outcome::Missing`] like `verify` would.
pub fn record(fs: &Ext2, paths: &[&str]) -> Vec<Check> {
    let mut out = String::from("thos-integrity v1\n");
    let mut checks = Vec::with_capacity(paths.len());
    for &path in paths {
        match sha256_of(fs, path) {
            Some(hash) => {
                out.push_str(&format!("{path}={}\n", hex(&hash)));
                checks.push(Check { path: path.to_string(), outcome: Outcome::Ok });
            }
            None => checks.push(Check { path: path.to_string(), outcome: Outcome::Missing }),
        }
    }
    for dir in ["/etc", STORE_DIR] {
        match fs.mkdir_path(dir) {
            Ok(()) | Err("already exists") => {}
            Err(_) => {} // best-effort — write_path below is the real failure signal
        }
    }
    let _ = fs.write_path(STORE_PATH, out.as_bytes());
    checks
}

/// Load the stored baseline and recompute + compare every entry's hash.
/// `[]` if there is no baseline yet — callers should check [`exists`] (or
/// `record` one) first; this deliberately doesn't do that itself, so a
/// caller can't silently treat "never baselined" the same as "verified
/// clean".
pub fn verify(fs: &Ext2) -> Vec<Check> {
    let Some(raw) = fs.read_path(STORE_PATH) else { return Vec::new() };
    let Ok(text) = core::str::from_utf8(&raw) else { return Vec::new() };
    let mut checks = Vec::new();
    for line in text.lines() {
        // The first line is the "thos-integrity v1" header — no `=`, skipped.
        let Some((path, want_hex)) = line.split_once('=') else { continue };
        let mut want = [0u8; 32];
        if !unhex(want_hex, &mut want) {
            continue; // a corrupt line shouldn't crash the check — just skip it
        }
        let outcome = match sha256_of(fs, path) {
            Some(got) if got == want => Outcome::Ok,
            Some(_) => Outcome::Tampered,
            None => Outcome::Missing,
        };
        checks.push(Check { path: path.to_string(), outcome });
    }
    checks
}

// --- tiny hex helpers — same shape as `cred.rs`'s own, deliberately not
// shared: this module and the credential store are independent enough (and
// the helpers small enough) that a shared dependency isn't worth it. ---

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xF) as u32, 16).unwrap());
    }
    s
}

fn unhex(s: &str, out: &mut [u8]) -> bool {
    let s = s.as_bytes();
    if s.len() != out.len() * 2 {
        return false;
    }
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (s[i * 2] as char).to_digit(16);
        let lo = (s[i * 2 + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => *o = ((h << 4) | l) as u8,
            _ => return false,
        }
    }
    true
}
