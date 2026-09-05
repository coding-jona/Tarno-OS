#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
#
# One entry point for the whole THOS language-model P0 pipeline, so you don't
# have to remember the individual commands. Every step is idempotent /
# resumable — safe to re-run after an interruption (the nightly internet
# cut-off, a killed training run, a reboot).
#
#   ml/train/run.sh all            setup -> data -> train -> export -> sample
#   ml/train/run.sh setup          create .venv, install torch(CPU)/numpy/tqdm
#   ml/train/run.sh data           fetch the open corpus + build train/val bins
#   ml/train/run.sh train          train (auto-resumes from out/<config>/latest.pt)
#   ml/train/run.sh train-bg       same, in the background -> out/<config>/train.log
#   ml/train/run.sh status         show background training progress
#   ml/train/run.sh stop           stop background training
#   ml/train/run.sh export         out/<config>/latest.pt  ->  $TLM (spike-1m.tlm)
#   ml/train/run.sh eval           perplexity + next-token probe + a sample
#   ml/train/run.sh sample "Text"  build the Rust engine + generate from the .tlm
#   ml/train/run.sh shell          launch the interactive local AI shell (thos-shell)
#   ml/train/run.sh watch-export      re-export $TLM whenever a new checkpoint lands (foreground)
#   ml/train/run.sh watch-export-bg   same, detached with a pid file — survives closing the terminal
#   ml/train/run.sh watch-export-stop stop the backgrounded watch-export
#   ml/train/run.sh dashboard      live curses status view of a training run
#   ml/train/run.sh ctl CMD        control a running job: stop|pause|resume|"lr <x>"
#   ml/train/run.sh game {on|off|toggle|status}   cap training's CPU share for a game (keeps training running, just slower); GAME_CORES=N to tune
#   ml/train/run.sh test           no_std build + golden cross-check + fixture
#   ml/train/run.sh clean          remove .venv, data/, out/, *.tlm
#
# Overridable via env:  CONFIG=  TLM=  PROMPT=  MAXTOK=  BPE=  PYTORCH_INDEX=  INIT_FROM=
#   INIT_FROM=<base .pt>  seed weights from a base checkpoint on a run's first
#                         start (SFT) — ignored once that run has its own
#                         latest.pt; see config/small-30m-sft.toml
# P1 recipe:  BPE=16384 ml/train/run.sh data
#             CONFIG=ml/train/config/small-30m.toml TLM=small-30m.tlm ml/train/run.sh train-bg

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
VENV="$HERE/.venv"
PY="$VENV/bin/python"
CONFIG="${CONFIG:-$HERE/config/spike-1m.toml}"
# Resolve now, while $PWD is still wherever the caller invoked this from —
# several subcommands 'cd' into $HERE before reading $CONFIG, which silently
# breaks a relative CONFIG typed from the repo root (e.g.
# CONFIG=ml/train/config/x.toml run from /repo instead of /repo/ml/train).
case "$CONFIG" in
  /*) : ;;
  *)  CONFIG="$PWD/$CONFIG" ;;
esac
# checkpoints/logs are per-config so a new model never resumes another's latest.pt
OUT="$HERE/out/$(basename "${CONFIG%.toml}")"
TLM="${TLM:-$ROOT/spike-1m.tlm}"
# Same class of bug as CONFIG above: a bare relative TLM (as every doc
# example writes it, e.g. TLM=small-30m.tlm) resolved against whatever a
# subcommand had cd'd to by the time it was used — cmd_self_report's `cd
# "$ROOT"` made a relative TLM point at $ROOT/small-30m.tlm, which never
# existed, so its generate call failed every time, and because it's called
# from inside watch-export's `set -euo pipefail` loop, that silently killed
# the whole background watch-export process. Resolve once, up front.
case "$TLM" in
  /*) : ;;
  *)  TLM="$PWD/$TLM" ;;
esac
PROMPT="${PROMPT:-The }"
MAXTOK="${MAXTOK:-200}"
PYTORCH_INDEX="${PYTORCH_INDEX:-https://download.pytorch.org/whl/cpu}"
RUST_TARGET="x86_64-unknown-linux-gnu"

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die()  { printf '\033[31merror: %s\033[0m\n' "$*" >&2; exit 1; }

have_py_mod() { [ -x "$PY" ] && "$PY" -c "import $1" >/dev/null 2>&1; }

cmd_setup() {
  say "setup"
  if [ ! -x "$PY" ]; then
    command -v python3 >/dev/null || die "python3 not found"
    python3 -m venv "$VENV" || die "python3 -m venv failed (need the python3-venv package)"
    "$VENV/bin/pip" -q install --upgrade pip
  fi
  if have_py_mod torch; then
    echo "torch present: $("$PY" -c 'import torch;print(torch.__version__)')"
  else
    echo "installing torch (CPU) from $PYTORCH_INDEX ..."
    "$VENV/bin/pip" install -q --index-url "$PYTORCH_INDEX" torch
  fi
  "$VENV/bin/pip" install -q numpy tqdm
  "$PY" -c 'import torch,numpy,tqdm;print("deps ok — torch",torch.__version__,"| threads",torch.get_num_threads())'
}

need_venv() { have_py_mod numpy || cmd_setup; }

cmd_data() {
  need_venv
  say "fetch corpus (resumable)"
  "$PY" "$HERE/fetch.py"
  say "prepare (tokenize + pack)  BPE=${BPE:-0}"
  "$PY" "$HERE/prepare.py" --bpe "${BPE:-0}"
}

cmd_eval() {
  need_venv
  [ -f "$TLM" ] || die "no weights at $TLM — run 'run.sh export'"
  say "eval  ($TLM)"
  "$PY" "$HERE/eval.py" --weights "$TLM"
}

_train_ready() { [ -f "$HERE/data/train.bin" ] && [ -f "$HERE/data/val.bin" ]; }

cmd_train() {
  need_venv
  _train_ready || cmd_data
  local resume=()
  if [ -f "$OUT/latest.pt" ]; then
    resume=(--resume); echo "resuming from $OUT/latest.pt"
  elif [ -n "${INIT_FROM:-}" ]; then
    resume=(--init-from "$INIT_FROM")
  fi
  say "train  ($(basename "$CONFIG"))"
  exec "$PY" -u "$HERE/train.py" --config "$CONFIG" "${resume[@]}"
}

cmd_train_bg() {
  need_venv
  _train_ready || cmd_data
  mkdir -p "$OUT"
  if [ -f "$OUT/train.pid" ] && kill -0 "$(cat "$OUT/train.pid")" 2>/dev/null; then
    die "training already running (pid $(cat "$OUT/train.pid")) — 'run.sh status' or 'run.sh stop'"
  fi
  local resume=()
  if [ -f "$OUT/latest.pt" ]; then
    resume=(--resume)
  elif [ -n "${INIT_FROM:-}" ]; then
    resume=(--init-from "$INIT_FROM")
  fi
  nohup "$PY" -u "$HERE/train.py" --config "$CONFIG" "${resume[@]}" > "$OUT/train.log" 2>&1 &
  echo $! > "$OUT/train.pid"
  say "training in background — pid $(cat "$OUT/train.pid")"
  echo "watch:  ml/train/run.sh status    (or: tail -f $OUT/train.log)"
}

cmd_status() {
  if [ -f "$OUT/train.pid" ] && kill -0 "$(cat "$OUT/train.pid")" 2>/dev/null; then
    echo "training RUNNING (pid $(cat "$OUT/train.pid"))"
  else
    echo "no background training active"
  fi
  if [ -f "$OUT/watch-export.pid" ] && kill -0 "$(cat "$OUT/watch-export.pid")" 2>/dev/null; then
    echo "watch-export RUNNING (pid $(cat "$OUT/watch-export.pid"))"
  else
    echo "watch-export not running — 'run.sh watch-export-bg' to auto-refresh $TLM"
  fi
  if [ -f "$OUT/latest.pt" ]; then
    local ckpt_t tlm_t
    ckpt_t="$(stat -c %Y "$OUT/latest.pt")"
    tlm_t="$(stat -c %Y "$TLM" 2>/dev/null || echo 0)"
    if [ "$ckpt_t" -gt "$((tlm_t + 120))" ]; then
      echo "STALE: $TLM is behind the latest checkpoint by $(( (ckpt_t - tlm_t) / 60 )) min — 'run.sh export' or 'run.sh watch-export-bg'"
    else
      echo "$TLM is up to date with the latest checkpoint"
    fi
  fi
  [ -f "$OUT/train.log" ] && { echo "--- last lines of $OUT/train.log ---"; tail -n 15 "$OUT/train.log"; }
  [ -f "$OUT/log.csv" ] && { echo "--- last eval rows ---"; tail -n 5 "$OUT/log.csv"; }
}

cmd_stop() {
  [ -f "$OUT/train.pid" ] || die "no pid file"
  local p; p="$(cat "$OUT/train.pid")"
  kill "$p" 2>/dev/null && echo "sent SIGTERM to $p" || echo "pid $p not running"
  rm -f "$OUT/train.pid"
  echo "checkpoint kept — 'run.sh train' resumes from it"
}

cmd_export() {
  need_venv
  [ -f "$OUT/latest.pt" ] || die "no checkpoint at $OUT/latest.pt — train first"
  say "export -> $TLM"
  "$PY" "$HERE/export.py" --ckpt "$OUT/latest.pt" --out "$TLM"
}

cmd_sample() {
  [ -f "$TLM" ] || die "no weights at $TLM — run 'run.sh export'"
  local p="${1:-$PROMPT}"
  say "generate  (prompt: '$p')"
  ( cd "$ROOT" && cargo run -q -p thos-lm --example generate --target "$RUST_TARGET" -- \
      --weights "$TLM" --prompt "$p" --max-tokens "$MAXTOK" )
}

SELF_PROMPT="${SELF_PROMPT:-Status report. Today I have been learning to}"

cmd_self_report() {
  # A short, playful self-description generated BY the current checkpoint —
  # not genuine introspection (a base LM has no access to its own loss curve
  # or step count), just flavour text so a training run feels alive. Logged
  # to out/<config>/self_report.log; shown in 'run.sh dashboard'.
  local step="${1:-?}"
  local text
  # `|| true`: this is flavour text, not load-bearing — a transient failure
  # here (cargo lock contention, a mid-export weights file, ...) must never
  # kill the caller, which for watch-export-bg is a `set -euo pipefail` loop
  # that would otherwise die silently and stop refreshing $TLM entirely.
  text="$( ( cd "$ROOT" && cargo run -q --release -p thos-lm --example generate \
      --target "$RUST_TARGET" -- --weights "$TLM" --prompt "$SELF_PROMPT" \
      --max-tokens 50 --temp 0.85 --seed "$(date +%s)" ) 2>/dev/null | tr '\n' ' ' )" || true
  [ -n "$text" ] || return 0
  printf '%s  step %-8s %s\n' "$(date '+%F %T')" "$step" "$text" >> "$OUT/self_report.log"
}

cmd_watch_export() {
  need_venv
  [ -f "$OUT/latest.pt" ] || echo "no checkpoint yet at $OUT/latest.pt — waiting ..."
  say "watch-export  ($OUT/latest.pt -> $TLM, every ${WATCH_SECS:-60}s on change)"
  local last=""
  while :; do
    if [ -f "$OUT/latest.pt" ]; then
      local mtime; mtime="$(stat -c %Y "$OUT/latest.pt" 2>/dev/null || echo "")"
      if [ -n "$mtime" ] && [ "$mtime" != "$last" ]; then
        if "$PY" "$HERE/export.py" --ckpt "$OUT/latest.pt" --out "$TLM"; then
          last="$mtime"
          echo "[watch-export] $(date '+%T') refreshed $TLM"
          local step=""; [ -f "$OUT/log.csv" ] && step="$(tail -n1 "$OUT/log.csv" | cut -d, -f1)"
          cmd_self_report "$step"
        fi
      fi
    fi
    sleep "${WATCH_SECS:-60}"
  done
}

# Foreground watch-export dies silently the moment its terminal closes (no
# nohup, no supervision) — that's exactly how it went stale for hours
# unnoticed. This variant runs it detached with a pid file, matching
# train-bg, so it survives the terminal and 'run.sh status' can see it.
cmd_watch_export_bg() {
  need_venv
  mkdir -p "$OUT"
  if [ -f "$OUT/watch-export.pid" ] && kill -0 "$(cat "$OUT/watch-export.pid")" 2>/dev/null; then
    die "watch-export already running (pid $(cat "$OUT/watch-export.pid")) — 'run.sh watch-export-stop' first"
  fi
  nohup env CONFIG="$CONFIG" TLM="$TLM" bash "${BASH_SOURCE[0]}" watch-export \
    > "$OUT/watch-export.log" 2>&1 &
  echo $! > "$OUT/watch-export.pid"
  say "watch-export in background — pid $(cat "$OUT/watch-export.pid")"
  echo "log: $OUT/watch-export.log"
}

cmd_watch_export_stop() {
  [ -f "$OUT/watch-export.pid" ] || die "no watch-export.pid — nothing tracked as running"
  local p; p="$(cat "$OUT/watch-export.pid")"
  kill "$p" 2>/dev/null && echo "sent SIGTERM to $p" || echo "pid $p not running"
  rm -f "$OUT/watch-export.pid"
}

cmd_ctl() {
  mkdir -p "$OUT"
  case "${1:-}" in
    stop)    echo '{"stop": true}' > "$OUT/control.json" ;;
    pause)   echo '{"pause": true}' > "$OUT/control.json" ;;
    resume)  echo '{}' > "$OUT/control.json" ;;
    lr)      echo "{\"lr_scale\": ${2:?usage: ctl lr <scale>}}" > "$OUT/control.json" ;;
    *) die "usage: run.sh ctl {stop|pause|resume|lr <scale>}  (CONFIG selects which run)" ;;
  esac
  echo "wrote $OUT/control.json: $(cat "$OUT/control.json")"
}

_game_pids() { pgrep -f "[t]rain\.py --config|[p]repare\.py --bpe|[f]etch\.py" || true; }

# Training keeps running while you game — it just gets capped to a slice of
# the CPU (cpu.max quota in a cgroup) instead of being paused, so progress
# never fully stops but the game gets the rest of the machine. Adjustable via
# GAME_CORES (default 6 of the 24 threads, ~25%).
GAME_CG=/sys/fs/cgroup/game-throttle
GAME_CORES="${GAME_CORES:-6}"
GAME_PERIOD=100000

_game_is_on() {
  # cgroup.procs is a virtual file — stat() reports size 0 even with content,
  # so -s always lies here; read it instead.
  [ -d "$GAME_CG" ] && [ -n "$(cat "$GAME_CG/cgroup.procs" 2>/dev/null)" ]
}

_game_on() {
  sudo -n mkdir -p "$GAME_CG"
  local quota=$((GAME_CORES * GAME_PERIOD))
  echo "$quota $GAME_PERIOD" | sudo -n tee "$GAME_CG/cpu.max" >/dev/null
  local pids; pids="$(_game_pids)"
  if [ -n "$pids" ]; then
    for p in $pids; do
      echo "$p" | sudo -n tee "$GAME_CG/cgroup.procs" >/dev/null 2>&1 || true
      renice -n 10 -p "$p" >/dev/null 2>&1 || true
      ionice -c 3 -p "$p" >/dev/null 2>&1 || true
    done
    echo "[game] training capped to ${GAME_CORES}/24 threads (~$((GAME_CORES*100/24))%): $pids"
  else
    echo "[game] cap armed (${GAME_CORES}/24 threads) — will apply once training is running"
  fi
  echo "[game] training keeps going, just slower — go play. 'ml/train/run.sh game off' when done"
}

_game_off() {
  local pids; pids="$(_game_pids)"
  if [ -n "$pids" ]; then
    for p in $pids; do
      echo "$p" | sudo -n tee /sys/fs/cgroup/cgroup.procs >/dev/null 2>&1 || true
      renice -n 0 -p "$p" >/dev/null 2>&1 || true
      ionice -c 2 -n 4 -p "$p" >/dev/null 2>&1 || true
    done
  fi
  [ -d "$GAME_CG" ] && sudo -n rmdir "$GAME_CG" 2>/dev/null || true
  echo "[game] resumed — full throttle again"
}

# Works any number of times, in either direction, whenever you want — this is
# a standing on/off switch, not a one-shot like the initial staged.sh window.
cmd_game() {
  case "${1:-}" in
    on)     _game_on ;;
    off)    _game_off ;;
    toggle) if _game_is_on; then _game_off; else _game_on; fi ;;
    status)
      if _game_is_on; then
        echo "[game] ON — training capped to ${GAME_CORES}/24 threads"
      else
        echo "[game] off — full throttle"
      fi
      local pids; pids="$(_game_pids)"
      [ -n "$pids" ] && echo "[game] live pids: $pids"
      ;;
    *) die "usage: run.sh game {on|off|toggle|status}   (on = cap training's CPU share, off = full throttle; GAME_CORES=N to tune)" ;;
  esac
}

cmd_shell() {
  local w="$TLM"
  [ -f "$w" ] || w="$ROOT/spike-1m.tlm"
  [ -f "$w" ] || die "no weights — train + 'run.sh export' first (looked for $TLM, spike-1m.tlm)"
  say "thos-shell  ($w)"
  ( cd "$ROOT" && cargo run -q --release -p thos-lm --example shell --target "$RUST_TARGET" -- \
      --weights "$w" )
}

cmd_test() {
  say "no_std build"
  ( cd "$ROOT" && cargo build -p thos-lm --target x86_64-unknown-none )
  say "golden cross-check"
  ( cd "$ROOT" && cargo test -p thos-lm --target "$RUST_TARGET" )
  if have_py_mod numpy; then
    say "regenerate fixture"
    "$PY" "$HERE/make_fixture.py"
  fi
}

cmd_all() { cmd_setup; cmd_data; "$0" train; cmd_export; cmd_sample; }

cmd_clean() {
  say "clean"
  rm -rf "$VENV" "$HERE/data" "$HERE/out" "$ROOT"/*.tlm
  echo "removed .venv, data/, out/, *.tlm  (committed fixture under ml/thos-lm/ is untouched)"
}

case "${1:-help}" in
  setup)     cmd_setup ;;
  data)      cmd_data ;;
  train)
    [ -n "${2:-}" ] && die "run.sh train takes no extra args (did you mean 'run.sh status', or 'ctl'/'game' for a running job? got: train $*)"
    cmd_train
    ;;
  train-bg)  cmd_train_bg ;;
  status)    cmd_status ;;
  stop)      cmd_stop ;;
  export)    cmd_export ;;
  eval)      cmd_eval ;;
  sample)    shift || true; cmd_sample "${1:-}" ;;
  shell)     cmd_shell ;;
  watch-export) cmd_watch_export ;;
  watch-export-bg)   cmd_watch_export_bg ;;
  watch-export-stop) cmd_watch_export_stop ;;
  ctl)       shift || true; cmd_ctl "$@" ;;
  game)      shift || true; cmd_game "$@" ;;
  dashboard) shift || true; need_venv; ( cd "$HERE" && "$PY" dashboard.py --config "$CONFIG" --tlm "$TLM" "$@" ) ;;
  test)      cmd_test ;;
  all)       cmd_all ;;
  clean)     cmd_clean ;;
  help|-h|--help)
    awk 'NR>2 && /^#/{sub(/^# ?/,"");print} NR>2 && !/^#/{exit}' "${BASH_SOURCE[0]}" ;;
  *)         die "unknown command '$1' — try: ml/train/run.sh help" ;;
esac
