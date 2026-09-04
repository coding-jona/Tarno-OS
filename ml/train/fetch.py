# SPDX-License-Identifier: GPL-2.0-or-later
"""Resumable downloader for the open training corpus.

Internet on this machine drops at 00:00 (02:00 Fri->Sat and Sat->Sun), so every
download is resumable: partial files are staged as `<name>.part`, an HTTP Range
request continues where it left off, and a SHA-256 manifest lets a re-run skip
what already finished. Safe to Ctrl-C and restart any number of times.

    python ml/train/fetch.py            # fetch everything still missing
    python ml/train/fetch.py --list     # show the source list + status

v0 corpus: a dozen Project Gutenberg public-domain books (~5-10 MB total).
Every source and its licence is recorded in ml/DATASETS.md.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "data", "raw")
MANIFEST = os.path.join(HERE, "data", "manifest.json")
UA = "thos-lm-fetch/0 (+https://github.com/coding-jona/Tarno-OS)"

# name -> url. Public domain (US). Gutenberg trademark headers/footers are
# stripped later by prepare.py.
SOURCES: dict[str, str] = {
    "pg1342_pride_and_prejudice.txt": "https://www.gutenberg.org/cache/epub/1342/pg1342.txt",
    "pg11_alice_in_wonderland.txt": "https://www.gutenberg.org/cache/epub/11/pg11.txt",
    "pg84_frankenstein.txt": "https://www.gutenberg.org/cache/epub/84/pg84.txt",
    "pg1661_sherlock_holmes.txt": "https://www.gutenberg.org/cache/epub/1661/pg1661.txt",
    "pg2701_moby_dick.txt": "https://www.gutenberg.org/cache/epub/2701/pg2701.txt",
    "pg98_a_tale_of_two_cities.txt": "https://www.gutenberg.org/cache/epub/98/pg98.txt",
    "pg1400_great_expectations.txt": "https://www.gutenberg.org/cache/epub/1400/pg1400.txt",
    "pg74_tom_sawyer.txt": "https://www.gutenberg.org/cache/epub/74/pg74.txt",
    "pg345_dracula.txt": "https://www.gutenberg.org/cache/epub/345/pg345.txt",
    "pg2600_war_and_peace.txt": "https://www.gutenberg.org/cache/epub/2600/pg2600.txt",
    "pg1080_a_modest_proposal.txt": "https://www.gutenberg.org/cache/epub/1080/pg1080.txt",
    "pg5200_metamorphosis.txt": "https://www.gutenberg.org/cache/epub/5200/pg5200.txt",
    # --- P1 expansion (~20 more public-domain titles) ---
    "pg1260_jane_eyre.txt": "https://www.gutenberg.org/cache/epub/1260/pg1260.txt",
    "pg768_wuthering_heights.txt": "https://www.gutenberg.org/cache/epub/768/pg768.txt",
    "pg158_emma.txt": "https://www.gutenberg.org/cache/epub/158/pg158.txt",
    "pg161_sense_and_sensibility.txt": "https://www.gutenberg.org/cache/epub/161/pg161.txt",
    "pg1232_the_prince.txt": "https://www.gutenberg.org/cache/epub/1232/pg1232.txt",
    "pg2542_a_dolls_house.txt": "https://www.gutenberg.org/cache/epub/2542/pg2542.txt",
    "pg1497_the_republic.txt": "https://www.gutenberg.org/cache/epub/1497/pg1497.txt",
    "pg2554_crime_and_punishment.txt": "https://www.gutenberg.org/cache/epub/2554/pg2554.txt",
    "pg28054_brothers_karamazov.txt": "https://www.gutenberg.org/cache/epub/28054/pg28054.txt",
    "pg120_treasure_island.txt": "https://www.gutenberg.org/cache/epub/120/pg120.txt",
    "pg43_jekyll_and_hyde.txt": "https://www.gutenberg.org/cache/epub/43/pg43.txt",
    "pg64317_the_great_gatsby.txt": "https://www.gutenberg.org/cache/epub/64317/pg64317.txt",
    "pg1400b_bleak_house.txt": "https://www.gutenberg.org/cache/epub/1023/pg1023.txt",
    "pg766_david_copperfield.txt": "https://www.gutenberg.org/cache/epub/766/pg766.txt",
    "pg730_oliver_twist.txt": "https://www.gutenberg.org/cache/epub/730/pg730.txt",
    "pg1250_anna_karenina.txt": "https://www.gutenberg.org/cache/epub/1399/pg1399.txt",
    "pg174_dorian_gray.txt": "https://www.gutenberg.org/cache/epub/174/pg174.txt",
    "pg16_peter_pan.txt": "https://www.gutenberg.org/cache/epub/16/pg16.txt",
    "pg35_the_time_machine.txt": "https://www.gutenberg.org/cache/epub/35/pg35.txt",
    "pg36_war_of_the_worlds.txt": "https://www.gutenberg.org/cache/epub/36/pg36.txt",
    "pg2591_grimms_fairy_tales.txt": "https://www.gutenberg.org/cache/epub/2591/pg2591.txt",
    "pg1727_the_odyssey.txt": "https://www.gutenberg.org/cache/epub/1727/pg1727.txt",
    "pg6130_the_iliad.txt": "https://www.gutenberg.org/cache/epub/6130/pg6130.txt",
    "pg1399_middlemarch.txt": "https://www.gutenberg.org/cache/epub/145/pg145.txt",
}

CHUNK = 1 << 16


def load_manifest() -> dict:
    try:
        with open(MANIFEST) as fh:
            return json.load(fh)
    except FileNotFoundError:
        return {}


def save_manifest(m: dict) -> None:
    os.makedirs(os.path.dirname(MANIFEST), exist_ok=True)
    tmp = MANIFEST + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(m, fh, indent=2, sort_keys=True)
    os.replace(tmp, MANIFEST)


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for blk in iter(lambda: fh.read(1 << 20), b""):
            h.update(blk)
    return h.hexdigest()


def fetch_one(name: str, url: str, manifest: dict, retries: int = 6) -> bool:
    final = os.path.join(RAW, name)
    part = final + ".part"
    if name in manifest and os.path.exists(final):
        return True

    for attempt in range(1, retries + 1):
        have = os.path.getsize(part) if os.path.exists(part) else 0
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        if have:
            req.add_header("Range", f"bytes={have}-")
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                mode = "ab"
                if have and resp.status != 206:
                    have, mode = 0, "wb"  # server ignored Range; restart
                total = have + int(resp.headers.get("Content-Length", 0))
                with open(part, mode) as out:
                    while True:
                        blk = resp.read(CHUNK)
                        if not blk:
                            break
                        out.write(blk)
                        have += len(blk)
                        if total:
                            pct = 100 * have / total
                            print(f"\r  {name}: {have:>9} / {total} B ({pct:5.1f}%)", end="")
                print()
            os.replace(part, final)
            digest = sha256_file(final)
            manifest[name] = {"url": url, "sha256": digest, "bytes": os.path.getsize(final)}
            save_manifest(manifest)
            return True
        except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
            wait = min(30, 2 ** attempt)
            print(f"\n  {name}: {exc} — retry {attempt}/{retries} in {wait}s "
                  f"(have {have} B; safe to stop and rerun)")
            time.sleep(wait)
    return False


def verify(manifest: dict) -> bool:
    ok = True
    for name, meta in manifest.items():
        path = os.path.join(RAW, name)
        if not os.path.exists(path):
            print(f"  MISSING {name}")
            ok = False
        elif sha256_file(path) != meta["sha256"]:
            print(f"  CORRUPT {name} — delete data/raw/{name} and rerun")
            ok = False
    return ok


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--list", action="store_true", help="show sources + status, do nothing")
    ap.add_argument("--verify", action="store_true", help="re-hash downloaded files")
    args = ap.parse_args()

    os.makedirs(RAW, exist_ok=True)
    manifest = load_manifest()

    if args.list:
        for name, url in SOURCES.items():
            done = "ok " if name in manifest else "-- "
            print(f"  [{done}] {name}  <-  {url}")
        return 0
    if args.verify:
        return 0 if verify(manifest) else 1

    failed = []
    for name, url in SOURCES.items():
        if name in manifest and os.path.exists(os.path.join(RAW, name)):
            continue
        print(f"fetching {name}")
        if not fetch_one(name, url, manifest):
            failed.append(name)

    total = sum(m["bytes"] for m in manifest.values())
    print(f"\n{len(manifest)}/{len(SOURCES)} files, {total/1e6:.1f} MB in {RAW}")
    if failed:
        print(f"still missing: {', '.join(failed)} — rerun in the next internet window")
        return 1
    return 0 if verify(manifest) else 1


if __name__ == "__main__":
    sys.exit(main())
