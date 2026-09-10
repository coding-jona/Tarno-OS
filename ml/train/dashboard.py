# SPDX-License-Identifier: GPL-2.0-or-later
"""Live split-screen dashboard: training status + control on the left, a chat
pane talking to the very model that's training on the right — one continuous
vertical bar down the middle keeps them visually separate.

    ml/train/run.sh dashboard
    python ml/train/dashboard.py --config config/small-30m.toml --tlm ../../small-30m.tlm

Left pane reads out/<config>/log.csv (written by train.py) and
out/staged/unlock_at (if staged.sh is driving the run); no extra
dependencies beyond the stdlib `curses` module.

Right pane sends each message through the built `thos-lm` `generate` example
(the same engine `run.sh shell` uses) against `--tlm`, so it always reflects
whatever's on disk — pair it with 'run.sh watch-export' to chat with a model
that's still training. It also understands the same control commands as
`thos-shell` (/game, /full, /train, /temp, /topk, /tokens, /reset, /params, /help) —
type /help in the chat box. Missing vs. the dedicated shell: /lang
translation and live token-by-token streaming (the whole reply prints at
once here).

Keys:  F2 pause   F3 resume   F4 request a graceful stop+checkpoint
       F1 quit the dashboard (training keeps running)
       type + Enter to chat; Backspace to edit; Esc clears the input line
Pause/resume/stop write out/<config>/control.json, which train.py polls once
per step — same file 'run.sh ctl' writes, so both are interchangeable.
"""

from __future__ import annotations

import argparse
import curses
import json
import os
import subprocess
import textwrap
import time

try:
    import tomllib
except ModuleNotFoundError:  # py < 3.11
    import tomli as tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
RUST_TARGET = "x86_64-unknown-linux-gnu"
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
    width = max(4, width)
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


def resolve_tlm(tlm: str) -> str | None:
    """Find the weights file regardless of whether `tlm` was given relative
    to the repo root, ml/train/, or as an absolute path — returns None (not
    an exception) if it genuinely doesn't exist anywhere sensible yet.

    When the same name exists in more than one place (the repo keeps a copy
    of each .tlm in both the root and ml/train/), return the *newest* — that
    is the one 'run.sh watch-export' just refreshed. Returning the first hit
    by search order instead would silently pin the chat to a stale copy.
    Always absolute: generate_reply's subprocess runs with cwd=ROOT."""
    hits = []
    for base in (None, ROOT, HERE):
        p = tlm if base is None else os.path.join(base, tlm)
        if os.path.isfile(p):
            hits.append(os.path.abspath(p))
    if not hits:
        return None
    return max(set(hits), key=os.path.getmtime)


GENERATE_BIN = os.path.join(ROOT, "target", RUST_TARGET, "release", "examples", "generate")


def generate_reply(tlm: str, prompt: str, max_tokens: int = 100, temp: float = 0.9,
                    top_k: int = 40) -> str:
    """One-shot call into the same Rust engine 'run.sh shell' uses. Blocking —
    thos-lm's matmuls are plain single-threaded Rust (no SIMD/BLAS — a
    deliberate no_std-friendly tradeoff), so this runs at roughly
    0.3-0.4s/token; 100 tokens is already ~30-40s. Runs the already-built
    binary directly rather than through 'cargo run', which re-checks the
    whole workspace on every call for no benefit once it's built once.
    Returns just the new continuation (the binary prints prompt+continuation
    decoded together)."""
    found = resolve_tlm(tlm)
    if found is None:
        return (
            f"(no weights at '{tlm}' yet — the model hasn't been exported. "
            f"Train further, then 'run.sh export' or 'run.sh watch-export'.)"
        )
    binary = GENERATE_BIN
    if not os.path.isfile(binary):
        subprocess.run(
            ["cargo", "build", "-q", "--release", "-p", "thos-lm", "--example", "generate",
             "--target", RUST_TARGET],
            cwd=ROOT, check=False,
        )
    try:
        out = subprocess.run(
            [binary, "--weights", found, "--prompt", prompt,
             "--max-tokens", str(max_tokens), "--temp", str(temp), "--top-k", str(top_k),
             "--seed", str(int(time.time())),
             # Harmless for a base model (this text never naturally occurs);
             # cuts a chat/SFT model's reply short at its own turn boundary
             # instead of always running to max_tokens.
             "--stop", "<|im_end|>", "--min-tokens", "1"],
            capture_output=True, text=True, timeout=180,
        )
    except (subprocess.TimeoutExpired, OSError) as e:
        return f"(generation failed: {e})"
    if out.returncode != 0:
        msg = out.stderr.strip()
        # Trim the raw Rust panic down to just the human-relevant message.
        if "panicked at" in msg:
            msg = msg.split("panicked at", 1)[1].split(":", 2)[-1].strip()
        return f"(generation failed: {msg[-300:] or 'unknown error'})"
    full = out.stdout.rstrip("\n")
    reply = full[len(prompt):] if full.startswith(prompt) else full
    return reply.split("<|im_end|>", 1)[0].rstrip()


