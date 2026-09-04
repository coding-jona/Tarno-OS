# SPDX-License-Identifier: GPL-2.0-or-later
"""Turn the raw corpus into training tensors.

    python ml/train/prepare.py [--val-frac 0.01]

Reads data/raw/*.txt, strips Project Gutenberg boilerplate, concatenates into
one UTF-8 stream (data/corpus.txt), then writes:

    data/tokenizer.json   v0: raw-byte tokenizer, vocab 256
    data/train.bin        uint16 token ids
    data/val.bin          uint16 token ids (tail slice)

Byte tokenizer now; a BPE tokenizer drops in at P1 behind the same file.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "data", "raw")
DATA = os.path.join(HERE, "data")

_START = re.compile(r"\*\*\*\s*START OF (THE|THIS) PROJECT GUTENBERG.*?\*\*\*", re.I | re.S)
_END = re.compile(r"\*\*\*\s*END OF (THE|THIS) PROJECT GUTENBERG.*?\*\*\*", re.I | re.S)


def strip_gutenberg(text: str) -> str:
    m = _START.search(text)
    if m:
        text = text[m.end():]
    m = _END.search(text)
    if m:
        text = text[: m.start()]
    return text.strip()


def normalise(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = "".join(ch for ch in text if ch == "\n" or ch == "\t" or 0x20 <= ord(ch))
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--val-frac", type=float, default=0.01)
    args = ap.parse_args()

    files = sorted(glob.glob(os.path.join(RAW, "*.txt")))
    if not files:
        print("no data/raw/*.txt — run fetch.py first")
        return 1

    parts = []
    for path in files:
        with open(path, encoding="utf-8", errors="replace") as fh:
            raw = fh.read()
        cleaned = normalise(strip_gutenberg(raw))
        parts.append(cleaned)
        print(f"  {os.path.basename(path):45} {len(cleaned):>9} chars")
    corpus = "\n\n".join(parts) + "\n"

    with open(os.path.join(DATA, "corpus.txt"), "w", encoding="utf-8") as fh:
        fh.write(corpus)

    ids = np.frombuffer(corpus.encode("utf-8"), dtype=np.uint8).astype(np.uint16)
    n_val = max(1, int(len(ids) * args.val_frac))
    train, val = ids[:-n_val], ids[-n_val:]
    train.tofile(os.path.join(DATA, "train.bin"))
    val.tofile(os.path.join(DATA, "val.bin"))

    with open(os.path.join(DATA, "tokenizer.json"), "w") as fh:
        json.dump({"kind": "byte", "vocab_size": 256}, fh, indent=2)

    print(f"\ncorpus {len(ids):,} tokens  ->  train {len(train):,} / val {len(val):,}")
    print(f"wrote {DATA}/{{corpus.txt,tokenizer.json,train.bin,val.bin}}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
