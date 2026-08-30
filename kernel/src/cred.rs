// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — the credential store (stub).
//!
//! THOS ships **no** account. On first boot the console setup makes the operator
//! choose the admin name + password themselves; this module hashes it and
//! persists it to ext2, and verifies it on every later boot.
//!
//! Stub crypto: PBKDF2-HMAC-SHA-256 (soft SHA), salt from `RDRAND` where present.
//! Phase 3 swaps the KDF for argon2id (needs a bigger kernel heap) and moves the
//! store behind the `Principal` / token model. There is deliberately no root
//! password — see `docs/thos/roadmap.md` "Identity, privilege & login".

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use sha2::{Digest, Sha256};

use crate::ext2::Ext2;

pub const STORE_DIR: &str = "/etc/thos";
pub const STORE_PATH: &str = "/etc/thos/admin.cred";

/// PBKDF2 iteration count. Modest for a stub in-kernel KDF; argon2id later.
const ROUNDS: u32 = 200_000;
/// The admin's normal session runs unprivileged — never uid 0. `elevate`
/// (Phase 3) is what hands out privilege.
pub const ADMIN_UID: u32 = 1000;

pub struct Cred {
    pub name: String,
    salt: [u8; 16],
    hash: [u8; 32],
}

impl Cred {
    /// Hash a fresh `(name, password)` with a random salt.
    pub fn create(name: &str, password: &str) -> Self {
        let mut salt = [0u8; 16];
        for c in salt.chunks_mut(8) {
            c.copy_from_slice(&rand64().to_le_bytes()[..c.len()]);
        }
        let hash = pbkdf2_hmac_sha256(password.as_bytes(), &salt, ROUNDS);
        Self { name: name.to_string(), salt, hash }
    }

    /// Constant-time check of a login attempt.
    pub fn verify(&self, name: &str, password: &str) -> bool {
        let got = pbkdf2_hmac_sha256(password.as_bytes(), &self.salt, ROUNDS);
        let name_ok = name.as_bytes() == self.name.as_bytes();
        ct_eq(&got, &self.hash) & name_ok
    }

    fn serialize(&self) -> Vec<u8> {
        format!(
            "thos-cred v1\nname={}\nrounds={ROUNDS}\nsalt={}\nhash={}\n",
            self.name,
            hex(&self.salt),
            hex(&self.hash),
        )
        .into_bytes()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(bytes).ok()?;
        let mut name = None;
        let mut salt = [0u8; 16];
        let mut hash = [0u8; 32];
        let (mut got_salt, mut got_hash) = (false, false);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue }; // skips the "thos-cred v1" header
            match k {
                "name" => name = Some(v.to_string()),
                "salt" => got_salt = unhex(v, &mut salt),
                "hash" => got_hash = unhex(v, &mut hash),
                _ => {}
            }
        }
        Some(Self { name: name?, salt: got_salt.then_some(salt)?, hash: got_hash.then_some(hash)? })
    }
}

/// Is an admin credential already stored?
pub fn exists(fs: &Ext2) -> bool {
    fs.read_path(STORE_PATH).is_some()
}

pub fn load(fs: &Ext2) -> Option<Cred> {
    Cred::parse(&fs.read_path(STORE_PATH)?)
}

/// Persist `cred` to `/etc/thos/admin.cred`, creating `/etc` and `/etc/thos`.
pub fn save(fs: &Ext2, cred: &Cred) -> Result<(), &'static str> {
    for dir in ["/etc", STORE_DIR] {
        match fs.mkdir_path(dir) {
            Ok(()) | Err("already exists") => {}
            Err(e) => return Err(e),
        }
    }
    fs.write_path(STORE_PATH, &cred.serialize())
}

// --- primitives ---------------------------------------------------------

/// 64 bits of salt entropy. Prefers `RDRAND` (real hardware has it); falls back
/// to a `RDTSC`-seeded xorshift where the CPU lacks it (QEMU's default model)
/// so a missing optional instruction can't `#UD` the kernel.
fn rand64() -> u64 {
    let has_rdrand = unsafe { core::arch::x86_64::__cpuid(1).ecx } & (1 << 30) != 0;
    if has_rdrand {
        for _ in 0..32 {
            let mut x = 0u64;
            if unsafe { core::arch::x86_64::_rdrand64_step(&mut x) } == 1 {
                return x;
            }
        }
    }
    static S: AtomicU64 = AtomicU64::new(0);
    let mut x = S.load(Ordering::Relaxed) ^ unsafe { core::arch::x86_64::_rdtsc() };
    if x == 0 {
        x = 0x9E37_79B9_7F4A_7C15;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    S.store(x, Ordering::Relaxed);
    x
}

fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut d = 0u8;
    for i in 0..32 {
        d |= a[i] ^ b[i];
    }
    d == 0
}

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

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// HMAC-SHA-256(key, msg).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(&[key]));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = sha256(&[&ipad, msg]);
    sha256(&[&opad, &inner])
}

/// PBKDF2-HMAC-SHA-256 for a single 32-byte output block.
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut block = salt.to_vec();
    block.extend_from_slice(&1u32.to_be_bytes()); // INT(i=1)
    let mut u = hmac_sha256(password, &block);
    let mut out = u;
    for _ in 1..rounds {
        u = hmac_sha256(password, &u);
        for i in 0..32 {
            out[i] ^= u[i];
        }
    }
    out
}
