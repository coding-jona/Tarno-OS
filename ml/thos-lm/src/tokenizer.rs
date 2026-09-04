// SPDX-License-Identifier: GPL-2.0-or-later
//! Byte-level tokenizer: raw byte (kind 0) or byte-level BPE (kind 1). The BPE
//! merge table is embedded in the `.tlm` file, so encoding a prompt and
//! decoding output need no side data. Matches `ml/train/bpe.py`.

use alloc::vec::Vec;

pub struct Tokenizer {
    /// `merges[k] = (a, b)` — merging ids `a`,`b` yields id `256 + k`.
    merges: Vec<(u32, u32)>,
    /// `expand[id]` = the raw bytes `id` stands for (ids 0..256 are 1 byte).
    expand: Vec<Vec<u8>>,
    pub vocab_size: usize,
}

impl Tokenizer {
    pub fn new(merges: Vec<(u32, u32)>) -> Self {
        let mut expand: Vec<Vec<u8>> = (0u16..256).map(|b| alloc::vec![b as u8]).collect();
        for &(a, b) in &merges {
            let mut v = expand[a as usize].clone();
            v.extend_from_slice(&expand[b as usize]);
            expand.push(v);
        }
        let vocab_size = 256 + merges.len();
        Tokenizer { merges, expand, vocab_size }
    }

    pub fn is_byte(&self) -> bool {
        self.merges.is_empty()
    }

    /// Encode bytes to token ids. Byte mode = identity. BPE: split into maximal
    /// whitespace / non-whitespace runs (matching `bpe.py`'s `\s+|\S+`), then
    /// per chunk repeatedly apply the lowest-rank applicable merge — merges
    /// never cross a chunk boundary.
    pub fn encode(&self, data: &[u8]) -> Vec<u16> {
        if self.merges.is_empty() {
            return data.iter().map(|&b| b as u16).collect();
        }
        let is_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C);
        let mut out: Vec<u16> = Vec::new();
        let mut i = 0usize;
        while i < data.len() {
            let ws = is_ws(data[i]);
            let mut j = i + 1;
            while j < data.len() && is_ws(data[j]) == ws {
                j += 1;
            }
            self.encode_chunk(&data[i..j], &mut out);
            i = j;
        }
        out
    }

    fn encode_chunk(&self, chunk: &[u8], out: &mut Vec<u16>) {
        let mut ids: Vec<u32> = chunk.iter().map(|&b| b as u32).collect();
        loop {
            let mut best: Option<usize> = None;
            let mut best_rank = usize::MAX;
            for w in 0..ids.len().saturating_sub(1) {
                let pair = (ids[w], ids[w + 1]);
                if let Some(r) = self.merges.iter().position(|&m| m == pair) {
                    if r < best_rank {
                        best_rank = r;
                        best = Some(w);
                    }
                }
            }
            match best {
                Some(w) => {
                    ids[w] = 256 + best_rank as u32;
                    ids.remove(w + 1);
                }
                None => break,
            }
        }
        out.extend(ids.iter().map(|&x| x as u16));
    }

    /// Decode token ids back to bytes.
    pub fn decode(&self, tokens: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for &t in tokens {
            if let Some(bytes) = self.expand.get(t as usize) {
                out.extend_from_slice(bytes);
            }
        }
        out
    }
}