def run_sh(config_path: str, *args: str, timeout: float = 90.0) -> str:
    """Shell out to run.sh the same way thos-shell's /game and /run do, with
    CONFIG pointed at whichever config this dashboard is watching. Returns
    combined, trimmed output for display in the chat pane."""
    try:
        out = subprocess.run(
            ["bash", os.path.join(HERE, "run.sh"), *args],
            cwd=HERE, env={**os.environ, "CONFIG": config_path},
            capture_output=True, text=True, timeout=timeout,
        )
    except (subprocess.TimeoutExpired, OSError) as e:
        return f"(run.sh {' '.join(args)} failed: {e})"
    text = (out.stdout + out.stderr).strip()
    return text or f"(run.sh {' '.join(args)}: no output)"


HELP_ROWS = [
    ("<text>", "generate a continuation; the exchange is kept as context"),
    ("/temp <f>", "sampling temperature (0 = greedy/argmax), default 0.9"),
    ("/topk <n>", "top-k cutoff, default 40"),
    ("/tokens <n>", "max new tokens per reply, default 100 (~30-40s — generation is single-threaded)"),
    ("/params", "show current temp/top-k/tokens"),
    ("/reset", "forget the chat context"),
    ("/game on|off|toggle|status", "cap training's CPU share for a game — training keeps running"),
    ("/full", "opposite of /game on: uncap, all cores, boosted priority (restarts the bg run)"),
    ("/train start|stop|pause|resume|status", "control the run this dashboard is watching"),
    ("/run <cmd> [args]", "any other run.sh subcommand, passed straight through"),
    ("/model <stem|path>", "switch which weights the chat talks to, e.g. /model spike-1m"),
    ("/models", "list all .tlm files found (repo root + ml/train/), with size/age"),
    ("/help", "this list"),
]


def find_models() -> list[str]:
    """All .tlm files under the repo root or ml/train/, deduped by *basename*
    (the same weights are often copied to both dirs — keep only the newest
    copy of each name), newest first."""
    best: dict[str, str] = {}
    for base in (ROOT, HERE):
        try:
            for name in os.listdir(base):
                if not name.endswith(".tlm"):
                    continue
                p = os.path.join(base, name)
                cur = best.get(name)
                if cur is None or os.path.getmtime(p) > os.path.getmtime(cur):
                    best[name] = os.path.abspath(p)
        except OSError:
            pass
    return sorted(best.values(), key=lambda p: -os.path.getmtime(p))


