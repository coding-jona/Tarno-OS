# SPDX-License-Identifier: GPL-2.0-or-later
"""Export a trained checkpoint to the `.tlm` weight format.

    python ml/train/export.py --ckpt out/latest.pt --out spike-1m.tlm

Then cross-check the Rust engine against the numpy reference on the real weights:

    python ml/train/ref_forward.py --weights spike-1m.tlm --prompt "The "
    cargo run -p thos-lm --example generate --target x86_64-unknown-linux-gnu -- \
        --weights spike-1m.tlm --prompt "The "
"""

from __future__ import annotations

import argparse
import json
import os

import torch

from model import GPT, ModelConfig
from tlm import Config, write_tlm

HERE = os.path.dirname(os.path.abspath(__file__))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ckpt", default="out/latest.pt")
    ap.add_argument("--out", default="spike.tlm")
    ap.add_argument("--tokenizer", default=os.path.join(HERE, "data", "tokenizer.json"))
    args = ap.parse_args()

    tok_kind, merges = 0, None
    if os.path.exists(args.tokenizer):
        with open(args.tokenizer) as fh:
            td = json.load(fh)
        tok_kind = td.get("kind", 0)
        merges = td.get("merges") or None

    blob = torch.load(args.ckpt, map_location="cpu")
    mc = blob["cfg"]["model"]
    model = GPT(ModelConfig(
        n_layer=mc["n_layer"], n_head=mc["n_head"], n_embd=mc["n_embd"],
        block_size=mc["block_size"], vocab_size=mc["vocab_size"],
        dropout=0.0, norm_eps=mc.get("norm_eps", 1e-5),
    ))
    model.load_state_dict(blob["model"])
    model.eval()

    cfg = Config(
        n_layer=mc["n_layer"], n_head=mc["n_head"], n_embd=mc["n_embd"],
        block_size=mc["block_size"], vocab_size=mc["vocab_size"],
        norm_eps=mc.get("norm_eps", 1e-5),
        tokenizer_kind=tok_kind, merges=merges,
    )
    write_tlm(args.out, cfg, model.export_tensors())
    print(f"wrote {args.out} ({os.path.getsize(args.out):,} bytes) from step {blob['step']} "
          f"(tokenizer kind {tok_kind}, vocab {cfg.vocab_size})")


if __name__ == "__main__":
    main()
