#!/usr/bin/env bash
# Startet die JVM (z.B. für den Minecraft-Server/-Client) mit Core-
# Affinität, optionaler Real-Time-Priorität und THP-freundlichen Heap-
# Flags. Begründung/Details: docs/month2-gaming-tuning.md.
#
# Usage:
#   jvm-launch.sh [--cpus 2,3] [--rt] [--dry-run] -- <java-args...>
#
# Beispiel:
#   jvm-launch.sh --cpus 2,3 -- -jar minecraft_server.jar nogui
#
# --rt setzt SCHED_FIFO-Priorität 10 via chrt — bewusst NICHT Standard
# (siehe ROADMAP.md-Warnhinweis: falsch konfigurierte RT-Priorität kann
# den restlichen Userspace verhungern lassen).
set -euo pipefail

CPUS=""
USE_RT=0
DRY_RUN=0
JAVA_BIN="${JAVA_BIN:-java}"
JAVA_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --cpus)
            CPUS="$2"
            shift 2
            ;;
        --rt)
            USE_RT=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --)
            shift
            JAVA_ARGS=("$@")
            break
            ;;
        *)
            echo "unbekanntes Argument: $1 (java-Argumente müssen nach '--' stehen)" >&2
            exit 2
            ;;
    esac
done
if [ "${TARNOD_DRY_RUN:-0}" = "1" ]; then
    DRY_RUN=1
fi

log() { echo "[jvm-launch] $*" >&2; }

# -XX:+UseTransparentHugePages + AlwaysPreTouch: Heap wird beim Start
# (statt während des Spiels) mit HugePages alloziert, vermeidet Stotterer
# durch Nachallozieren mitten im Gameplay.
JVM_FLAGS=(
    -XX:+UseTransparentHugePages
    -XX:+AlwaysPreTouch
)

CMD=("$JAVA_BIN" "${JVM_FLAGS[@]}" "${JAVA_ARGS[@]}")

if [ -n "$CPUS" ]; then
    if ! command -v taskset >/dev/null 2>&1; then
        log "warnung: taskset nicht gefunden, starte ohne Core-Affinität"
    else
        CMD=(taskset -c "$CPUS" "${CMD[@]}")
    fi
fi

if [ "$USE_RT" = "1" ]; then
    if ! command -v chrt >/dev/null 2>&1; then
        log "warnung: chrt nicht gefunden, starte ohne RT-Priorität"
    else
        CMD=(chrt -f 10 "${CMD[@]}")
    fi
fi

log "starte: ${CMD[*]}"
if [ "$DRY_RUN" = "1" ]; then
    log "[dry-run] Prozess wird NICHT tatsächlich gestartet"
    exit 0
fi

exec "${CMD[@]}"
