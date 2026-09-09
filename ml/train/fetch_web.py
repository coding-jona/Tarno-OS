# SPDX-License-Identifier: GPL-2.0-or-later
"""Stage-2 corpus: stream open web/edu datasets from Hugging Face into cleaned
UTF-8 text shards, each source capped at a token budget.

    python ml/train/fetch_web.py            # fetch everything still missing
    python ml/train/fetch_web.py --list     # show sources + budgets + status
    python ml/train/fetch_web.py --only fineweb-edu

Streaming (`streaming=True`) — parquet is pulled shard-by-shard and we stop the
moment a source hits its budget, so the machine's tiny disk/RAM are never asked
to hold a full dataset. Output: data_stage2/raw/<source>/<NNNN>.txt (~64 MB
each) + data_stage2/raw/manifest.json. Resumable: a finished source is skipped;
an interrupted one restarts from shard 0 (cheap — budgets are small).

Every source is open-licensed; see ml/DATASETS.md. Token counts are estimated
as bytes/4 during the stream (good enough to hit a budget; the real count comes
out of prepare_web.py's tokenizer).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "data_stage2", "raw")
MANIFEST = os.path.join(OUT, "manifest.json")
SHARD_BYTES = 64 * 1024 * 1024
BYTES_PER_TOK = 4  # rough; byte-BPE at vocab 16k lands near here on English

# name -> (hf path, config, split, text field, token budget, license note)
SOURCES = {
    # front-loaded: short, clean, simple English — teaches grammar/fluency fast
    "tinystories": (
        "roneneldan/TinyStories", None, "train", "text",
        350_000_000, "TinyStories — CDLA-Sharing-1.0 / synthetic, open",
    ),
    # the backbone: education-filtered CommonCrawl
    "fineweb-edu": (
        "HuggingFaceFW/fineweb-edu", "sample-10BT", "train", "text",
        600_000_000, "FineWeb-Edu sample-10BT — ODC-By-1.0",
    ),
    # facts
    "wikipedia-en": (
        "wikimedia/wikipedia", "20231101.en", "train", "text",
        150_000_000, "Wikipedia (en) — CC-BY-SA-4.0 (share-alike on derived text)",
    ),
    # synthetic textbooks — knowledge density per token
    "cosmopedia-v2": (
        "HuggingFaceTB/smollm-corpus", "cosmopedia-v2", "train", "text",
        120_000_000, "Cosmopedia v2 (SmolLM corpus) — Apache-2.0",
    ),
}

_WS = re.compile(r"[ \t]+")
_NL = re.compile(r"\n{3,}")


def clean(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = "".join(c for c in text if c == "\n" or c == "\t" or 0x20 <= ord(c) or ord(c) > 0x7F)
    text = _WS.sub(" ", text)
    text = _NL.sub("\n\n", text)
    return text.strip()


def load_manifest() -> dict:
    try:
        with open(MANIFEST) as fh:
            return json.load(fh)
    except FileNotFoundError:
        return {}


def save_manifest(m: dict) -> None:
    os.makedirs(OUT, exist_ok=True)
    tmp = MANIFEST + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(m, fh, indent=2, sort_keys=True)
    os.replace(tmp, MANIFEST)


def fetch_source(name: str, manifest: dict) -> bool:
    from datasets import load_dataset

    hf_path, config, split, field, budget, lic = SOURCES[name]
    sdir = os.path.join(OUT, name)
    if manifest.get(name, {}).get("done"):
        print(f"  {name}: done ({manifest[name]['tokens_est']/1e6:.0f}M tok est) — skip")
        return True
    os.makedirs(sdir, exist_ok=True)
    print(f"fetching {name}  <-  {hf_path}"
          + (f" [{config}]" if config else "")
          + f"  budget {budget/1e6:.0f}M tok\n  {lic}")

    ds = load_dataset(hf_path, config, split=split, streaming=True)

    bytes_total = 0
    docs = 0
    shard_idx = 0
    buf: list[str] = []
    buf_bytes = 0

    def flush():
        nonlocal shard_idx, buf, buf_bytes
        if not buf:
            return
        path = os.path.join(sdir, f"{shard_idx:04d}.txt")
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("\n\n".join(buf))
        shard_idx += 1
        buf = []
        buf_bytes = 0

    try:
        for row in ds:
            t = clean(str(row.get(field, "")))
            if len(t) < 200:  # drop stubs
                continue
            buf.append(t)
            b = len(t.encode("utf-8")) + 2
            buf_bytes += b
            bytes_total += b
            docs += 1
            if buf_bytes >= SHARD_BYTES:
                flush()
                print(f"\r  {name}: {bytes_total/1e6:8.1f} MB  "
                      f"~{bytes_total/BYTES_PER_TOK/1e6:6.0f}M tok  {docs} docs", end="")
            if bytes_total / BYTES_PER_TOK >= budget:
                break
    except KeyboardInterrupt:
        flush()
        print(f"\n  {name}: interrupted at {bytes_total/1e6:.1f} MB — rerun to resume (restarts this source)")
        return False
    flush()
    print(f"\r  {name}: {bytes_total/1e6:8.1f} MB  "
          f"~{bytes_total/BYTES_PER_TOK/1e6:6.0f}M tok  {docs} docs  DONE")
    manifest[name] = {
        "hf_path": hf_path, "config": config, "license": lic,
        "shards": shard_idx, "docs": docs, "bytes": bytes_total,
        "tokens_est": int(bytes_total / BYTES_PER_TOK), "done": True,
    }
    save_manifest(manifest)
    return True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--only", action="append", help="fetch only this source (repeatable)")
    args = ap.parse_args()

    os.makedirs(OUT, exist_ok=True)
    manifest = load_manifest()

    if args.list:
        for name, (p, c, _s, _f, budget, lic) in SOURCES.items():
            st = "done " if manifest.get(name, {}).get("done") else "--   "
            print(f"  [{st}] {name:14} {budget/1e6:5.0f}M tok  {p}{f' [{c}]' if c else ''}")
            print(f"           {lic}")
        return 0

    want = args.only or list(SOURCES)
    failed = []
    for name in want:
        if name not in SOURCES:
            print(f"unknown source: {name}")
            return 2
        if not fetch_source(name, manifest):
            failed.append(name)

    total = sum(m.get("tokens_est", 0) for m in manifest.values() if m.get("done"))
    print(f"\n{sum(1 for m in manifest.values() if m.get('done'))}/{len(SOURCES)} sources done, "
          f"~{total/1e6:.0f}M tokens estimated in {OUT}")
    if failed:
        print(f"unfinished: {', '.join(failed)} — rerun in the next internet window")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
