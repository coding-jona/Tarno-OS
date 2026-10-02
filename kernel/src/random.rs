// SPDX-License-Identifier: GPL-2.0-or-later
//! The kernel's cryptographically secure random number generator.
//!
//! Before this, `getrandom` was a fixed-seed xorshift (every boot produced the same,
//! trivially predictable stream) and password salts fell back to a TSC xorshift on
//! CPUs without `RDRAND` — such as the Acer's Westmere. Network keys, TLS and package
//! signatures all need real randomness, so this is the foundation for them.
//!
//! Design (the same shape as Linux's and the BSDs'):
//!  * an **entropy pool** (a SHA-256 chaining value) into which every unpredictable
//!    event is mixed: `RDSEED`/`RDRAND` words where the CPU has them, TSC jitter
//!    around PIT reads (boot), the RTC, and — continuously — the TSC at timer ticks,
//!    keyboard events and disk completions;
//!  * a **ChaCha20** generator keyed from the pool, with *fast key erasure*: every
//!    request first replaces the key from the keystream, so a later compromise of the
//!    state cannot reveal earlier output;
//!  * periodic **reseeding** from the pool as new events arrive.
//!
//! Honest limitation: on a CPU with no hardware RNG the boot-time entropy is the
//! timing jitter measured here (a few hundred TSC samples around slow I/O reads). That
//! is what such machines have; interrupt/keyboard/disk timing keeps feeding the pool
//! afterwards.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use spin::Mutex;

struct Rng {
    /// Entropy pool: SHA-256 chaining state.
    pool: [u8; 32],
    /// ChaCha20 key and counter.
    key: [u32; 8],
    counter: u64,
    /// Events mixed in since the last reseed.
    fresh: u32,
    seeded: bool,
}

static RNG: Mutex<Rng> =
    Mutex::new(Rng { pool: [0; 32], key: [0; 8], counter: 0, fresh: 0, seeded: false });
/// Cheap lock-free staging area for events from IRQ context (folded into the pool
/// on the next request, never blocking an interrupt handler on the RNG lock).
static STAGE: AtomicU64 = AtomicU64::new(0);
static STAGED: AtomicU32 = AtomicU32::new(0);

