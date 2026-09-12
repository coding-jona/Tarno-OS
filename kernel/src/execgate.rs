// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — the native-exec gate, first slice.
//!
//! The full design (see `docs/thos/roadmap.md`'s security-architecture
//! section): every program entering the system — PE *or* ELF — passes one
//! pipeline before it is allowed to run: `format detect → parse headers →
//! hash / signature check → YARA / static analysis → policy engine → ALLOW
//! | QUARANTINE`. Because both container formats execute natively on THOS,
//! this one mechanism covers the whole system instead of two half-measures.
//!
//! This module is the kernel-side skeleton only: format detection and
//! header parsing already happen in `pe::load`/`elf::load` right after
//! [`check`] runs (a malformed file is rejected there either way); what
//! this adds is the hash/signature check and the (trivial, for now) policy
//! decision. Real YARA / heuristics / a quarantine store are the isolated
//! *userspace* Security Service's job, deliberately — "the scanner never
//! runs in the kernel" is the whole point of that split. The kernel's job
//! is staying small, auditable, and alive no matter what it's handed.

use sha2::{Digest, Sha256};

/// The EICAR Standard Anti-Virus Test File string — the industry-standard,
/// deliberately harmless byte string every real AV engine is expected to
/// flag, specifically so a detection pipeline can be exercised without
/// needing real malware. Present anywhere in a file, that's as real a
/// signature hit as this gate can produce without a real signature
/// database (the Security Service's job later).
const EICAR: &[u8] = b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";

/// SHA-256 hashes of known-bad content. Empty today — a stand-in for the
/// real signature database a userspace scanner would maintain and push
/// down; this exists so the hash/signature pipeline stage is something
/// real, not a TODO, even with nothing in it yet.
const BLOCKED_HASHES: &[[u8; 32]] = &[];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// A human-readable reason, for the reject message / a future
    /// quarantine-log entry.
    Quarantine(&'static str),
}

/// Run `bytes` through the hash/signature check and policy engine. Callers
/// run this *before* handing the same bytes to `pe::load`/`elf::load` —
/// `spawn_pe`, `execve`.
pub fn check(bytes: &[u8]) -> Verdict {
    if contains(bytes, EICAR) {
        return Verdict::Quarantine("EICAR test signature");
    }
    let mut h = Sha256::new();
    h.update(bytes);
    let digest: [u8; 32] = h.finalize().into();
    if BLOCKED_HASHES.contains(&digest) {
        return Verdict::Quarantine("known-bad hash");
    }
    Verdict::Allow
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}
