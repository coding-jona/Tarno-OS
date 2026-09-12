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
//! This module is the kernel-side skeleton, format detection and header
//! parsing already happen in `pe::load`/`elf::load` right after [`check`]
//! runs (a malformed file is rejected there either way) — but the hash
//! verdict itself is no longer purely local. [`check`] first asks the real,
//! isolated Security Service process (`secsvc.rs`) over its channel; only
//! when that's unavailable (never spawned, or gone — see `secsvc.rs`'s own
//! doc comment for why that's a real, not guessed, signal) does it fall
//! back to [`BLOCKED_HASHES`], this module's own small, always-available
//! list. That fallback *is* the "a crash there degrades to a policy
//! default" property the roadmap requires, not a separate mechanism. Real
//! YARA / heuristics / a quarantine store are the Security Service's job to
//! grow into — this kernel-side check stays small, auditable, and alive no
//! matter what it's handed, on top of or without the service.

use sha2::{Digest, Sha256};

/// The EICAR Standard Anti-Virus Test File string — the industry-standard,
/// deliberately harmless byte string every real AV engine is expected to
/// flag, specifically so a detection pipeline can be exercised without
/// needing real malware. Present anywhere in a file, that's as real a
/// signature hit as this gate can produce without a real signature
/// database (the Security Service's job later).
const EICAR: &[u8] = b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";

/// A THOS-authored test marker — **not malware, not derived from any real
/// sample**. THOS ships no malware corpus and never will (fetching /
/// possessing real malicious samples for a hash list is a different, much
/// bigger undertaking than this kernel-side skeleton — that data belongs to
/// the userspace Security Service's real signature database later, sourced
/// from actual threat-intel feeds, not hand-picked into kernel source).
/// This exists purely so `BLOCKED_HASHES` has a genuine, non-empty entry to
/// check against — on content the EICAR *substring* check has no way to
/// catch at all, since matching is a whole-file hash, not a scan — so the
/// hash/signature pipeline stage is provably doing something, not a TODO
/// with nothing in it. `execgate_check` (main.rs) verifies the hash below
/// really is this string's SHA-256, by computing it fresh and comparing,
/// rather than trusting a hand-transcribed hex constant blindly.
pub const MARKER_STRING: &[u8] = b"THOS-EXECGATE-TEST-KNOWN-BAD-MARKER-v1";

/// SHA-256 hashes of known-bad content, checked **only when the Security
/// Service is unavailable** (see the module doc) — the local policy
/// default, not the primary authority. A stand-in for the real signature
/// database the isolated service maintains — one entry (`MARKER_STRING`'s
/// hash) today, so this fallback is real, not a TODO, even though it's
/// nowhere close to an actual malware database.
const BLOCKED_HASHES: &[[u8; 32]] = &[[
    0x80, 0x2c, 0x34, 0x94, 0xf7, 0x96, 0xf1, 0x3b, 0xf2, 0x1d, 0x55, 0x90, 0x92, 0x28, 0x1b, 0x26,
    0x52, 0x58, 0x68, 0xec, 0x99, 0x2c, 0xcf, 0xc7, 0x6f, 0xa8, 0xd2, 0x0b, 0x1c, 0xf5, 0x92, 0xe2,
]];

/// `BLOCKED_HASHES`'s one entry — for `execgate_check` (main.rs) to verify
/// it's really `MARKER_STRING`'s SHA-256, independent of whether that hash
/// currently resolves through the (lifecycle-dependent) Security Service
/// or the local fallback.
pub(crate) fn marker_hash() -> [u8; 32] {
    BLOCKED_HASHES[0]
}

/// A second THOS-authored test marker — same non-malware convention as
/// `MARKER_STRING`, deliberately **not** in [`BLOCKED_HASHES`]. Its hash is
/// only ever known to the test `/secsvc` binary's own list (`xtask/testdata/
/// secsvc.rs`) — so a quarantine on this content can only have come from a
/// genuine round trip through the isolated service, never the local
/// fallback, distinguishing "the service really decided this" from "the
/// kernel's own tiny list happened to already cover it".
pub const SECSVC_ONLY_MARKER: &[u8] = b"THOS-SECSVC-ONLY-TEST-MARKER-v1";

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
    match crate::secsvc::check_hash(&digest) {
        Some(true) => Verdict::Quarantine("Security Service: known-bad hash"),
        Some(false) => Verdict::Allow,
        // Unavailable — degrade to the local policy default, not a crash.
        None if BLOCKED_HASHES.contains(&digest) => Verdict::Quarantine("known-bad hash (local fallback)"),
        None => Verdict::Allow,
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}
