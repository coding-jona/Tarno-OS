// SPDX-License-Identifier: GPL-2.0-or-later
//! The `.tlm` weight container.
//!
//! Layout (all little-endian):
//!
//! ```text
//! offset  size  field
//!   0      4    magic  = b"TLM1"
//!   4      4    u32 version = 1
//!   8      4    u32 n_layer
//!  12      4    u32 n_head
//!  16      4    u32 n_embd            (C)
//!  20      4    u32 block_size        (T, max context)
//!  24      4    u32 vocab_size        (V)
//!  28      4    u32 flags             bit0 tied-output (always 1)
//!  32      4    u32 tokenizer_kind    0 = raw byte, 1 = byte-level BPE
//!  36      4    f32 norm_eps
//!  40      4    u32 tok_bytes         size of the tokenizer blob (0 for kind 0)
//!  44     ...   tokenizer blob (`tok_bytes`): for kind 1, `u32 n_merges` then
//!               `n_merges * (u32 left_id, u32 right_id)` in merge order —
//!               new token id = 256 + merge_index
//!  44+tb  ...   f32 tensors, back to back, in `TENSOR ORDER` below
//! ```
//!
//! TENSOR ORDER (shapes as PyTorch `nn.Linear`: `weight` is `[out, in]`,
//! `y = x @ weight.T + bias`):
//!
//! ```text
//!   wte   [V, C]        token embedding (also the tied output projection)
//!   wpe   [T, C]        learned positional embedding
//!   for layer in 0..n_layer:
//!     ln1_w [C]   ln1_b [C]
//!     qkv_w [3C, C]     qkv_b [3C]
//!     proj_w [C, C]     proj_b [C]
//!     ln2_w [C]   ln2_b [C]
//!     fc_w  [4C, C]     fc_b  [4C]
//!     mlpproj_w [C, 4C] mlpproj_b [C]
//!   lnf_w [C]   lnf_b [C]
//! ```

use alloc::vec::Vec;

pub const MAGIC: [u8; 4] = *b"TLM1";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlmError {
    TooShort,
    BadMagic,
    BadVersion,
    /// Header describes a model whose tensor bytes don't fit the file.
    SizeMismatch,
    /// A config field is zero or otherwise impossible.
    BadConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub n_layer: usize,
    pub n_head: usize,
    pub n_embd: usize,
    pub block_size: usize,
    pub vocab_size: usize,
    pub norm_eps: f32,
}

impl Config {
    /// Number of `f32` elements in one transformer block.
    pub fn layer_elems(&self) -> usize {
        let c = self.n_embd;
        // ln1(2C) + qkv(3C*C + 3C) + proj(C*C + C) + ln2(2C)
        //        + fc(4C*C + 4C) + mlpproj(4C*C + C)
        2 * c + (3 * c * c + 3 * c) + (c * c + c) + 2 * c + (4 * c * c + 4 * c) + (4 * c * c + c)
    }
    /// Total `f32` elements across every tensor.
    pub fn total_elems(&self) -> usize {
        let c = self.n_embd;
        self.vocab_size * c + self.block_size * c + self.n_layer * self.layer_elems() + 2 * c
    }
    pub fn head_dim(&self) -> usize {
        self.n_embd / self.n_head
    }
}

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A parsed `.tlm`: config plus every tensor decoded into one contiguous
/// `Vec<f32>` in `TENSOR ORDER`. (Decoded once at load so the rest of the crate
/// never touches raw bytes or worries about `&[u8]` alignment.)
pub struct Weights {
    pub cfg: Config,
    pub data: Vec<f32>,
    /// `tokenizer_kind` from the header (0 = byte, 1 = byte-BPE).
    pub tok_kind: u8,
    /// BPE merges in order; empty for kind 0. Token id `256 + k` == merge `k`.
    pub merges: Vec<(u32, u32)>,
}

