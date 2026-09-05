// SPDX-License-Identifier: GPL-2.0-or-later
//! The handful of `f32` ops the forward pass needs. Scalar, hand-written; the
//! compiler may auto-vectorise but there are no SIMD intrinsics. Transcendentals
//! come from `libm` (soft-float, no CPU-feature dispatch).

use alloc::vec::Vec;

/// `sqrt(2/pi)`, the GELU tanh-approximation constant.
const GELU_C: f32 = 0.797_884_560_803_017_1;

/// `out[o] = sum_i x[i] * w[o*in + i] + bias[o]`  (PyTorch `nn.Linear` layout:
/// `w` is `[out, in]` row-major, `y = x @ w.T + bias`). `bias` may be empty.
pub fn linear(x: &[f32], w: &[f32], bias: &[f32], out: &mut [f32]) {
    let in_dim = x.len();
    debug_assert_eq!(w.len(), out.len() * in_dim);
    for (o, y) in out.iter_mut().enumerate() {
        let row = &w[o * in_dim..o * in_dim + in_dim];
        let mut acc = 0.0f32;
        for i in 0..in_dim {
            acc += x[i] * row[i];
        }
        *y = acc + if bias.is_empty() { 0.0 } else { bias[o] };
    }
}

/// LayerNorm over the whole vector: `(v - mean) / sqrt(var + eps) * w + b`.
pub fn layernorm(v: &[f32], w: &[f32], b: &[f32], eps: f32, out: &mut [f32]) {
    let n = v.len() as f32;
    let mean = v.iter().sum::<f32>() / n;
    let var = v.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / n;
    let inv = 1.0 / libm::sqrtf(var + eps);
    for i in 0..v.len() {
        out[i] = (v[i] - mean) * inv * w[i] + b[i];
    }
}

/// In-place softmax over `s`.
pub fn softmax(s: &mut [f32]) {
    let m = s.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for x in s.iter_mut() {
        *x = libm::expf(*x - m);
        sum += *x;
    }
    let inv = 1.0 / sum;
    for x in s.iter_mut() {
        *x *= inv;
    }
}

/// GELU, tanh approximation (matches PyTorch `nn.GELU(approximate="tanh")`).
pub fn gelu_tanh_inplace(v: &mut [f32]) {
    for x in v.iter_mut() {
        let x3 = *x * *x * *x;
        *x = 0.5 * *x * (1.0 + libm::tanhf(GELU_C * (*x + 0.044_715 * x3)));
    }
}

/// Dot product.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = 0.0f32;
    for i in 0..a.len() {
        acc += a[i] * b[i];
    }
    acc
}

/// `argmax` with ties broken by the lowest index.
pub fn argmax(v: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_v = v[0];
    for (i, &x) in v.iter().enumerate().skip(1) {
        if x > best_v {
            best_v = x;
            best = i;
        }
    }
    best
}

/// Scratch vector of `n` zeros.
pub fn zeros(n: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(n);
    v.resize(n, 0.0);
    v
}
