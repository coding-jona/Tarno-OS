#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
#
# The "full load" schedule — not synthetic filler load, but the actual
# training running unthrottled: raises the training cgroup's RAM ceiling so
# big-1b stops fighting a tight cap and can hold much more of its state in
# real RAM instead of swap, which makes it genuinely faster. Outside the
# window it drops back to a small cap so the desktop stays responsive during
# the day. (An earlier version used synthetic stress-ng load instead — that
# measurably slowed real training down for no benefit, since training never
# touches the GPU. Dropped.)
#
# Does not touch small-30m (already runs uncapped/full-speed always) or stop
# any training process — only changes how much real RAM big-1b is allowed.
#
#   ml/train/fullload.sh start   # full load: raise the RAM cap (fast, less desktop headroom)
#   ml/train/fullload.sh stop    # gentle: drop the RAM cap back down (desktop-friendly)
#   ml/train/fullload.sh status

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="$HERE/out/fullload"
mkdir -p "$STATE"
LOG="$STATE/fullload.log"
CG=/sys/fs/cgroup/training

# Full-load cap: generous, close to letting big-1b use most of real RAM.
# Gentle cap: the original 2G, leaves the desktop comfortably usable.
FULL_CAP="${FULL_CAP:-13G}"
GENTLE_CAP="${GENTLE_CAP:-2G}"

ensure_cgroup() {
  if [ ! -d "$CG" ]; then
    sudo mkdir -p "$CG"
    echo "60G" | sudo tee "$CG/memory.swap.max" >/dev/null
  fi
}

case "${1:-status}" in
  start)
    ensure_cgroup
    echo "$FULL_CAP" | sudo tee "$CG/memory.max" >/dev/null
    echo "$(date '+%F %T') full load ON — memory.max=$FULL_CAP" >> "$LOG"
    ;;
  stop)
    ensure_cgroup
    echo "$GENTLE_CAP" | sudo tee "$CG/memory.max" >/dev/null
    echo "$(date '+%F %T') full load off — memory.max=$GENTLE_CAP" >> "$LOG"
    ;;
  status)
    if [ -d "$CG" ]; then
      echo "memory.max      = $(cat "$CG/memory.max")"
      echo "memory.current   = $(cat "$CG/memory.current")"
      echo "memory.swap.current = $(cat "$CG/memory.swap.current")"
    else
      echo "cgroup not set up yet"
    fi
    ;;
  *) echo "usage: fullload.sh {start|stop|status}"; exit 1 ;;
esac
