// SPDX-License-Identifier: GPL-2.0-or-later
//! The decoder-only forward pass. No KV cache in v0 — the whole context is
//! recomputed each step, which is fine for the small models this crate targets
//! and keeps the code a direct transcription of the reference (`ml/train/tlm.py`
//! `numpy_forward`).

use alloc::vec::Vec;

use crate::math::{argmax, dot, gelu_tanh_inplace, layernorm, linear, softmax, zeros};
use crate::tlm::{Config, Layout, LayerRef, TlmError, Weights};

pub struct Model {
    pub cfg: Config,
    w: Vec<f32>,
    lay: Layout,
    lref: LayerRef,
}

impl Model {
    pub fn load(bytes: &[u8]) -> Result<Model, TlmError> {
        let Weights { cfg, data } = Weights::parse(bytes)?;
        let lay = Layout::new(cfg);
        let lref = LayerRef::new(cfg.n_embd);
        Ok(Model { cfg, w: data, lay, lref })
    }

    fn s(&self, start: usize, len: usize) -> &[f32] {
        &self.w[start..start + len]
    }

    /// Logits over the vocabulary for the position *after* `tokens`.
    /// `tokens.len()` must be in `1..=block_size`; ids must be `< vocab_size`.
    pub fn forward(&self, tokens: &[u16]) -> Vec<f32> {
        let c = self.cfg.n_embd;
        let h = self.cfg.n_head;
        let hd = self.cfg.head_dim();
        let n = tokens.len().min(self.cfg.block_size).max(1);
        let eps = self.cfg.norm_eps;
        let scale = 1.0 / libm::sqrtf(hd as f32);

        // --- embeddings: x[t] = wte[tok] + wpe[t] ---
        let wte = self.s(0, self.cfg.vocab_size * c);
        let wpe = self.s(self.lay.wpe, self.cfg.block_size * c);
        let mut x = zeros(n * c);
        for t in 0..n {
            let tok = tokens[t] as usize;
            let e = &wte[tok * c..tok * c + c];
            let p = &wpe[t * c..t * c + c];
            for i in 0..c {
                x[t * c + i] = e[i] + p[i];
            }
        }

        let stride = self.cfg.layer_elems();
        let mut hbuf = zeros(c);
        let mut qkv = zeros(3 * c);
        let mut att_out = zeros(c);
        let mut proj = zeros(c);
        let mut f1 = zeros(4 * c);
        let mut f2 = zeros(c);
        // per-(head) attention scratch, sized to the longest row.
        let mut scores = zeros(n);

        for l in 0..self.cfg.n_layer {
            let base = self.lay.layer0 + l * stride;
            let r = &self.lref;
            let ln1_w = self.s(base + r.ln1_w, c);
            let ln1_b = self.s(base + r.ln1_b, c);
            let qkv_w = self.s(base + r.qkv_w, 3 * c * c);
            let qkv_b = self.s(base + r.qkv_b, 3 * c);
            let proj_w = self.s(base + r.proj_w, c * c);
            let proj_b = self.s(base + r.proj_b, c);
            let ln2_w = self.s(base + r.ln2_w, c);
            let ln2_b = self.s(base + r.ln2_b, c);
            let fc_w = self.s(base + r.fc_w, 4 * c * c);
            let fc_b = self.s(base + r.fc_b, 4 * c);
            let mp_w = self.s(base + r.mlpproj_w, 4 * c * c);
            let mp_b = self.s(base + r.mlpproj_b, c);

            // qkv for every position (needed because attention is all-to-all).
            let mut q = zeros(n * c);
            let mut k = zeros(n * c);
            let mut v = zeros(n * c);
            for t in 0..n {
                layernorm(&x[t * c..t * c + c], ln1_w, ln1_b, eps, &mut hbuf);
                linear(&hbuf, qkv_w, qkv_b, &mut qkv);
                q[t * c..t * c + c].copy_from_slice(&qkv[0..c]);
                k[t * c..t * c + c].copy_from_slice(&qkv[c..2 * c]);
                v[t * c..t * c + c].copy_from_slice(&qkv[2 * c..3 * c]);
            }

            // causal multi-head attention, write y back through the residual.
            for t in 0..n {
                for hh in 0..h {
                    let qh = &q[t * c + hh * hd..t * c + hh * hd + hd];
                    let row = &mut scores[..t + 1];
                    for s in 0..=t {
                        let kh = &k[s * c + hh * hd..s * c + hh * hd + hd];
                        row[s] = dot(qh, kh) * scale;
                    }
                    softmax(row);
                    let dst = &mut att_out[hh * hd..hh * hd + hd];
                    for d in dst.iter_mut() {
                        *d = 0.0;
                    }
                    for s in 0..=t {
                        let vh = &v[s * c + hh * hd..s * c + hh * hd + hd];
                        let p = row[s];
                        for d in 0..hd {
                            dst[d] += p * vh[d];
                        }
                    }
                }
                linear(&att_out, proj_w, proj_b, &mut proj);
                for i in 0..c {
                    x[t * c + i] += proj[i];
                }
            }

            // MLP.
            for t in 0..n {
                layernorm(&x[t * c..t * c + c], ln2_w, ln2_b, eps, &mut hbuf);
                linear(&hbuf, fc_w, fc_b, &mut f1);
                gelu_tanh_inplace(&mut f1);
                linear(&f1, mp_w, mp_b, &mut f2);
                for i in 0..c {
                    x[t * c + i] += f2[i];
                }
            }
        }

        // final norm + tied output projection for the last position only.
        let lnf_w = self.s(self.lay.lnf, c);
        let lnf_b = self.s(self.lay.lnf + c, c);
        let last = &x[(n - 1) * c..(n - 1) * c + c];
        layernorm(last, lnf_w, lnf_b, eps, &mut hbuf);
        let mut logits = zeros(self.cfg.vocab_size);
        linear(&hbuf, wte, &[], &mut logits);
        logits
    }

    /// Greedy next token (ties → lowest id).
    pub fn greedy(&self, tokens: &[u16]) -> u16 {
        argmax(&self.forward(tokens)) as u16
    }
}
