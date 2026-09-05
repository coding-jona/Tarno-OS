# SPDX-License-Identifier: GPL-2.0-or-later
"""Byte-pack data/dialogue.txt for a short fine-tune on top of an existing
byte-tokenizer checkpoint (spike-1m). Raw-byte only — a fine-tune must reuse
the base checkpoint's exact tokenizer, and spike-1m is vocab_size=256 raw
bytes, so there is no BPE step here (unlike prepare.py for the base runs).

    python ml/train/dialogue_extract.py     # writes data/dialogue.txt
    python ml/train/prepare_dialogue.py     # writes data/dialogue_{train,val}.bin
"""

from __future__ import annotations

import os

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data")


def main() -> int:
    src = os.path.join(DATA, "dialogue.txt")
    if not os.path.exists(src):
        print("no data/dialogue.txt — run dialogue_extract.py first")
        return 1
    raw = open(src, "rb").read()
    ids = np.frombuffer(raw, dtype=np.uint8).astype(np.uint16)
    n_val = max(1, int(len(ids) * 0.02))
    train, val = ids[:-n_val], ids[-n_val:]
    train.tofile(os.path.join(DATA, "dialogue_train.bin"))
    val.tofile(os.path.join(DATA, "dialogue_val.bin"))
    print(f"dialogue corpus {len(ids):,} bytes -> train {len(train):,} / val {len(val):,}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
