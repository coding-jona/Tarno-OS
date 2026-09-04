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

    blob = torch.load(args.ckpt, map_location="cpu")
    mc = blob["cfg"]["model"]

    # A tokenizer only belongs with a checkpoint if its vocab actually
    # matches: data/tokenizer.json is whatever the *last* prepare.py run
    # wrote, which may be a different run's BPE tokenizer entirely (e.g. a
    # 16k-vocab BPE table sitting around from a 30M run while exporting an
    # unrelated 256-vocab byte-tokenizer checkpoint) — attaching it blindly
    # produces a .tlm whose token ids run past the embedding table and
    # crashes thos-lm. Fall back to the raw-byte tokenizer (always valid)
    # rather than exporting something broken.
    tok_kind, merges = 0, None
    if os.path.exists(args.tokenizer):
        with open(args.tokenizer) as fh:
            td = json.load(fh)
        cand_kind = td.get("kind", 0)
        cand_merges = td.get("merges") or None
        cand_vocab = 256 + len(cand_merges) if cand_merges else 256
        if cand_vocab == mc["vocab_size"]:
            tok_kind, merges = cand_kind, cand_merges
        elif cand_kind != 0:
            print(f"note: ignoring {args.tokenizer} (vocab {cand_vocab}) — "
                  f"checkpoint's model vocab_size is {mc['vocab_size']}; "
                  "exporting with the raw-byte tokenizer instead")
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
