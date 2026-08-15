#!/usr/bin/env bash
# Gaming-Mode-Tuning für Tarno OS: CPU-Governor, Transparent-HugePages,
# isolcpus-Status. Begründung/Details: docs/month2-gaming-tuning.md.
#
# Bewusst NICHT über tarnod/IPC verdrahtet — einfaches, auditierbares Skript
# für einen einmaligen Vorgang beim Spielstart (siehe docs/architecture.md).
#
# Usage:
#   gaming-mode.sh status         # aktuellen Zustand anzeigen
#   gaming-mode.sh start          # Gaming-Mode aktivieren (performance-Governor, THP=madvise)
#   gaming-mode.sh stop           # zurück zu Alltags-Defaults (powersave-Governor, THP=always)
#
# --dry-run (oder TARNOD_DRY_RUN=1): nur loggen, nichts schreiben. Auf
# Systemen ohne die passenden sysfs-Pfade (z.B. in dieser Sandbox, ohne
# reale M6700-Hardware/cpufreq) werden fehlende Pfade übersprungen statt
# einen Fehler zu werfen.
set -euo pipefail
shopt -s nullglob

DRY_RUN=0
CMD="status"
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        status|start|stop) CMD="$arg" ;;
        *)
            echo "usage: $0 <status|start|stop> [--dry-run]" >&2
            exit 2
            ;;
    esac
done
if [ "${TARNOD_DRY_RUN:-0}" = "1" ]; then
    DRY_RUN=1
fi

log() { echo "[gaming-mode] $*" >&2; }

write_sysfs() {
    local path="$1" value="$2"
    if [ ! -e "$path" ]; then
        log "übersprungen (nicht vorhanden): $path"
        return 0
    fi
    if [ "$DRY_RUN" = "1" ]; then
        log "[dry-run] echo $value > $path"
        return 0
    fi
    if ! echo "$value" > "$path" 2>/dev/null; then
        log "warnung: konnte nicht schreiben (Berechtigung/Kernel-Support?): $path"
        return 0
    fi
    log "gesetzt: $path = $value"
}

isolated_cpus_status() {
    local path=/sys/devices/system/cpu/isolated
    if [ -e "$path" ]; then
        local val
        val="$(cat "$path")"
        if [ -n "$val" ]; then
            log "isolcpus aktiv: $val"
        else
            log "isolcpus-Datei vorhanden, aber leer (kein isolcpus=-Boot-Parameter aktiv)"
        fi
    else
        log "isolcpus nicht verfügbar auf diesem System ($path fehlt)"
    fi
}

governor_status() {
    local any=0 path
    for path in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
        [ -e "$path" ] || continue
        any=1
        log "$path = $(cat "$path")"
    done
    if [ "$any" = "0" ]; then
        log "keine cpufreq/scaling_governor-Pfade gefunden (in virtualisierten Umgebungen ohne cpufreq-Treiber normal)"
    fi
}

thp_status() {
    local path=/sys/kernel/mm/transparent_hugepage/enabled
    if [ -e "$path" ]; then
        log "THP: $(cat "$path")"
    else
        log "THP-Steuerung nicht verfügbar ($path fehlt)"
    fi
}

set_governor() {
    local value="$1" path any=0
    for path in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
        any=1
        write_sysfs "$path" "$value"
    done
    if [ "$any" = "0" ]; then
        log "übersprungen: keine cpufreq/scaling_governor-Pfade gefunden"
    fi
}

set_thp() {
    write_sysfs /sys/kernel/mm/transparent_hugepage/enabled "$1"
}

case "$CMD" in
    status)
        isolated_cpus_status
        governor_status
        thp_status
        ;;
    start)
        log "aktiviere Gaming-Mode ..."
        set_governor performance
        # madvise statt always: vermeidet Latenz-Spikes durch Hintergrund-
        # Compaction während des Spiels, siehe docs/month2-gaming-tuning.md.
        set_thp madvise
        isolated_cpus_status
        ;;
    stop)
        log "deaktiviere Gaming-Mode (zurück zu Alltags-Defaults) ..."
        set_governor powersave
        set_thp always
        ;;
esac
