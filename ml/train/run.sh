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
#   ml/train/run.sh train          train (auto-resumes from out/latest.pt)
#   ml/train/run.sh train-bg       same, in the background -> out/train.log
#   ml/train/run.sh status         show background training progress
#   ml/train/run.sh stop           stop background training
#   ml/train/run.sh export         out/latest.pt  ->  spike-1m.tlm
#   ml/train/run.sh sample "Text"  build the Rust engine + generate from the .tlm
#   ml/train/run.sh test           no_std build + golden cross-check + fixture
#   ml/train/run.sh clean          remove .venv, data/, out/, *.tlm
#
# Overridable via env:  CONFIG=  TLM=  PROMPT=  MAXTOK=  PYTORCH_INDEX=

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
VENV="$HERE/.venv"
PY="$VENV/bin/python"
OUT="$HERE/out"
CONFIG="${CONFIG:-$HERE/config/spike-1m.toml}"
TLM="${TLM:-$ROOT/spike-1m.tlm}"
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
  say "prepare (tokenize + pack)"
  "$PY" "$HERE/prepare.py"
}

_train_ready() { [ -f "$HERE/data/train.bin" ] && [ -f "$HERE/data/val.bin" ]; }

cmd_train() {
  need_venv
  _train_ready || cmd_data
  local resume=()
  [ -f "$OUT/latest.pt" ] && { resume=(--resume); echo "resuming from $OUT/latest.pt"; }
  say "train  ($(basename "$CONFIG"))"
  exec "$PY" "$HERE/train.py" --config "$CONFIG" "${resume[@]}"
}

cmd_train_bg() {
  need_venv
  _train_ready || cmd_data
  mkdir -p "$OUT"
  if [ -f "$OUT/train.pid" ] && kill -0 "$(cat "$OUT/train.pid")" 2>/dev/null; then
    die "training already running (pid $(cat "$OUT/train.pid")) — 'run.sh status' or 'run.sh stop'"
  fi
  local resume=()
  [ -f "$OUT/latest.pt" ] && resume=(--resume)
  nohup "$PY" "$HERE/train.py" --config "$CONFIG" "${resume[@]}" > "$OUT/train.log" 2>&1 &
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
  rm -rf "$VENV" "$HERE/data" "$OUT" "$ROOT"/*.tlm
  echo "removed .venv, data/, out/, *.tlm  (committed fixture under ml/thos-lm/ is untouched)"
}

case "${1:-help}" in
  setup)     cmd_setup ;;
  data)      cmd_data ;;
  train)     cmd_train ;;
  train-bg)  cmd_train_bg ;;
  status)    cmd_status ;;
  stop)      cmd_stop ;;
  export)    cmd_export ;;
  sample)    shift || true; cmd_sample "${1:-}" ;;
  test)      cmd_test ;;
  all)       cmd_all ;;
  clean)     cmd_clean ;;
  help|-h|--help)
    awk 'NR>2 && /^#/{sub(/^# ?/,"");print} NR>2 && !/^#/{exit}' "${BASH_SOURCE[0]}" ;;
  *)         die "unknown command '$1' — try: ml/train/run.sh help" ;;
esac
