# SPDX-License-Identifier: GPL-2.0-or-later
"""A tiny local, offline translation coprocess for thos-shell.

Not part of "the AI" — thos-lm stays the from-scratch model. This is a text
in/out utility layer, backed by argostranslate (Apache-2.0, fully offline,
no API key, no network at translate time): it lets you talk to an
English-trained model in German without pretending the model itself
understands German any better than the corpus gives it.

Protocol: one JSON object per line on stdin, one back on stdout, both
flushed immediately (thos-shell talks to this as a long-lived subprocess so
the translation models load once, not per call):

    {"from": "de", "to": "en", "text": "Hallo Welt"}   ->   {"text": "Hello World"}
    {"ping": true}                                     ->   {"ok": true}

    python ml/train/translate.py             # run the coprocess (stdin/stdout)
    python ml/train/translate.py --install   # download + install the en<->de
                                              # packages once (needs internet)
"""

from __future__ import annotations

import argparse
import json
import sys


def install() -> int:
    import argostranslate.package as pkg

    pkg.update_package_index()
    avail = pkg.get_available_packages()
    wanted = {("en", "de"), ("de", "en")}
    got = set()
    for p in avail:
        if (p.from_code, p.to_code) in wanted:
            print(f"installing {p.from_code} -> {p.to_code}", file=sys.stderr)
            pkg.install_from_path(p.download())
            got.add((p.from_code, p.to_code))
    missing = wanted - got
    if missing:
        print(f"could not find packages for: {missing}", file=sys.stderr)
        return 1
    return 0


def serve() -> int:
    import argostranslate.translate as tr

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            print(json.dumps({"error": str(e)}), flush=True)
            continue
        if req.get("ping"):
            print(json.dumps({"ok": True}), flush=True)
            continue
        try:
            out = tr.translate(req["text"], req["from"], req["to"])
            print(json.dumps({"text": out}), flush=True)
        except Exception as e:  # keep the coprocess alive on any single bad request
            print(json.dumps({"error": str(e)}), flush=True)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--install", action="store_true", help="download the en<->de packages, then exit")
    args = ap.parse_args()
    return install() if args.install else serve()


if __name__ == "__main__":
    sys.exit(main())
