# SPDX-License-Identifier: GPL-2.0-or-later
"""Live curses status view + control panel for a CPU training run.

    ml/train/run.sh dashboard
    python ml/train/dashboard.py --config config/small-30m.toml --tlm ../../small-30m.tlm

Reads out/<config>/log.csv (written by train.py) and out/staged/unlock_at (if
staged.sh is driving the run) and redraws every couple of seconds — no extra
dependencies, just the stdlib `curses` module.

Keys:  [p] pause   [r] resume   [s] request a graceful stop+checkpoint
       [q] quit the dashboard (training keeps running)
Pause/resume/stop write out/<config>/control.json, which train.py polls once
per step — same file 'run.sh ctl' writes, so both are interchangeable.
"""

from __future__ import annotations

import argparse
import curses
import json
import os
import textwrap
import time

try:
    import tomllib
except ModuleNotFoundError:  # py < 3.11
    import tomli as tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
SPARK = " ▁▂▃▄▅▆▇█"


def load_cfg(path: str) -> dict:
    with open(path, "rb") as fh:
        return tomllib.load(fh)


def read_log(path: str) -> list[dict]:
    rows = []
    try:
        with open(path) as fh:
            next(fh, None)  # header
            for line in fh:
                p = line.strip().split(",")
                if len(p) != 5:
                    continue
                rows.append({
                    "step": int(p[0]), "train": float(p[1]), "val": float(p[2]),
                    "lr": float(p[3]), "tps": float(p[4]),
                })
    except FileNotFoundError:
        pass
    return rows


def read_control(path: str) -> dict:
    try:
        with open(path) as fh:
            return json.load(fh)
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def write_control(path: str, data: dict) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        json.dump(data, fh)


def sparkline(values: list[float], width: int) -> str:
    if not values:
        return ""
    vs = values[-width:]
    lo, hi = min(vs), max(vs)
    span = (hi - lo) or 1.0
    return "".join(SPARK[min(8, int((v - lo) / span * 8))] for v in vs)


def bar(frac: float, width: int) -> str:
    frac = max(0.0, min(1.0, frac))
    filled = int(round(frac * width))
    return "█" * filled + "░" * (width - filled)


def tail_line(path: str) -> str:
    try:
        with open(path) as fh:
            lines = fh.readlines()
        return lines[-1].strip() if lines else ""
    except FileNotFoundError:
        return ""


def wrap(text: str, width: int) -> list[str]:
    return textwrap.wrap(text, width=width, max_lines=3, placeholder=" …") or [text[:width]]


def fmt_dur(seconds: float) -> str:
    if seconds != seconds or seconds in (float("inf"), float("-inf")):  # NaN/inf
        return "?"
    seconds = max(0, int(seconds))
    d, seconds = divmod(seconds, 86400)
    h, seconds = divmod(seconds, 3600)
    m, s = divmod(seconds, 60)
    if d:
        return f"{d}d {h}h"
    if h:
        return f"{h}h {m}m"
    if m:
        return f"{m}m {s}s"
    return f"{s}s"