class ChatPane:
    def __init__(self, tlm: str, config_path: str):
        self.tlm = tlm
        self.config_path = config_path
        self.ctx = ""
        self.lines: list[tuple[str, int]] = []  # (text, curses attr) already wrapped
        self.input = ""
        self.busy = False
        self.temperature = 0.9
        self.top_k = 40
        self.max_tokens = 100  # ~30-40s at thos-lm's current single-threaded speed

    def push(self, text: str, width: int, attr: int = 0) -> None:
        for chunk in textwrap.wrap(text, width=max(4, width)) or [""]:
            self.lines.append((chunk, attr))

    def _dispatch_command(self, msg: str, width: int) -> None:
        parts = msg[1:].split()
        name = parts[0].lower() if parts else ""
        arg = parts[1] if len(parts) > 1 else ""
        rest = parts[2:]
        if name in ("help", "?"):
            for k, v in HELP_ROWS:
                self.push(f"  {k:<32} {v}", width, curses.A_DIM)
        elif name == "params":
            self.push(
                f"  temp {self.temperature}  top-k {self.top_k}  tokens {self.max_tokens}  "
                f"ctx {len(self.ctx)} chars", width, curses.A_DIM,
            )
        elif name in ("temp", "temperature"):
            try:
                v = float(arg)
                assert 0.0 <= v <= 5.0
                self.temperature = v
                self.push(f"  temp = {v}", width)
            except (ValueError, AssertionError):
                self.push("  usage: /temp <0..5>", width, curses.A_DIM)
        elif name in ("topk", "top-k"):
            try:
                v = int(arg)
                assert v >= 1
                self.top_k = v
                self.push(f"  top-k = {v}", width)
            except (ValueError, AssertionError):
                self.push("  usage: /topk <n>", width, curses.A_DIM)
        elif name in ("tokens", "max"):
            try:
                v = int(arg)
                assert 1 <= v <= 8192
                self.max_tokens = v
                self.push(f"  tokens = {v}", width)
            except (ValueError, AssertionError):
                self.push("  usage: /tokens <1..8192>", width, curses.A_DIM)
        elif name in ("reset", "clear"):
            self.ctx = ""
            self.push("  context cleared", width)
        elif name == "game":
            sub = arg or "status"
            self.push(run_sh(self.config_path, "game", sub), width, curses.A_DIM)
        elif name == "full":
            self.push(run_sh(self.config_path, "full", timeout=90), width, curses.A_DIM)
        elif name == "train":
            sub = arg or "status"
            if sub == "start":
                self.push(run_sh(self.config_path, "train-bg"), width, curses.A_DIM)
            elif sub == "status":
                self.push(run_sh(self.config_path, "status"), width, curses.A_DIM)
            elif sub in ("stop", "pause", "resume"):
                self.push(run_sh(self.config_path, "ctl", sub), width, curses.A_DIM)
            else:
                self.push(f"  unknown /train {sub} — start|stop|pause|resume|status", width, curses.A_DIM)
        elif name == "models":
            current = resolve_tlm(self.tlm)
            models = find_models()
            if not models:
                self.push("  no .tlm files found under the repo root or ml/train/", width, curses.A_DIM)
            for p in models:
                mark = "* " if p == current else "  "
                size_mb = os.path.getsize(p) / 1e6
                age = fmt_dur(time.time() - os.path.getmtime(p))
                self.push(f"{mark}{os.path.basename(p):<22} {size_mb:>7.1f} MB  {age} old",
                          width, curses.A_BOLD if p == current else curses.A_DIM)
        elif name == "model":
            # Take everything after "/model" and drop stray spaces, so a
            # fat-fingered "/model spike -1m" still resolves to "spike-1m"
            # (.tlm stems never contain spaces).
            stem = "".join(parts[1:])
            if not stem:
                self.push(f"  current model: {os.path.basename(self.tlm)}  "
                          f"(usage: /model <stem|path>; /models to list)", width, curses.A_DIM)
            else:
                candidate = stem if stem.endswith(".tlm") else f"{stem}.tlm"
                found = resolve_tlm(candidate)
                if found is None:
                    self.push(f"  no weights named '{candidate}'. Available:", width, curses.A_DIM)
                    models = find_models()
                    if not models:
                        self.push("    (none — train + 'run.sh export' first)", width, curses.A_DIM)
                    for p in models:
                        self.push(f"    {os.path.basename(p)}", width, curses.A_DIM)
                else:
                    self.tlm = found
                    self.ctx = ""
                    self.push(f"  switched to {os.path.basename(found)} — context cleared", width)
        elif name == "run":
            if not arg:
                self.push("  usage: /run <run.sh subcommand> [args...]", width, curses.A_DIM)
            else:
                self.push(run_sh(self.config_path, arg, *rest), width, curses.A_DIM)
        else:
            self.push(f"  unknown /{name} — /help for the list", width, curses.A_DIM)

    def submit(self, width: int, generating_attr: int) -> None:
        msg = self.input.strip()
        self.input = ""
        if not msg:
            return
        self.push(f"you> {msg}", width, curses.A_BOLD)
        if msg.startswith("/"):
            self._dispatch_command(msg, width)
            return
        self.ctx = (self.ctx + "\n" + msg + "\n") if self.ctx else msg + "\n"
        if len(self.ctx) > 3000:
            self.ctx = self.ctx[-3000:]
        self.busy = True


