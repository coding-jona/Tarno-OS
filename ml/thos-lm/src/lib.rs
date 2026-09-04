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
//! See `docs/thos/ai.md` for the wider plan. Model: `f32` weights, learned
//! positional embeddings, tied output projection, tanh-approximation GELU;
//! tokenizer is raw byte (kind 0) or byte-level BPE with the merge table
//! embedded in the `.tlm` file (kind 1).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod math;
pub mod model;
pub mod sample;
pub mod tlm;
pub mod tokenizer;

pub use model::Model;
pub use sample::{Sampler, SamplerConfig};
pub use tlm::{Config, TlmError};
pub use tokenizer::Tokenizer;
