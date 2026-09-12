# SPDX-License-Identifier: GPL-2.0-or-later
"""Turns data/sft_raw/*.jsonl ({"messages":[{role,content},...]}) into packed
SFT training tensors, reusing the SAME BPE tokenizer.json a base model (e.g.
small-30m) was pre-trained with — no vocab change, so the pre-trained
embedding table stays fully compatible and SFT can just resume from it.

    python ml/train/prepare_sft.py [--val-frac 0.02]

ChatML-style framing (as literal text, not reserved token ids — see the
no-special-tokens tradeoff note below):

    <|im_start|>user
    <content><|im_end|>
    <|im_start|>assistant
    <content><|im_end|>

Loss masking: each conversation is tokenized turn-by-turn (not as one joined
string) so every token can be tagged with whether it belongs to an assistant
reply. Everything except assistant content + its closing <|im_end|> is
masked out (mask=0) in the *_mask.bin sidecar; train.py's --sft mode turns
mask==0 positions into ignore_index=-100 targets, so gradients only ever come
from the assistant's own words — the whole point of SFT over plain LM loss.

Why literal text instead of reserved special-token ids: adding real new
vocab ids would mean resizing (surgering) the embedding/lm_head matrices of
an already-trained checkpoint. Treating "<|im_start|>", "user", "<|im_end|>"
etc. as ordinary bytes costs a few extra tokens per turn but needs zero
checkpoint surgery and zero tokenizer retraining — the existing BPE merges
and the existing small-30m.tlm stay exactly as they are.

Writes:
    data/sft_train.bin / sft_val.bin        uint16 token ids
    data/sft_train_mask.bin / sft_val_mask.bin   uint8 (1 = compute loss)
"""
from __future__ import annotations

import argparse
import glob
import json
import os

import numpy as np

from bpe import Tokenizer

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data")
RAW_DIR = os.path.join(DATA, "sft_raw")


def encode_piece(tok: Tokenizer, text: str) -> list[int]:
    return tok.encode(text.encode("utf-8"))


def build_example(tok: Tokenizer, messages: list[dict]) -> tuple[list[int], list[int]]:
    """One conversation -> (token ids, mask) with only assistant turns (incl.
    their closing <|im_end|>) contributing to the loss."""
    ids: list[int] = []
    mask: list[int] = []

    def add(text: str, loss: bool) -> None:
        piece_ids = encode_piece(tok, text)
        ids.extend(piece_ids)
        mask.extend([1 if loss else 0] * len(piece_ids))

    for m in messages:
        role = m["role"]
        content = m["content"].strip()
        if role not in ("user", "assistant"):
            continue
        add(f"<|im_start|>{role}\n", False)
        if role == "assistant":
            add(content, True)
            add("<|im_end|>\n", True)  # model must learn to emit the stop marker itself
        else:
            add(content, False)
            add("<|im_end|>\n", False)
    return ids, mask


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--val-frac", type=float, default=0.02)
    ap.add_argument("--tokenizer", default=os.path.join(DATA, "tokenizer.json"))
    args = ap.parse_args()

    if not os.path.isfile(args.tokenizer):
        print(f"no tokenizer at {args.tokenizer} — run prepare.py first (must match the base model)")
        return 1
    tok = Tokenizer.load(args.tokenizer)

    files = sorted(glob.glob(os.path.join(RAW_DIR, "*.jsonl")))
    if not files:
        print(f"no *.jsonl under {RAW_DIR} — run fetch_sft.py and/or gen_thos_custom.py first")
        return 1

    all_ids: list[int] = []
    all_mask: list[int] = []
    n_convos = 0
    for path in files:
        n_here = 0
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                rec = json.loads(line)
                ids, mask = build_example(tok, rec["messages"])
                if not ids or not any(mask):
                    continue  # no assistant turn to learn from -- skip
                all_ids.extend(ids)
                all_mask.extend(mask)
                n_here += 1
        print(f"  {os.path.basename(path):30} {n_here:>6} conversations")
        n_convos += n_here

    ids_arr = np.asarray(all_ids, dtype=np.uint16)
    mask_arr = np.asarray(all_mask, dtype=np.uint8)
    assert len(ids_arr) == len(mask_arr)

    n_val = max(1, int(len(ids_arr) * args.val_frac))
    train_ids, val_ids = ids_arr[:-n_val], ids_arr[-n_val:]
    train_mask, val_mask = mask_arr[:-n_val], mask_arr[-n_val:]

    train_ids.tofile(os.path.join(DATA, "sft_train.bin"))
    val_ids.tofile(os.path.join(DATA, "sft_val.bin"))
    train_mask.tofile(os.path.join(DATA, "sft_train_mask.bin"))
    val_mask.tofile(os.path.join(DATA, "sft_val_mask.bin"))

    loss_frac = mask_arr.mean() if len(mask_arr) else 0.0
    print(f"\n{n_convos:,} conversations -> {len(ids_arr):,} tokens "
          f"(train {len(train_ids):,} / val {len(val_ids):,}), "
          f"{loss_frac:.1%} of tokens carry loss (assistant turns)")
    print(f"wrote {DATA}/{{sft_train.bin,sft_val.bin,sft_train_mask.bin,sft_val_mask.bin}}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
