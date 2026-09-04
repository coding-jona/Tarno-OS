# SPDX-License-Identifier: GPL-2.0-or-later
"""The `.tlm` weight container + a numpy-only reference forward pass.

This module is the single source of truth for the on-disk format and the exact
arithmetic of the forward pass. `thos-lm` (Rust) must match `numpy_forward`
here to within ~1e-4; the golden test enforces it.

Format: see `ml/thos-lm/src/tlm.rs` docstring (kept in sync by hand).
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

import numpy as np

MAGIC = b"TLM1"
VERSION = 1
HEADER_LEN = 44

# Tensor order, one entry per tensor. Per-layer tensors are expanded n_layer
# times between `wpe` and `lnf`. Shapes use PyTorch nn.Linear convention:
# weight is [out, in], y = x @ weight.T + bias.
_PRE = ["wte", "wpe"]
_LAYER = [
    "ln1_w", "ln1_b",
    "qkv_w", "qkv_b",
    "proj_w", "proj_b",
    "ln2_w", "ln2_b",
    "fc_w", "fc_b",
    "mlpproj_w", "mlpproj_b",
]
_POST = ["lnf_w", "lnf_b"]

GELU_C = np.float32(0.7978845608028654)  # sqrt(2/pi)


@dataclass
class Config:
    n_layer: int
    n_head: int
    n_embd: int
    block_size: int
    vocab_size: int
    norm_eps: float = 1e-5

    @property
    def head_dim(self) -> int:
        return self.n_embd // self.n_head

    def shapes(self) -> dict[str, tuple[int, ...]]:
        c, v, t = self.n_embd, self.vocab_size, self.block_size
        s = {"wte": (v, c), "wpe": (t, c)}
        for li in range(self.n_layer):
            p = f"h{li}."
            s.update({
                p + "ln1_w": (c,), p + "ln1_b": (c,),
                p + "qkv_w": (3 * c, c), p + "qkv_b": (3 * c,),
                p + "proj_w": (c, c), p + "proj_b": (c,),
                p + "ln2_w": (c,), p + "ln2_b": (c,),
                p + "fc_w": (4 * c, c), p + "fc_b": (4 * c,),
                p + "mlpproj_w": (c, 4 * c), p + "mlpproj_b": (c,),
            })
        s.update({"lnf_w": (c,), "lnf_b": (c,)})
        return s

    def ordered_keys(self) -> list[str]:
        keys = list(_PRE)
        for li in range(self.n_layer):
            keys += [f"h{li}.{name}" for name in _LAYER]
        keys += _POST
        return keys


def write_tlm(path: str, cfg: Config, tensors: dict[str, np.ndarray]) -> None:
    shapes = cfg.shapes()
    hdr = (
        MAGIC
        + struct.pack("<I", VERSION)
        + struct.pack("<IIIII", cfg.n_layer, cfg.n_head, cfg.n_embd,
                      cfg.block_size, cfg.vocab_size)
        + struct.pack("<I", 1)          # flags
        + struct.pack("<I", 0)          # tokenizer_kind
        + struct.pack("<f", cfg.norm_eps)
        + struct.pack("<I", 0)          # reserved
    )
    assert len(hdr) == HEADER_LEN, len(hdr)
    with open(path, "wb") as fh:
        fh.write(hdr)
        for key in cfg.ordered_keys():
            arr = np.ascontiguousarray(tensors[key], dtype="<f4")
            assert arr.shape == shapes[key], (key, arr.shape, shapes[key])
            fh.write(arr.tobytes())


def read_tlm(path: str) -> tuple[Config, dict[str, np.ndarray]]:
    with open(path, "rb") as fh:
        blob = fh.read()
    assert blob[:4] == MAGIC, "bad magic"
    (version,) = struct.unpack_from("<I", blob, 4)
    assert version == VERSION, version
    n_layer, n_head, n_embd, block_size, vocab_size = struct.unpack_from("<IIIII", blob, 8)
    (norm_eps,) = struct.unpack_from("<f", blob, 36)
    cfg = Config(n_layer, n_head, n_embd, block_size, vocab_size, float(norm_eps))
    shapes = cfg.shapes()
    off = HEADER_LEN
    tensors: dict[str, np.ndarray] = {}
    for key in cfg.ordered_keys():
        shp = shapes[key]
        n = int(np.prod(shp))
        arr = np.frombuffer(blob, dtype="<f4", count=n, offset=off).reshape(shp).astype(np.float32)
        tensors[key] = arr
        off += n * 4
    assert off == len(blob), (off, len(blob))
    return cfg, tensors


# --- reference forward pass (numpy, float32 throughout) ---

def _layernorm(v, w, b, eps):
    mu = v.mean(axis=-1, keepdims=True)
    var = ((v - mu) ** 2).mean(axis=-1, keepdims=True)
    return ((v - mu) / np.sqrt(var + np.float32(eps))) * w + b


def _gelu_tanh(x):
    return np.float32(0.5) * x * (np.float32(1.0) + np.tanh(GELU_C * (x + np.float32(0.044715) * x ** 3)))


def _softmax(s):
    s = s - s.max(axis=-1, keepdims=True)
    e = np.exp(s)
    return e / e.sum(axis=-1, keepdims=True)


def numpy_forward(cfg: Config, w: dict[str, np.ndarray], tokens: list[int]) -> np.ndarray:
    """Return logits (float32, shape [vocab]) for the position after `tokens`."""
    c, h, hd = cfg.n_embd, cfg.n_head, cfg.head_dim
    n = min(len(tokens), cfg.block_size)
    toks = np.asarray(tokens[:n], dtype=np.int64)
    eps = cfg.norm_eps
    scale = np.float32(1.0 / np.sqrt(hd))

    x = (w["wte"][toks] + w["wpe"][:n]).astype(np.float32)  # [n, c]

    for li in range(cfg.n_layer):
        p = f"h{li}."
        hln = _layernorm(x, w[p + "ln1_w"], w[p + "ln1_b"], eps)
        qkv = hln @ w[p + "qkv_w"].T + w[p + "qkv_b"]        # [n, 3c]
        q, k, v = qkv[:, :c], qkv[:, c:2 * c], qkv[:, 2 * c:]
        q = q.reshape(n, h, hd)
        k = k.reshape(n, h, hd)
        v = v.reshape(n, h, hd)
        att = np.zeros((n, h, hd), dtype=np.float32)
        for t in range(n):
            for hh in range(h):
                sc = (q[t, hh] @ k[: t + 1, hh].T) * scale   # [t+1]
                pr = _softmax(sc)
                att[t, hh] = pr @ v[: t + 1, hh]
        att = att.reshape(n, c)
        x = x + (att @ w[p + "proj_w"].T + w[p + "proj_b"])

        h2 = _layernorm(x, w[p + "ln2_w"], w[p + "ln2_b"], eps)
        f1 = _gelu_tanh(h2 @ w[p + "fc_w"].T + w[p + "fc_b"])
        x = x + (f1 @ w[p + "mlpproj_w"].T + w[p + "mlpproj_b"])

    xf = _layernorm(x[-1], w["lnf_w"], w["lnf_b"], eps)
    return (xf @ w["wte"].T).astype(np.float32)