def run(stdscr, args) -> None:
    curses.curs_set(0)
    stdscr.nodelay(True)
    curses.start_color()
    curses.use_default_colors()
    curses.init_pair(1, curses.COLOR_GREEN, -1)
    curses.init_pair(2, curses.COLOR_YELLOW, -1)
    curses.init_pair(3, curses.COLOR_CYAN, -1)
    curses.init_pair(4, curses.COLOR_MAGENTA, -1)
    GREEN, YELLOW, CYAN, MAGENTA = (curses.color_pair(i) for i in (1, 2, 3, 4))

    stem = os.path.splitext(os.path.basename(args.config))[0]
    out_dir = os.path.join(HERE, "out", stem)
    log_path = os.path.join(out_dir, "log.csv")
    ctl_path = os.path.join(out_dir, "control.json")
    report_path = os.path.join(out_dir, "self_report.log")
    unlock_path = os.path.join(HERE, "out", "staged", "unlock_at")

    cfg = load_cfg(args.config)
    mc, tc = cfg["model"], cfg["train"]
    tokens_per_step = tc["batch_size"] * tc["grad_accum"] * mc["block_size"]
    msg = ""

    while True:
        rows = read_log(log_path)
        ctl = read_control(ctl_path)
        stdscr.erase()
        h, w = stdscr.getmaxyx()
        y = 0

        def put(text: str, attr=0):
            nonlocal y
            if y < h - 1:
                stdscr.addnstr(y, 1, text, max(0, w - 2), attr)
            y += 1

        put(f" THOS training dashboard  —  {stem}", curses.A_BOLD | MAGENTA)
        put(f" model  {mc['n_layer']}L {mc['n_head']}H d{mc['n_embd']} ctx{mc['block_size']} "
            f"vocab {mc['vocab_size']}   weights -> {args.tlm}")

        if os.path.exists(unlock_path):
            try:
                unlock = int(open(unlock_path).read().strip())
                remain = unlock - time.time()
                if remain > 0:
                    put(f" phase  GENTLE — full throttle in {fmt_dur(remain)}", YELLOW)
                else:
                    put(" phase  FULL THROTTLE", GREEN)
            except (ValueError, OSError):
                pass

        last = rows[-1] if rows else None
        step = last["step"] if last else 0
        max_steps = tc["max_steps"]
        put("")
        put(f" step {step:,} / {max_steps:,}")
        put(f" {bar(step / max_steps if max_steps else 0, min(60, w - 4))}", CYAN)

        if last:
            tps = last["tps"] or 1.0
            steps_per_s = tps / tokens_per_step if tokens_per_step else 0
            eta = (max_steps - step) / steps_per_s if steps_per_s > 0 else float("nan")
            best_val = min(r["val"] for r in rows)
            put("")
            put(f" train loss  {last['train']:.4f}      val loss  {last['val']:.4f}  "
                f"(best {best_val:.4f})")
            put(f" lr  {last['lr']:.2e}      tok/s  {tps:,.0f}      ETA  {fmt_dur(eta)}")
            put("")
            spark_w = min(70, w - 12)
            put(f" val  {sparkline([r['val'] for r in rows], spark_w)}", GREEN)
            put(f" train{sparkline([r['train'] for r in rows], spark_w)}", CYAN)
        else:
            put("")
            put(" (no eval rows yet — waiting for the first checkpoint interval)", YELLOW)

        report = tail_line(report_path)
        if report:
            put("")
            put(" self-report (generated by the checkpoint — flavour text, not real", curses.A_DIM)
            put(" introspection: a base LM can't see its own loss curve or step count)",
                curses.A_DIM)
            for chunk in wrap(report, max(20, w - 4)):
                put(f" {chunk}", MAGENTA)

        put("")
        state = "PAUSED" if ctl.get("pause") else ("STOP REQUESTED" if ctl.get("stop") else "running")
        put(f" control  {state}   ({ctl_path})",
            YELLOW if state != "running" else GREEN)
        put(" keys: [p]ause  [r]esume  [s]top+checkpoint  [q]uit dashboard", curses.A_DIM)
        if msg:
            put(f" {msg}", MAGENTA)

        stdscr.refresh()
        try:
            ch = stdscr.getch()
        except curses.error:
            ch = -1
        if ch in (ord("q"), ord("Q")):
            return
        if ch in (ord("p"), ord("P")):
            write_control(ctl_path, {"pause": True})
            msg = "pause requested"
        elif ch in (ord("r"), ord("R")):
            write_control(ctl_path, {})
            msg = "resume requested"
        elif ch in (ord("s"), ord("S")):
            write_control(ctl_path, {"stop": True})
            msg = "stop requested — will checkpoint at the end of the current step"
        time.sleep(args.interval)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default=os.path.join(HERE, "config", "small-30m.toml"))
    ap.add_argument("--tlm", default="small-30m.tlm")
    ap.add_argument("--interval", type=float, default=2.0)
    args = ap.parse_args()
    curses.wrapper(run, args)


if __name__ == "__main__":
    main()
