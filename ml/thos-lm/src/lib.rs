// SPDX-License-Identifier: GPL-2.0-or-later
//! `thos-lm` — a from-scratch, `#![no_std]` inference engine for a small
//! decoder-only ("GPT-style") language model.
//!
//! The model is trained off-device (PyTorch, CPU) and exported to the `.tlm`
//! weight format ([`tlm`]); this crate parses that format and runs the forward
//! pass ([`model`]) and sampling ([`sample`]) with only `core` + `alloc` +
//! `libm` — no `std`, no external tensor library, no SIMD intrinsics. One
//! `Model::forward` call runs to completion, so it is unaffected by the THOS
//! kernel's lack of FPU-state save across a preemption.
//!
//! See `docs/thos/ai.md` for the wider plan. This is the P0 "Proof of Life"
//! slice: byte-level tokenizer, `f32` weights, learned positional embeddings,
//! tied output projection, tanh-approximation GELU.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod math;
pub mod model;
pub mod sample;
pub mod tlm;

pub use model::Model;
pub use sample::{Sampler, SamplerConfig};
pub use tlm::{Config, TlmError};

/// Encode a UTF-8 / byte string to token ids (v0: identity byte tokenizer).
pub fn encode_bytes(s: &[u8], out: &mut alloc::vec::Vec<u16>) {
    out.clear();
    out.extend(s.iter().map(|&b| b as u16));
}

/// Decode token ids back to bytes (v0: identity byte tokenizer). Ids >= 256 are
/// dropped.
pub fn decode_bytes(tokens: &[u16]) -> alloc::vec::Vec<u8> {
    tokens.iter().filter(|&&t| t < 256).map(|&t| t as u8).collect()
}
