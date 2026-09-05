# SPDX-License-Identifier: GPL-2.0-or-later
"""Turn the raw corpus into training tensors.

    python ml/train/prepare.py [--val-frac 0.01] [--bpe 16384]

Reads data/raw/*.txt, strips Project Gutenberg boilerplate, concatenates into
one UTF-8 stream (data/corpus.txt), then:

  --bpe 0        raw-byte tokenizer, vocab 256           (P0 default)
  --bpe N        train a byte-level BPE tokenizer, vocab N (P1)

Writes:
    data/tokenizer.json   {kind, vocab_size, merges}
    data/train.bin        uint16 token ids
    data/val.bin          uint16 token ids (tail slice)
"""

from __future__ import annotations

import argparse
import glob
import os
import re

import numpy as np

from bpe import Tokenizer, train_bpe

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
    ap.add_argument("--bpe", type=int, default=0, help="BPE vocab size (0 = raw byte)")
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
    corpus = ("\n\n".join(parts) + "\n").encode("utf-8")

    with open(os.path.join(DATA, "corpus.txt"), "wb") as fh:
        fh.write(corpus)

    if args.bpe and args.bpe > 256:
        print(f"training byte-level BPE, vocab {args.bpe} on {len(corpus):,} bytes ...")
        merges = train_bpe(corpus, args.bpe)
        tok = Tokenizer(merges)
        print(f"  {len(merges)} merges -> vocab {tok.vocab_size}")
        ids = np.asarray(tok.encode(corpus), dtype=np.uint16)
        ratio = len(corpus) / max(1, len(ids))
        print(f"  compression: {len(corpus):,} bytes -> {len(ids):,} tokens ({ratio:.2f} B/tok)")
    else:
        tok = Tokenizer([])
        ids = np.frombuffer(corpus, dtype=np.uint8).astype(np.uint16)

    tok.save(os.path.join(DATA, "tokenizer.json"))

    n_val = max(1, int(len(ids) * args.val_frac))
    train, val = ids[:-n_val], ids[-n_val:]
    train.tofile(os.path.join(DATA, "train.bin"))
    val.tofile(os.path.join(DATA, "val.bin"))

    print(f"\ncorpus {len(ids):,} tokens (vocab {tok.vocab_size}) "
          f"-> train {len(train):,} / val {len(val):,}")
    print(f"wrote {DATA}/{{corpus.txt,tokenizer.json,train.bin,val.bin}}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
