// SPDX-License-Identifier: GPL-2.0-or-later
//! Deterministic token sampling. The RNG (xorshift64*) and the top-k / softmax
//! procedure are byte-for-byte the same as `ml/train/sample.py`, so a Rust run
//! and the Python reference produce the identical token sequence from the same
//! seed.

use alloc::vec::Vec;

use crate::math::{argmax, softmax};
use crate::model::Model;

/// xorshift64* — tiny, deterministic, good enough for sampling.
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed } }
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `[0, 1)` from the top 24 bits.
    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u32 << 24) as f32)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SamplerConfig {
    pub temperature: f32,
    pub top_k: usize,
    pub max_tokens: usize,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        SamplerConfig { temperature: 0.8, top_k: 40, max_tokens: 64 }
    }
}

pub struct Sampler {
    cfg: SamplerConfig,
    rng: Rng,
}

impl Sampler {
    pub fn new(cfg: SamplerConfig, seed: u64) -> Self {
        Sampler { cfg, rng: Rng::new(seed) }
    }

    /// One next token from `logits` (consumed/mutated as scratch).
    pub fn pick(&mut self, logits: &mut [f32]) -> u16 {
        if self.cfg.temperature <= 0.0 {
            return argmax(logits) as u16;
        }
        let inv_t = 1.0 / self.cfg.temperature;
        for x in logits.iter_mut() {
            *x *= inv_t;
        }
        let k = self.cfg.top_k.clamp(1, logits.len());

        // Deterministic top-k: pick the max k times, ties -> lowest index.
        let mut chosen: Vec<(usize, f32)> = Vec::with_capacity(k);
        let mut used = alloc::vec![false; logits.len()];
        for _ in 0..k {
            let mut bi = usize::MAX;
            let mut bv = f32::NEG_INFINITY;
            for (i, &v) in logits.iter().enumerate() {
                if !used[i] && v > bv {
                    bv = v;
                    bi = i;
                }
            }
            used[bi] = true;
            chosen.push((bi, bv));
        }

        let mut probs: Vec<f32> = chosen.iter().map(|&(_, v)| v).collect();
        softmax(&mut probs);

        let u = self.rng.next_f32();
        let mut acc = 0.0f32;
        for (j, &p) in probs.iter().enumerate() {
            acc += p;
            if u < acc {
                return chosen[j].0 as u16;
            }
        }
        chosen[chosen.len() - 1].0 as u16
    }

    /// Autoregressively extend `tokens` in place by up to `max_tokens`, clamping
    /// the context to the model's `block_size`.
    pub fn generate(&mut self, model: &Model, tokens: &mut Vec<u16>) {
        let bs = model.cfg.block_size;
        for _ in 0..self.cfg.max_tokens {
            let start = tokens.len().saturating_sub(bs);
            let mut logits = model.forward(&tokens[start..]);
            let next = self.pick(&mut logits);
            tokens.push(next);
        }
    }
}
