# SPDX-License-Identifier: GPL-2.0-or-later
"""A from-scratch byte-level BPE tokenizer (train + encode + decode).

No external tokenizer library — the base vocabulary is the 256 byte values, and
merges are learned greedily by adjacent-pair frequency. Token id `256 + k` is
the result of merge `k` — the same shape GPT-2 uses.

Training works on a word-frequency table (the corpus split into maximal
whitespace / non-whitespace runs, deduplicated with counts), with incremental
pair-count maintenance and a lazy max-heap, so a 16k-merge run over tens of MB
takes a minute or two, not hours. Merges never cross a chunk boundary.

The ordered merge list is embedded in the `.tlm` file so `thos-lm` (Rust) can
encode prompts and decode output with no side data.
"""

from __future__ import annotations

import heapq
import json
import re
from collections import Counter, defaultdict

_CHUNK_RE = re.compile(rb"\s+|\S+")


def _apply(ids: list[int], pair: tuple, new_id: int) -> list[int]:
    out = []
    i = 0
    n = len(ids)
    while i < n:
        if i + 1 < n and ids[i] == pair[0] and ids[i + 1] == pair[1]:
            out.append(new_id)
            i += 2
        else:
            out.append(ids[i])
            i += 1
    return out


def train_bpe(data: bytes, vocab_size: int, verbose: bool = True) -> list[tuple[int, int]]:
    """Learn merges until the vocabulary reaches `vocab_size` (>= 256)."""
    assert vocab_size >= 256
    wc: Counter = Counter(bytes(m.group()) for m in _CHUNK_RE.finditer(data))
    words = [[list(w), c] for w, c in wc.items()]
    if verbose:
        print(f"  bpe: {len(words):,} unique chunks from {len(data):,} bytes", flush=True)

    stats: Counter = Counter()
    where: dict = defaultdict(set)
    for i, (ids, c) in enumerate(words):
        for p in zip(ids, ids[1:]):
            stats[p] += c
            where[p].add(i)
    heap = [(-v, p) for p, v in stats.items()]
    heapq.heapify(heap)

    merges: list[tuple[int, int]] = []
    target = vocab_size - 256
    while len(merges) < target:
        pair = None
        while heap:
            negc, cand = heap[0]
            if stats.get(cand, 0) == -negc:
                pair = cand
                break
            heapq.heappop(heap)
        if pair is None or stats[pair] < 2:
            break

        new_id = 256 + len(merges)
        merges.append(pair)
        touched: set = set()
        for i in list(where.get(pair, ())):
            ids, c = words[i]
            for p in zip(ids, ids[1:]):
                stats[p] -= c
                where[p].discard(i)
                touched.add(p)
            ids = _apply(ids, pair, new_id)
            words[i][0] = ids
            for p in zip(ids, ids[1:]):
                stats[p] += c
                where[p].add(i)
                touched.add(p)
        for p in touched:
            v = stats.get(p, 0)
            if v >= 2:
                heapq.heappush(heap, (-v, p))
        where.pop(pair, None)
        if verbose and len(merges) % 1000 == 0:
            print(f"  bpe: {len(merges)}/{target} merges", flush=True)
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
