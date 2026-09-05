# SPDX-License-Identifier: GPL-2.0-or-later
"""Downloads a couple of open instruction/chat datasets from Hugging Face and
normalises each into the same {"messages": [{"role", "content"}, ...]} JSONL
shape as gen_thos_custom.py, under data/sft_raw/. prepare_sft.py then reads
everything in that directory together.

    python ml/train/fetch_sft.py [--limit N]

Needs internet once, at fetch time — training itself still doesn't. Only
open, redistributable text datasets; no pretrained model weights touch this
project (THOS's language model is trained from scratch — see DATASETS.md).
"""
from __future__ import annotations

import argparse
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_DIR = os.path.join(HERE, "data", "sft_raw")


def write_jsonl(path: str, records: list[dict]) -> None:
    with open(path, "w", encoding="utf-8") as fh:
        for r in records:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"  wrote {len(records):,} -> {path}")


def fetch_smoltalk(limit: int) -> None:
    from datasets import load_dataset
    print("smoltalk (HuggingFaceTB/smoltalk, 'everyday-conversations' subset) ...")
    ds = load_dataset("HuggingFaceTB/smoltalk", "everyday-conversations", split="train")
    records = []
    for row in ds:
        msgs = row.get("messages")
        if not msgs:
            continue
        # keep it simple for a 30M model: single user->assistant turn pairs,
        # short content only.
        clean = [{"role": m["role"], "content": m["content"].strip()}
                 for m in msgs if m["role"] in ("user", "assistant") and m["content"].strip()]
        if len(clean) >= 2 and all(len(m["content"]) < 500 for m in clean):
            records.append({"messages": clean})
        if len(records) >= limit:
            break
    write_jsonl(os.path.join(OUT_DIR, "smoltalk.jsonl"), records)


def fetch_alpaca(limit: int) -> None:
    from datasets import load_dataset
    print("alpaca-cleaned (yahma/alpaca-cleaned) ...")
    ds = load_dataset("yahma/alpaca-cleaned", split="train")
    records = []
    for row in ds:
        instr = (row.get("instruction") or "").strip()
        inp = (row.get("input") or "").strip()
        out = (row.get("output") or "").strip()
        if not instr or not out:
            continue
        user = f"{instr}\n{inp}" if inp else instr
        if len(user) > 500 or len(out) > 500:
            continue
        records.append({"messages": [
            {"role": "user", "content": user},
            {"role": "assistant", "content": out},
        ]})
        if len(records) >= limit:
            break
    write_jsonl(os.path.join(OUT_DIR, "alpaca.jsonl"), records)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=4000,
                     help="max examples per dataset (30M params needs quality, not GB of data)")
    ap.add_argument("--only", choices=["smoltalk", "alpaca"], default=None)
    args = ap.parse_args()

    os.makedirs(OUT_DIR, exist_ok=True)
    if args.only in (None, "smoltalk"):
        fetch_smoltalk(args.limit)
    if args.only in (None, "alpaca"):
        fetch_alpaca(args.limit)


if __name__ == "__main__":
    main()