// ---- ChaCha20 (RFC 8439) -------------------------------------------------

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One 64-byte ChaCha20 block for `(key, counter, nonce)`.
pub fn chacha20_block(key: &[u32; 8], counter: u32, nonce: &[u32; 3]) -> [u8; 64] {
    let mut init = [0u32; 16];
    init[0..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    init[4..12].copy_from_slice(key);
    init[12] = counter;
    init[13..16].copy_from_slice(nonce);
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[i * 4..i * 4 + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// Self-check against the RFC 8439 §2.3.2 test vector. `true` if the block function is correct.
pub fn chacha20_known_answer() -> bool {
    let mut key = [0u32; 8];
    for (i, k) in key.iter_mut().enumerate() {
        let b = (i * 4) as u32;
        *k = b | ((b + 1) << 8) | ((b + 2) << 16) | ((b + 3) << 24);
    }
    let out = chacha20_block(&key, 1, &[0x0900_0000, 0x4a00_0000, 0]);
    // First four and last four words of the RFC's serialized block.
    let w = |i: usize| u32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
    w(0) == 0xe4e7_f110 && w(1) == 0x1559_3bd1 && w(2) == 0x1fdd_0f50 && w(3) == 0xc471_20a3
        && w(12) == 0xd19c_12b5 && w(13) == 0xb94e_16de && w(14) == 0xe883_d0cb && w(15) == 0x4e3c_50a2
}

// ---- entropy ---------------------------------------------------------------

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn has_cpuid_bit(leaf: u32, sub: u32, reg_ebx: bool, bit: u32) -> bool {
    let max = core::arch::x86_64::__cpuid(0).eax;
    if leaf > max {
        return false;
    }
    let r = core::arch::x86_64::__cpuid_count(leaf, sub);
    (if reg_ebx { r.ebx } else { r.ecx }) & (1 << bit) != 0
}

fn hw_random64() -> Option<u64> {
    let mut x = 0u64;
    if has_cpuid_bit(7, 0, true, 18) {
        // RDSEED
        for _ in 0..16 {
            if unsafe { core::arch::x86_64::_rdseed64_step(&mut x) } == 1 {
                return Some(x);
            }
        }
    }
    if has_cpuid_bit(1, 0, false, 30) {
        // RDRAND
        for _ in 0..16 {
            if unsafe { core::arch::x86_64::_rdrand64_step(&mut x) } == 1 {
                return Some(x);
            }
        }
    }
    None
}

impl Rng {
    fn mix(&mut self, data: &[u8]) {
        let mut h = Sha256::new();
        h.update(self.pool);
        h.update(data);
        self.pool = h.finalize().into();
        self.fresh = self.fresh.saturating_add(1);
    }

    /// Fold staged IRQ-time events into the pool.
    fn drain_stage(&mut self) {
        if STAGED.swap(0, Ordering::Relaxed) > 0 {
            let v = STAGE.swap(0, Ordering::Relaxed);
            self.mix(&v.to_le_bytes());
        }
    }

    /// Re-derive the generator key from the pool.
    fn reseed(&mut self) {
        let mut h = Sha256::new();
        h.update(b"thos-rng-reseed");
        h.update(self.pool);
        h.update(self.counter.to_le_bytes());
        let k: [u8; 32] = h.finalize().into();
        for i in 0..8 {
            self.key[i] = u32::from_le_bytes(k[i * 4..i * 4 + 4].try_into().unwrap());
        }
        self.pool = k; // pool forgets nothing it needs, but is no longer the raw key
        self.fresh = 0;
        self.seeded = true;
    }

    fn fill(&mut self, out: &mut [u8]) {
        self.drain_stage();
        if !self.seeded || self.fresh >= 64 {
            self.reseed();
        }
        let mut done = 0;
        while done < out.len() {
            // Fast key erasure: block 0 of each request-round becomes the *next* key,
            // block 1 is the output.
            let blk0 = chacha20_block(&self.key, self.counter as u32, &[(self.counter >> 32) as u32, 0x7468_6f73, 0]);
            self.counter += 1;
            for i in 0..8 {
                self.key[i] = u32::from_le_bytes(blk0[i * 4..i * 4 + 4].try_into().unwrap());
            }
            let blk = chacha20_block(&self.key, self.counter as u32, &[(self.counter >> 32) as u32, 0x7468_6f73, 1]);
            self.counter += 1;
            let n = (out.len() - done).min(64);
            out[done..done + n].copy_from_slice(&blk[..n]);
            done += n;
        }
    }
}

/// Seed the generator at boot: hardware RNG words, the RTC, the TSC and timing jitter.
pub fn init() {
    // A broken generator would silently hand out bad randomness: refuse to boot instead.
    assert!(chacha20_known_answer(), "ChaCha20 failed its RFC 8439 known-answer test");
    let mut r = RNG.lock();
    let mut hw = 0;
    for _ in 0..8 {
        if let Some(x) = hw_random64() {
            r.mix(&x.to_le_bytes());
            hw += 1;
        }
    }
    r.mix(&rdtsc().to_le_bytes());
    if let Some(t) = crate::rtc::read_unix() {
        r.mix(&t.to_le_bytes());
    }
    // Timing jitter: read the (slow, variable-latency) PIT counter and RTC register a few
    // hundred times and mix the TSC deltas.
    let mut last = rdtsc();
    let mut acc = [0u8; 8];
    for i in 0..512u32 {
        unsafe {
            core::arch::asm!("out 0x43, al", in("al") 0u8, options(nomem, nostack));
            let lo: u8;
            core::arch::asm!("in al, 0x40", out("al") lo, options(nomem, nostack));
            core::arch::asm!("out 0x70, al", in("al") 0u8, options(nomem, nostack));
            let _s: u8;
            core::arch::asm!("in al, 0x71", out("al") _s, options(nomem, nostack));
            acc[(i % 8) as usize] ^= lo;
        }
        let now = rdtsc();
        let d = now.wrapping_sub(last);
        last = now;
        acc[((i + 3) % 8) as usize] ^= d as u8 ^ (d >> 8) as u8;
        if i % 32 == 31 {
            r.mix(&acc);
        }
    }
    r.reseed();
    crate::kprintln!(
        "THOS: random           ChaCha20 CSPRNG seeded ({} hardware words, {} jitter samples; {})",
        hw,
        512,
        if hw > 0 { "RDSEED/RDRAND present" } else { "no hardware RNG — jitter only" }
    );
}

/// Mix an unpredictable event (a timestamp, a scancode, an IRQ count ...) into the
/// pool. Safe and cheap to call from any context, including interrupt handlers.
pub fn add_event(v: u64) {
    let t = rdtsc();
    STAGE.fetch_xor(v.rotate_left(17) ^ t.rotate_left(5), Ordering::Relaxed);
    STAGED.fetch_add(1, Ordering::Relaxed);
}

/// Fill `out` with cryptographically secure random bytes.
pub fn fill(out: &mut [u8]) {
    RNG.lock().fill(out);
}

pub fn u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}
