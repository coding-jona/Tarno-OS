#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
#
# One-shot staged pipeline for the P1b "more data, then full-stack training" run.
#
#   phase 1  (now .. now + GENTLE_HOURS)   gentle window
#            Only the network fetch runs, niced + ioniced and thread-capped, so
#            a game (Minecraft) keeps its CPU / RAM headroom. The GPU is never
#            touched by this pipeline (PyTorch CPU wheel).
#
#   phase 2  (after the window)            full throttle
#            BPE prepare, then training, using every core at normal priority.
#
# The hand-off is purely time-based and one-time. Resumable: the unlock time is
# persisted under out/staged/, so killing and restarting resumes the schedule
# (and the fetch itself is resumable across the nightly internet cut-off).
#
#   ml/train/staged.sh start [--hours 6]   launch in the background (nohup)
#   ml/train/staged.sh status              where in the schedule we are
#   ml/train/staged.sh stop                stop the driver (keeps any checkpoint)
#   ml/train/staged.sh run                 run in the foreground (used internally)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="$HERE/out/staged"
UNLOCK="$STATE/unlock_at"
LOG="$STATE/staged.log"
PIDF="$STATE/staged.pid"

GENTLE_HOURS="${GENTLE_HOURS:-6}"
GENTLE_THREADS="${GENTLE_THREADS:-2}"
BPE="${BPE:-16384}"
CONFIG="${CONFIG:-$HERE/config/small-30m.toml}"
TLM="${TLM:-$(cd "$HERE/../.." && pwd)/small-30m.tlm}"
PY="$HERE/.venv/bin/python"

say() { printf '\n\033[1m== %s\033[0m  %s\n' "$1" "$(date '+%F %T')"; }

cmd_start() {
  [ -x "$PY" ] || { echo "run ml/train/run.sh setup first (no .venv)"; exit 1; }
  mkdir -p "$STATE"
  if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then
    echo "already running (pid $(cat "$PIDF")) — 'staged.sh status'"; exit 1
  fi
  local hours="$GENTLE_HOURS"
  [ "${1:-}" = "--hours" ] && hours="${2:?--hours needs a number}"
  echo $(( $(date +%s) + hours * 3600 )) > "$UNLOCK"
  : > "$LOG"
  setsid "$BASH" "$0" run < /dev/null >> "$LOG" 2>&1 &
  echo $! > "$PIDF"
  echo "staged pipeline started (pid $(cat "$PIDF"))"
  echo "  gentle window until : $(date -d "@$(cat "$UNLOCK")" '+%F %T')  (~${hours} h, fetch only)"
  echo "  then                : prepare (BPE $BPE) + full-throttle training -> $TLM"
  echo "  watch               : tail -f $LOG   |   ml/train/staged.sh status"
}

cmd_status() {
  if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then
    echo "driver RUNNING (pid $(cat "$PIDF"))"
  else
    echo "driver not running"
  fi
  if [ -f "$UNLOCK" ]; then
    local now u; now="$(date +%s)"; u="$(cat "$UNLOCK")"
    if [ "$now" -lt "$u" ]; then
      echo "phase: GENTLE — $(( (u - now + 59) / 60 )) min of fetch-only left (until $(date -d "@$u" '+%T'))"
    else
      echo "phase: FULL THROTTLE (window ended $(date -d "@$u" '+%F %T'))"
    fi
  fi
  echo "--- data/raw ---"
  ls -1 "$HERE/data/raw" 2>/dev/null | wc -l | xargs printf '  %s files\n'
  du -sh "$HERE/data/raw" 2>/dev/null | awk '{print "  "$1" on disk"}'
  [ -f "$LOG" ] && { echo "--- last staged.log ---"; tail -n 12 "$LOG"; } || true
  local tl="$STATE/train.log"
  [ -f "$tl" ] && { echo "--- last train.log ---"; tail -n 12 "$tl"; } || true
}

cmd_stop() {
  if [ -f "$PIDF" ]; then
    local p; p="$(cat "$PIDF")"
    kill -TERM -- -"$p" 2>/dev/null || kill "$p" 2>/dev/null || true
    echo "sent SIGTERM to driver group $p"
    rm -f "$PIDF"
  else
    echo "no driver pid file"
  fi
  rm -f "$STATE/train.pid"
  echo "stopped — any checkpoint under out/ is kept; re-run 'staged.sh start' or 'run.sh train'"
}

cmd_run() {
  trap 'echo "[staged] exiting"; rm -f "$PIDF"' EXIT
  local u; u="$(cat "$UNLOCK")"

  say "phase 1: gentle fetch"
  echo "[staged] capping threads to $GENTLE_THREADS, nice 19 + ionice idle, until $(date -d "@$u")"
  export OMP_NUM_THREADS="$GENTLE_THREADS" MKL_NUM_THREADS="$GENTLE_THREADS" \
         OPENBLAS_NUM_THREADS="$GENTLE_THREADS" NUMEXPR_NUM_THREADS="$GENTLE_THREADS"
  while :; do
    nice -n 19 ionice -c 3 "$PY" "$HERE/fetch.py" || echo "[staged] fetch incomplete (internet window?) — will retry"
    now="$(date +%s)"
    [ "$now" -ge "$u" ] && break
    # fetch returns 0 only when every file verified; otherwise wait and retry.
    if nice -n 19 "$PY" "$HERE/fetch.py" --verify >/dev/null 2>&1; then
      remain=$(( u - now ))
      echo "[staged] corpus complete; holding the gentle window for $(( remain / 60 )) more min"
      sleep "$(( remain < 600 ? remain : 600 ))"
    else
      sleep 300
    fi
    [ "$(date +%s)" -ge "$u" ] && break
  done

  say "phase 2: full throttle"
  unset OMP_NUM_THREADS MKL_NUM_THREADS OPENBLAS_NUM_THREADS NUMEXPR_NUM_THREADS
  echo "[staged] prepare: BPE $BPE"
  "$PY" -u "$HERE/prepare.py" --bpe "$BPE"

  local outdir; outdir="$HERE/out/$(basename "${CONFIG%.toml}")"
  mkdir -p "$outdir"
  local resume=()
  [ -f "$outdir/latest.pt" ] && resume=(--resume) || true
  say "training"
  echo "[staged] config $(basename "$CONFIG")  ->  checkpoints in $outdir/  ->  export $TLM"
  echo "[staged] ~30 s/step on this CPU; a checkpoint lands every ckpt_interval steps."
  echo "[staged] auto-exporting to $TLM whenever one lands (chat with it live via 'run.sh shell'):"
  CONFIG="$CONFIG" TLM="$TLM" "$HERE/run.sh" watch-export >> "$STATE/watch-export.log" 2>&1 &
  echo $! > "$STATE/watch-export.pid"
  "$PY" -u "$HERE/train.py" --config "$CONFIG" "${resume[@]}" 2>&1 | tee "$STATE/train.log"
}

BASH="$(command -v bash)"
case "${1:-status}" in
  start)  shift; cmd_start "$@" ;;
  status) cmd_status ;;
  stop)   cmd_stop ;;
  run)    cmd_run ;;
  *) echo "usage: ml/train/staged.sh {start [--hours N]|status|stop}"; exit 1 ;;
esac
