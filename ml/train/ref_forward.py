# SPDX-License-Identifier: GPL-2.0-or-later
"""Numpy reference forward pass over a `.tlm` — the oracle the Rust engine is
checked against. No torch.

    python ml/train/ref_forward.py --weights spike-1m.tlm --prompt "The "
    python ml/train/ref_forward.py --weights toy.tlm --prompt-bytes 84,72,79,83 --dump

`--dump` prints every logit (one per line) so a diff against the Rust output is
trivial.
"""

from __future__ import annotations

import argparse

import numpy as np

from tlm import numpy_forward, read_tlm


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--prompt", default=None)
    ap.add_argument("--prompt-bytes", default=None, help="comma-separated byte ids")
    ap.add_argument("--dump", action="store_true")
    args = ap.parse_args()

    cfg, w = read_tlm(args.weights)
    if args.prompt_bytes:
        tokens = [int(x) for x in args.prompt_bytes.split(",")]
    else:
        tokens = list((args.prompt or "The ").encode("utf-8"))

    logits = numpy_forward(cfg, w, tokens[: cfg.block_size])
    am = int(np.argmax(logits))
    print(f"# n_ctx={min(len(tokens), cfg.block_size)} vocab={cfg.vocab_size} "
          f"argmax={am} top={logits[am]:.6f}")
    if args.dump:
        for x in logits:
            print(f"{x:.8e}")


if __name__ == "__main__":
    main()