impl Weights {
    pub fn parse(bytes: &[u8]) -> Result<Weights, TlmError> {
        if bytes.len() < HEADER_LEN {
            return Err(TlmError::TooShort);
        }
        if bytes[0..4] != MAGIC {
            return Err(TlmError::BadMagic);
        }
        if rd_u32(bytes, 4) != VERSION {
            return Err(TlmError::BadVersion);
        }
        let cfg = Config {
            n_layer: rd_u32(bytes, 8) as usize,
            n_head: rd_u32(bytes, 12) as usize,
            n_embd: rd_u32(bytes, 16) as usize,
            block_size: rd_u32(bytes, 20) as usize,
            vocab_size: rd_u32(bytes, 24) as usize,
            norm_eps: f32::from_bits(rd_u32(bytes, 36)),
        };
        if cfg.n_layer == 0
            || cfg.n_head == 0
            || cfg.n_embd == 0
            || cfg.block_size == 0
            || cfg.vocab_size == 0
            || cfg.n_embd % cfg.n_head != 0
            || !(cfg.norm_eps.is_finite() && cfg.norm_eps > 0.0)
        {
            return Err(TlmError::BadConfig);
        }

        // Tokenizer blob (between the header and the tensors).
        let tok_kind = rd_u32(bytes, 32) as u8;
        let tok_bytes = rd_u32(bytes, 40) as usize;
        if bytes.len() < HEADER_LEN + tok_bytes {
            return Err(TlmError::SizeMismatch);
        }
        let mut merges = Vec::new();
        if tok_bytes >= 4 {
            let n = rd_u32(bytes, HEADER_LEN) as usize;
            if 4 + n * 8 > tok_bytes {
                return Err(TlmError::SizeMismatch);
            }
            merges.reserve(n);
            for k in 0..n {
                let o = HEADER_LEN + 4 + k * 8;
                merges.push((rd_u32(bytes, o), rd_u32(bytes, o + 4)));
            }
        }

        let want = cfg.total_elems();
        let body = &bytes[HEADER_LEN + tok_bytes..];
        if body.len() < want * 4 {
            return Err(TlmError::SizeMismatch);
        }
        let mut data = Vec::with_capacity(want);
        for i in 0..want {
            let o = i * 4;
            data.push(f32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]]));
        }
        Ok(Weights { cfg, data, tok_kind, merges })
    }
}

/// Byte offsets (in `f32` elements) into `Weights::data` for each tensor of one
/// layer, plus the shared pre/post tensors.
pub struct Layout {
    pub cfg: Config,
    pub wpe: usize, // wte starts at 0
    pub layer0: usize,
    pub lnf: usize,
}

impl Layout {
    pub fn new(cfg: Config) -> Self {
        let c = cfg.n_embd;
        let wpe = cfg.vocab_size * c;
        let layer0 = wpe + cfg.block_size * c;
        let lnf = layer0 + cfg.n_layer * cfg.layer_elems();
        Layout { cfg, wpe, layer0, lnf }
    }
}

/// The tensor slice offsets *within one layer* (element indices relative to that
/// layer's base). Order matches `TENSOR ORDER`.
pub struct LayerRef {
    pub ln1_w: usize,
    pub ln1_b: usize,
    pub qkv_w: usize,
    pub qkv_b: usize,
    pub proj_w: usize,
    pub proj_b: usize,
    pub ln2_w: usize,
    pub ln2_b: usize,
    pub fc_w: usize,
    pub fc_b: usize,
    pub mlpproj_w: usize,
    pub mlpproj_b: usize,
}

impl LayerRef {
    pub fn new(c: usize) -> Self {
        let mut o = 0usize;
        let mut take = |n: usize| {
            let s = o;
            o += n;
            s
        };
        LayerRef {
            ln1_w: take(c),
            ln1_b: take(c),
            qkv_w: take(3 * c * c),
            qkv_b: take(3 * c),
            proj_w: take(c * c),
            proj_b: take(c),
            ln2_w: take(c),
            ln2_b: take(c),
            fc_w: take(4 * c * c),
            fc_b: take(4 * c),
            mlpproj_w: take(4 * c * c),
            mlpproj_b: take(c),
        }
    }
}
