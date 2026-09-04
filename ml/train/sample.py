# SPDX-License-Identifier: GPL-2.0-or-later
"""Reference sampler — numpy only, no torch. Byte-for-byte the same RNG and
top-k / softmax procedure as `ml/thos-lm/src/sample.rs`, so this and the Rust
`generate` example produce the identical token sequence from the same seed.

    python ml/train/sample.py --weights spike-1m.tlm --prompt "The " \
        --max-tokens 200 --temp 0.8 --top-k 40 --seed 1
"""

from __future__ import annotations

import argparse

import numpy as np

from tlm import numpy_forward, read_tlm

MASK64 = (1 << 64) - 1
MUL = 0x2545F4914F6CDD1D


class Rng:
    __slots__ = ("s",)

    def __init__(self, seed: int):
        self.s = seed if seed != 0 else 0x9E3779B97F4A7C15

    def next_u64(self) -> int:
        x = self.s
        x ^= x >> 12
        x ^= (x << 25) & MASK64
        x ^= x >> 27
        self.s = x
        return (x * MUL) & MASK64

    def next_f32(self) -> float:
        return (self.next_u64() >> 40) / float(1 << 24)


def pick(logits: np.ndarray, temp: float, top_k: int, rng: Rng) -> int:
    if temp <= 0.0:
        return int(np.argmax(logits))
    logits = (logits / np.float32(temp)).astype(np.float32)
    k = max(1, min(top_k, logits.size))

    order = []
    used = np.zeros(logits.size, dtype=bool)
    for _ in range(k):
        masked = np.where(used, -np.inf, logits)
        bi = int(np.argmax(masked))  # first-max on ties
        used[bi] = True
        order.append(bi)

    vals = np.array([logits[i] for i in order], dtype=np.float32)
    vals = vals - vals.max()
    e = np.exp(vals)
    probs = e / e.sum()

    u = rng.next_f32()
    acc = 0.0
    for j, p in enumerate(probs):
        acc += float(p)
        if u < acc:
            return order[j]
    return order[-1]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--prompt", default="The ")
    ap.add_argument("--max-tokens", type=int, default=200)
    ap.add_argument("--temp", type=float, default=0.8)
    ap.add_argument("--top-k", type=int, default=40)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--dump-tokens", action="store_true", help="print the token id list too")
    args = ap.parse_args()

    cfg, w = read_tlm(args.weights)
    toks = list(args.prompt.encode("utf-8"))
    rng = Rng(args.seed)
    for _ in range(args.max_tokens):
        ctx = toks[-cfg.block_size:]
        logits = numpy_forward(cfg, w, ctx)
        toks.append(pick(logits, args.temp, args.top_k, rng))

    text = bytes(t for t in toks if t < 256).decode("utf-8", errors="replace")
    print(text)
    if args.dump_tokens:
        print("\n# tokens:", " ".join(map(str, toks)))


if __name__ == "__main__":
    main()
