# SPDX-License-Identifier: GPL-2.0-or-later
"""Evaluate a `.tlm` model: perplexity + a teacher-forced next-token probe +
a short sample. numpy-only (no torch) — uses tlm.py:numpy_forward.

    python ml/train/eval.py --weights small-30m.tlm [--windows 200]
"""

from __future__ import annotations

import argparse
import math
import os

import numpy as np

from bpe import Tokenizer
from tlm import numpy_forward, read_tlm

HERE = os.path.dirname(os.path.abspath(__file__))


def _softmax_last(logits: np.ndarray) -> np.ndarray:
    z = logits - logits.max()
    e = np.exp(z)
    return e / e.sum()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--val", default=os.path.join(HERE, "data", "val.bin"))
    ap.add_argument("--windows", type=int, default=200, help="eval contexts to score")
    ap.add_argument("--probe-ctx", type=int, default=128)
    ap.add_argument("--probe-gen", type=int, default=64)
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()

    cfg, w = read_tlm(args.weights)
    val = np.fromfile(args.val, dtype=np.uint16).astype(np.int64)
    bs = cfg.block_size
    print(f"model L{cfg.n_layer} H{cfg.n_head} C{cfg.n_embd} T{bs} V{cfg.vocab_size} "
          f"| tokenizer kind {cfg.tokenizer_kind} | val {len(val):,} tokens")

    tok = Tokenizer(cfg.merges or [])
    # bytes-per-token compression, to convert token perplexity -> bits/byte
    bpt = 1.0
    if cfg.tokenizer_kind == 1:
        sample_ids = val[: min(len(val), 20000)].tolist()
        bpt = len(tok.decode(sample_ids)) / max(1, len(sample_ids))

    rng = np.random.default_rng(args.seed)

    # --- perplexity: score the last token of `windows` random contexts ---
    nll = 0.0
    n = 0
    for _ in range(args.windows):
        i = int(rng.integers(0, len(val) - bs - 1))
        ctx = val[i : i + bs]
        tgt = int(val[i + bs])
        logits = numpy_forward(cfg, w, ctx.tolist())
        p = _softmax_last(logits)[tgt]
        nll += -math.log(max(p, 1e-12))
        n += 1
    ppl_tok = math.exp(nll / n)
    bits_per_byte = (nll / n) / math.log(2) / bpt
    baseline = math.log(cfg.vocab_size)
    print(f"\nperplexity : {ppl_tok:.3f}  (nll {nll/n:.4f} nats/tok, "
          f"baseline exp({baseline:.2f})={math.exp(baseline):.0f})")
    print(f"bits/byte  : {bits_per_byte:.3f}  (uniform bytes = 8.0)")

    # --- teacher-forced next-token accuracy over a held-out passage ---
    i = int(rng.integers(0, len(val) - args.probe_ctx - args.probe_gen - 1))
    hit = 0
    for k in range(args.probe_gen):
        ctx = val[i + k : i + k + args.probe_ctx]
        pred = int(np.argmax(numpy_forward(cfg, w, ctx.tolist())))
        if pred == int(val[i + k + args.probe_ctx]):
            hit += 1
    print(f"\nnext-token accuracy (teacher-forced, {args.probe_gen} steps): "
          f"{hit}/{args.probe_gen} = {100*hit/args.probe_gen:.1f}%")

    # --- free-running sample from a val prompt ---
    ctx = list(val[i : i + 48])
    gen = list(ctx)
    for _ in range(120):
        logits = numpy_forward(cfg, w, gen[-bs:])
        z = logits / 0.8
        z -= z.max()
        p = np.exp(z)
        p /= p.sum()
        gen.append(int(rng.choice(len(p), p=p)))
    print("\n--- prompt (from val) ---")
    print(repr(tok.decode(ctx).decode("utf-8", "replace")))
    print("--- continuation ---")
    print(repr(tok.decode(gen[len(ctx):]).decode("utf-8", "replace")))


if __name__ == "__main__":
    main()