def run(stdscr, args) -> None:
    curses.curs_set(1)
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
    progress_path = os.path.join(out_dir, "progress.json")
    report_path = os.path.join(out_dir, "self_report.log")
    unlock_path = os.path.join(HERE, "out", "staged", "unlock_at")

    cfg = load_cfg(args.config)
    mc, tc = cfg["model"], cfg["train"]
    tokens_per_step = tc["batch_size"] * tc["grad_accum"] * mc["block_size"]
    msg = ""
    chat = ChatPane(args.tlm, args.config)
    last_poll = 0.0
    rows: list[dict] = []
    ctl: dict = {}

    while True:
        now = time.time()
        if now - last_poll > 1.0:
            rows = read_log(log_path)
            ctl = read_control(ctl_path)
            last_poll = now

        stdscr.erase()
        h, w = stdscr.getmaxyx()
        left_w = max(28, min(w - 24, w * 5 // 9))
        divider = left_w
        right_x = left_w + 2
        right_w = max(10, w - right_x)

        for row in range(h - 1):
            try:
                stdscr.addch(row, divider, curses.ACS_VLINE)
            except curses.error:
                pass

        ly = [0]

        def putL(text: str, attr=0):
            if ly[0] < h - 1:
                stdscr.addnstr(ly[0], 1, text, max(0, left_w - 2), attr)
            ly[0] += 1

        putL(f" THOS  —  {stem}", curses.A_BOLD | MAGENTA)
        putL(f" {mc['n_layer']}L {mc['n_head']}H d{mc['n_embd']} ctx{mc['block_size']} "
             f"vocab {mc['vocab_size']}")

        if os.path.exists(unlock_path):
            try:
                unlock = int(open(unlock_path).read().strip())
                remain = unlock - now
                if remain > 0:
                    putL(f" GENTLE — full throttle in {fmt_dur(remain)}", YELLOW)
                else:
                    putL(" FULL THROTTLE", GREEN)
            except (ValueError, OSError):
                pass

        last = rows[-1] if rows else None
        step = last["step"] if last else 0
        max_steps = tc["max_steps"]
        putL("")
        putL(f" step {step:,} / {max_steps:,}")
        putL(f" {bar(step / max_steps if max_steps else 0, max(4, left_w - 4))}", CYAN)

        prog = read_control(progress_path)  # generic JSON reader, works for this too
        if prog and now - prog.get("t", 0) < 30:  # stale (job restarted/stalled) -> hide
            of = max(1, prog.get("of", 1))
            micro = min(prog.get("micro", 0), of)
            putL(f" step {prog.get('step', step) + 1} micro {micro}/{of}", curses.A_DIM)
            putL(f" {bar(micro / of, max(4, left_w - 4))}", GREEN)

        # Chat freshness: warn if the exported .tlm has fallen behind the
        # checkpoint on disk (e.g. watch-export died) — this went unnoticed
        # for hours once already, so surface it every frame, not just in
        # 'run.sh status'.
        ckpt_path = os.path.join(out_dir, "latest.pt")
        found_tlm = resolve_tlm(chat.tlm)
        if os.path.exists(ckpt_path) and found_tlm:
            ckpt_t = os.path.getmtime(ckpt_path)
            tlm_t = os.path.getmtime(found_tlm)
            if ckpt_t - tlm_t > 120:
                stale_min = int((ckpt_t - tlm_t) / 60)
                putL(f" chat model stale by {stale_min}m — watch-export not running?", YELLOW)

        if last:
            tps = last["tps"]
            if not tps:
                # step 0's row has no throughput measurement yet (nothing to
                # divide by) — fall back to the most recent row that does
                # have one, rather than a tiny placeholder that turns into a
                # multi-year ETA.
                tps = next((r["tps"] for r in reversed(rows) if r["tps"]), 0)
            steps_per_s = tps / tokens_per_step if tokens_per_step else 0
            eta = (max_steps - step) / steps_per_s if steps_per_s > 0 else None
            best_val = min(r["val"] for r in rows)
            putL("")
            putL(f" train {last['train']:.4f}  val {last['val']:.4f} (best {best_val:.4f})")
            putL(f" lr {last['lr']:.1e}  {tps:,.0f} tok/s")
            putL(f" ETA {fmt_dur(eta) if eta is not None else '? (noch keine Messung)'}")
            putL("")
            spark_w = max(4, left_w - 8)
            putL(f" val  {sparkline([r['val'] for r in rows], spark_w)}", GREEN)
            putL(f" trn  {sparkline([r['train'] for r in rows], spark_w)}", CYAN)
        else:
            putL("")
            putL(" (no eval rows yet)", YELLOW)

        report = tail_line(report_path)
        if report:
            putL("")
            putL(" self-report (flavour text, not", curses.A_DIM)
            putL(" real introspection):", curses.A_DIM)
            for chunk in wrap(report, left_w - 4):
                putL(f" {chunk}", MAGENTA)

        putL("")
        state = "PAUSED" if ctl.get("pause") else ("STOP REQ." if ctl.get("stop") else "running")
        putL(f" {state}", YELLOW if state != "running" else GREEN)
        putL(" F2 pause F3 resume", curses.A_DIM)
        putL(" F4 stop  F1 quit", curses.A_DIM)
        if msg:
            for chunk in wrap(msg, left_w - 2):
                putL(f" {chunk}", MAGENTA)

        # --- right pane: chat ---
        chat_h = h - 3  # leave room for a heading + input line + blank
        stdscr.addnstr(0, right_x, f"chat — {os.path.basename(chat.tlm)}"
                        + (f"  (thinking… ~{chat.max_tokens * 0.35:.0f}s, single-threaded)" if chat.busy else ""),
                        right_w, curses.A_BOLD | (YELLOW if chat.busy else CYAN))
        visible = chat.lines[-chat_h:] if chat_h > 0 else []
        for i, (text, attr) in enumerate(visible):
            stdscr.addnstr(1 + i, right_x, text, right_w, attr)
        input_row = h - 2
        prompt = "you> "
        stdscr.addnstr(input_row, right_x, (prompt + chat.input)[-right_w:], right_w, curses.A_REVERSE)

        stdscr.move(input_row, min(right_x + len(prompt) + len(chat.input), right_x + right_w - 1))
        stdscr.refresh()

        if chat.busy:
            reply = generate_reply(chat.tlm, chat.ctx, max_tokens=chat.max_tokens,
                                    temp=chat.temperature, top_k=chat.top_k)
            chat.push(f"model> {reply.strip()}", right_w, GREEN)
            chat.ctx += reply
            if len(chat.ctx) > 3000:
                chat.ctx = chat.ctx[-3000:]
            chat.busy = False
            continue

        try:
            ch = stdscr.getch()
        except curses.error:
            ch = -1

        if ch == curses.KEY_F1:
            return
        elif ch == curses.KEY_F2:
            write_control(ctl_path, {"pause": True})
            msg = "pause requested"
        elif ch == curses.KEY_F3:
            write_control(ctl_path, {})
            msg = "resume requested"
        elif ch == curses.KEY_F4:
            write_control(ctl_path, {"stop": True})
            msg = "stop requested — checkpointing at end of step"
        elif ch in (curses.KEY_ENTER, 10, 13):
            chat.submit(right_w, YELLOW)
        elif ch in (curses.KEY_BACKSPACE, 127, 8):
            chat.input = chat.input[:-1]
        elif ch == 27:  # Esc
            chat.input = ""
        elif 32 <= ch < 256:
            chat.input += chr(ch)

        time.sleep(args.interval if ch == -1 else 0.0)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default=os.path.join(HERE, "config", "small-30m.toml"))
    ap.add_argument("--tlm", default="small-30m.tlm")
    ap.add_argument("--interval", type=float, default=0.15)
    args = ap.parse_args()

    print("building the chat engine (once) ...")
    subprocess.run(
        ["cargo", "build", "-q", "--release", "-p", "thos-lm", "--example", "generate",
         "--target", RUST_TARGET],
        cwd=ROOT, check=False,
    )
    curses.wrapper(run, args)


if __name__ == "__main__":
    main()
