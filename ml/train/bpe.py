# SPDX-License-Identifier: GPL-2.0-or-later
"""A from-scratch byte-level BPE tokenizer (train + encode + decode).

No external tokenizer library — the base vocabulary is the 256 byte values, and
merges are learned greedily by adjacent-pair frequency. Token id `256 + k` is
the result of merge `k` — the same shape GPT-2 uses.

Training works on a *word-frequency table* (the corpus split into maximal
space / non-space runs, deduplicated with counts) rather than the raw stream,
which keeps a 16k-merge run over tens of MB to minutes, not hours. Merges never
cross a chunk boundary.

The ordered merge list is what gets embedded in the `.tlm` file so `thos-lm`
(Rust) can encode prompts and decode output with no side data.
"""

from __future__ import annotations

import json
import re
from collections import Counter

_CHUNK_RE = re.compile(rb"\s+|\S+")


def _words(data: bytes) -> Counter:
    """Corpus -> {chunk-as-tuple-of-byte-ids: count}."""
    wf: Counter = Counter()
    for m in _CHUNK_RE.finditer(data):
        wf[tuple(m.group())] += 1
    return wf


def _pair_stats(wf: dict) -> Counter:
    st: Counter = Counter()
    for word, c in wf.items():
        for a, b in zip(word, word[1:]):
            st[(a, b)] += c
    return st


def _merge_word(word: tuple, pair: tuple, new_id: int) -> tuple:
    out = []
    i = 0
    n = len(word)
    while i < n:
        if i + 1 < n and word[i] == pair[0] and word[i + 1] == pair[1]:
            out.append(new_id)
            i += 2
        else:
            out.append(word[i])
            i += 1
    return tuple(out)


def train_bpe(data: bytes, vocab_size: int, verbose: bool = True) -> list[tuple[int, int]]:
    """Learn merges until the vocabulary reaches `vocab_size` (>= 256)."""
    assert vocab_size >= 256
    wf = _words(data)
    if verbose:
        print(f"  bpe: {len(wf):,} unique chunks from {len(data):,} bytes")
    merges: list[tuple[int, int]] = []
    target = vocab_size - 256
    while len(merges) < target:
        stats = _pair_stats(wf)
        if not stats:
            break
        pair, freq = stats.most_common(1)[0]
        if freq < 2:
            break
        new_id = 256 + len(merges)
        merges.append(pair)
        wf = Counter(
            {(_merge_word(w, pair, new_id) if pair[0] in w else w): c for w, c in wf.items()}
        )
        if verbose and len(merges) % 1000 == 0:
            print(f"  bpe: {len(merges)}/{target} merges (last freq {freq})")
    return merges


class Tokenizer:
    """Byte-level BPE codec built from a merge list."""

    def __init__(self, merges: list[tuple[int, int]]):
        self.merges = [tuple(m) for m in merges]
        self.rank = {m: i for i, m in enumerate(self.merges)}
        self.expand: list[bytes] = [bytes([b]) for b in range(256)]
        for a, b in self.merges:
            self.expand.append(self.expand[a] + self.expand[b])
        self.vocab_size = 256 + len(self.merges)

    @property
    def kind(self) -> int:
        return 1 if self.merges else 0

    def save(self, path: str) -> None:
        with open(path, "w") as fh:
            json.dump(
                {"kind": self.kind, "vocab_size": self.vocab_size,
                 "merges": [list(m) for m in self.merges]},
                fh,
            )

    @classmethod
    def load(cls, path: str) -> "Tokenizer":
        with open(path) as fh:
            d = json.load(fh)
        return cls(d.get("merges", []))

    def encode(self, data: bytes) -> list[int]:
        out: list[int] = []
        for m in _CHUNK_RE.finditer(data):
            out.extend(self._encode_chunk(list(m.group())))
        return out

    def _encode_chunk(self, ids: list[int]) -> list[int]:
        if not self.merges:
            return ids
        while len(ids) >= 2:
            best_rank = None
            best_i = None
            for i, pair in enumerate(zip(ids, ids[1:])):
                r = self.rank.get(pair)
                if r is not None and (best_rank is None or r < best_rank):
                    best_rank, best_i = r, i
            if best_i is None:
                break
            ids = ids[:best_i] + [256 + best_rank] + ids[best_i + 2:]
        return ids

    def decode(self, ids: list[int]) -> bytes:
        out = bytearray()
        for t in ids:
            if 0 <= t < len(self.expand):
                out += self.expand[t]
        return bytes(out)
