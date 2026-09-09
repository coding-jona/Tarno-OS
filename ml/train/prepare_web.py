# SPDX-License-Identifier: GPL-2.0-or-later
"""Stage-2: turn data_stage2/raw/<source>/*.txt into train.bin / val.bin.

    python ml/train/prepare_web.py            # all phases
    python ml/train/prepare_web.py --phase bpe
    python ml/train/prepare_web.py --phase tok --workers 6
    python ml/train/prepare_web.py --phase pack

Phases:
  bpe   train a byte-level BPE (vocab 16384) on a bounded sample drawn across
        all sources -> data_stage2/tokenizer.json
  tok   multiprocess-encode every raw shard -> data_stage2/tok/<name>_NNNN.u16
        (resumable: an existing .u16 is skipped)
  pack  concat every .u16 in a fixed order -> data_stage2/train.bin, hold the
        tail --val-frac out as data_stage2/val.bin

RAM is tight on this box, so nothing holds the whole corpus: BPE sees only the
sample, `tok` streams one ~64 MB shard per worker, `pack` copies in 32 MB
blocks. Docs are separated by a blank line already; the BPE learns "\\n\\n" as
the boundary — no reserved token.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import sys

import numpy as np

from bpe import Tokenizer, train_bpe

HERE = os.path.dirname(os.path.abspath(__file__))
DS = os.path.join(HERE, "data_stage2")
RAW = os.path.join(DS, "raw")
TOKDIR = os.path.join(DS, "tok")
TOKENIZER = os.path.join(DS, "tokenizer.json")

SAMPLE_PER_SOURCE = 40 * 1024 * 1024   # bytes of each source fed to the BPE learner
VOCAB = 16384


def raw_shards() -> list[str]:
    return sorted(glob.glob(os.path.join(RAW, "*", "*.txt")))


def sources() -> list[str]:
    return sorted(d for d in os.listdir(RAW) if os.path.isdir(os.path.join(RAW, d)))


# --- phase: bpe ------------------------------------------------------------
def phase_bpe() -> None:
    parts: list[bytes] = []
    for src in sources():
        got = 0
        for shard in sorted(glob.glob(os.path.join(RAW, src, "*.txt"))):
            with open(shard, "rb") as fh:
                b = fh.read(SAMPLE_PER_SOURCE - got)
            parts.append(b)
            got += len(b)
            if got >= SAMPLE_PER_SOURCE:
                break
        print(f"  sample: {src} -> {got/1e6:.0f} MB", flush=True)
    sample = b"\n\n".join(parts)
    del parts
    print(f"training BPE vocab {VOCAB} on {len(sample)/1e6:.0f} MB sample ...", flush=True)
    merges = train_bpe(sample, VOCAB)
    with open(TOKENIZER, "w") as fh:
        json.dump({"kind": 1, "vocab_size": VOCAB, "merges": merges}, fh)
    print(f"wrote {TOKENIZER}  ({len(merges)} merges -> vocab {256 + len(merges)})")


# --- phase: tok ----------------------------------------------------------
_TOK = None  # per-worker Tokenizer


def _winit(merges):
    global _TOK
    _TOK = Tokenizer([tuple(m) for m in merges])


def _wencode(shard: str) -> tuple[str, int]:
    name = os.path.basename(os.path.dirname(shard)) + "_" + os.path.basename(shard)[:-4]
    out = os.path.join(TOKDIR, name + ".u16")
    if os.path.exists(out):
        return name, os.path.getsize(out) // 2
    with open(shard, "rb") as fh:
        data = fh.read()
    ids = np.asarray(_TOK.encode(data), dtype=np.uint16)
    tmp = out + ".tmp"
    ids.tofile(tmp)
    os.replace(tmp, out)
    return name, len(ids)


def phase_tok(workers: int) -> None:
    import multiprocessing as mp

    os.makedirs(TOKDIR, exist_ok=True)
    merges = json.load(open(TOKENIZER))["merges"]
    shards = raw_shards()
    todo = [s for s in shards
            if not os.path.exists(os.path.join(
                TOKDIR,
                os.path.basename(os.path.dirname(s)) + "_" + os.path.basename(s)[:-4] + ".u16"))]
    print(f"tok: {len(shards)} shards, {len(todo)} to do, {workers} workers", flush=True)
    if not todo:
        return
    done = 0
    with mp.Pool(workers, initializer=_winit, initargs=(merges,)) as pool:
        for name, n in pool.imap_unordered(_wencode, todo):
            done += 1
            print(f"\r  [{done}/{len(todo)}] {name}: {n/1e6:.1f}M tok", end="", flush=True)
    print()


# --- phase: pack ---------------------------------------------------------
def phase_pack(val_frac: float) -> None:
    # Fixed order: source-name, then shard index — reproducible.
    us = sorted(glob.glob(os.path.join(TOKDIR, "*.u16")))
    if not us:
        sys.exit("no .u16 files — run --phase tok first")
    total = sum(os.path.getsize(u) for u in us) // 2
    n_val = max(1, int(total * val_frac))
    n_train = total - n_val
    print(f"pack: {len(us)} files, {total/1e6:.1f}M tokens -> train {n_train/1e6:.1f}M / val {n_val/1e6:.1f}M",
          flush=True)

    train_path = os.path.join(DS, "train.bin")
    val_path = os.path.join(DS, "val.bin")
    written = 0
    BLK = 32 * 1024 * 1024  # tokens
    with open(train_path, "wb") as tf, open(val_path, "wb") as vf:
        for u in us:
            arr = np.fromfile(u, dtype=np.uint16)
            i = 0
            while i < len(arr):
                chunk = arr[i:i + BLK]
                # route this chunk between train / val by absolute position
                start = written
                end = written + len(chunk)
                if end <= n_train:
                    chunk.tofile(tf)
                elif start >= n_train:
                    chunk.tofile(vf)
                else:
                    cut = n_train - start
                    chunk[:cut].tofile(tf)
                    chunk[cut:].tofile(vf)
                written = end
                i += BLK
            print(f"\r  packed {written/1e6:.1f}M / {total/1e6:.1f}M", end="", flush=True)
    print(f"\nwrote {train_path} ({n_train}) + {val_path} ({n_val})")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", choices=["bpe", "tok", "pack", "all"], default="all")
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--val-frac", type=float, default=0.005)
    args = ap.parse_args()

    if not os.path.isdir(RAW) or not raw_shards():
        sys.exit(f"no raw shards under {RAW} — run fetch_web.py first")

    if args.phase in ("bpe", "all"):
        phase_bpe()
    if args.phase in ("tok", "all"):
        phase_tok(args.workers)
    if args.phase in ("pack", "all"):
        phase_pack(args.val_frac)
    return 0


if __name__ == "__main__":
    sys.exit(main())
