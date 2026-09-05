# SPDX-License-Identifier: GPL-2.0-or-later
"""Extract dialogue turns from the public-domain plays/dialogues already in
data/raw/ into a plain chat-style transcript: "Speaker: line" per turn, one
turn per line. This is what teaches the base model turn-taking *format* — a
prose novel never says "X: ... \n Y: ...", a play always does.

Three source markups, auto-detected per line (Gutenberg's Moby Shakespeare /
English-play style, and two different German italic/bold conventions):

    ROMEO.                  _Faust._                =Ruodi.=
    Line of dialogue...     Line of dialogue...      Line of dialogue...

Real character names are kept (not normalised to "A:"/"B:") — with dozens of
distinct names across many plays, the model has to generalise the abstract
"Name: text" turn-taking pattern rather than memorise one fixed pair, which
is what we actually want it to pick up.

    python ml/train/dialogue_extract.py

Writes data/dialogue.txt (git-ignored, like everything else under data/).
"""

from __future__ import annotations

import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "data", "raw")
OUT = os.path.join(HERE, "data", "dialogue.txt")

SOURCES = [
    "pg2542_a_dolls_house.txt",
    "pg844_importance_of_being_earnest.txt",
    "pg1513_romeo_and_juliet.txt",
    "pg1514_midsummer_nights_dream.txt",
    "pg1515_merchant_of_venice.txt",
    "pg1519_much_ado_about_nothing.txt",
    "pg1524_hamlet.txt",
    "pg1533_macbeth.txt",
    "de_pg21000_faust.txt",
    "de_pg77182_wilhelm_tell.txt",
]

CUE_PATTERNS = [
    re.compile(r"^([A-ZÀ-ÖØ-Ý][A-ZÀ-ÖØ-Ý' ]{1,25})\.\s*$"),                      # ENGLISH ALLCAPS.
    re.compile(r"^_([A-ZÄÖÜ][a-zA-ZÄÖÜäöüß ]{1,25})\.?_\.?\s*$"),                # _German Italic._
    re.compile(r"^\s*=([A-ZÄÖÜ][a-zA-ZÄÖÜäöüß ]{1,25})\.=\s*(?:\(.*\))?\s*$"),   # =German Bold.=
]

STAGE_DIRECTION = re.compile(r"_\[[^\]]*\]_|\[[^\]]*\]|_[^_]*_|\([^)]*\)")
WS = re.compile(r"\s+")


def cue(line: str) -> str | None:
    for pat in CUE_PATTERNS:
        m = pat.match(line)
        if m:
            return m.group(1).strip().title()
    return None


def clean(text: str) -> str:
    text = STAGE_DIRECTION.sub(" ", text)
    return WS.sub(" ", text).strip()


def extract(path: str) -> list[tuple[str, str]]:
    turns: list[tuple[str, str]] = []
    speaker: str | None = None
    lines: list[str] = []

    def flush() -> None:
        if speaker and lines:
            text = clean(" ".join(lines))
            if 2 <= len(text.split()) <= 120:  # drop empties and runaway soliloquies
                turns.append((speaker, text))

    with open(path, encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            line = raw.rstrip("\n")
            who = cue(line)
            if who:
                flush()
                speaker, lines = who, []
                continue
            if speaker is not None and line.strip():
                lines.append(line.strip())
    flush()
    return turns


def main() -> None:
    total = 0
    with open(OUT, "w", encoding="utf-8") as out:
        for name in SOURCES:
            path = os.path.join(RAW, name)
            if not os.path.exists(path):
                print(f"  skip (not fetched): {name}")
                continue
            turns = extract(path)
            print(f"  {name:45} {len(turns):>5} turns")
            total += len(turns)
            for speaker, text in turns:
                out.write(f"{speaker}: {text}\n")
            out.write("\n")
    size = os.path.getsize(OUT)
    print(f"\n{total:,} turns, {size/1e6:.2f} MB -> {OUT}")


if __name__ == "__main__":
    main()
