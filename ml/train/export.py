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
import os

import torch

from model import GPT, ModelConfig
from tlm import Config, write_tlm


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ckpt", default="out/latest.pt")
    ap.add_argument("--out", default="spike.tlm")
    args = ap.parse_args()

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
    )
    write_tlm(args.out, cfg, model.export_tensors())
    print(f"wrote {args.out} ({os.path.getsize(args.out):,} bytes) from step {blob['step']}")


if __name__ == "__main__":
    main()
