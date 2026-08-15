#!/usr/bin/env bash
# Parst ein MangoHud-CSV-Log und berechnet Durchschnitts-FPS, 1%-Low und
# Frametime-Standardabweichung (Konsistenz-Metrik) — Grundlage für den
# Vorher/Nachher-Vergleich aus docs/month2-gaming-tuning.md.
#
# Usage: benchmark.sh <mangohud-log.csv>
#
# Erwartetes Format: MangoHud-CSV-Export mit einer "fps"-Spalte (Standard
# bei MANGOHUD_CONFIG=output_folder=...,log_duration=...). Siehe
# https://github.com/flightlessmango/MangoHud für das Log-Format.
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: $0 <mangohud-log.csv>" >&2
    exit 2
fi

LOGFILE="$1"
if [ ! -f "$LOGFILE" ]; then
    echo "fehler: Datei nicht gefunden: $LOGFILE" >&2
    exit 1
fi

exec python3 - "$LOGFILE" << 'PYEOF'
import csv
import statistics
import sys

path = sys.argv[1]
fps_values = []

with open(path, newline="") as f:
    reader = csv.DictReader(f)
    if reader.fieldnames is None or "fps" not in reader.fieldnames:
        print("fehler: Spalte 'fps' nicht im CSV-Header gefunden", file=sys.stderr)
        print(f"gefundene Spalten: {reader.fieldnames}", file=sys.stderr)
        sys.exit(1)
    for row in reader:
        try:
            fps_values.append(float(row["fps"]))
        except (TypeError, ValueError):
            continue

if not fps_values:
    print("fehler: keine gültigen FPS-Werte gefunden", file=sys.stderr)
    sys.exit(1)

fps_values.sort()
avg = statistics.mean(fps_values)
onepct_low_count = max(1, len(fps_values) // 100)
onepct_low = statistics.mean(fps_values[:onepct_low_count])

frametimes_ms = [1000.0 / v for v in fps_values if v > 0]
frametime_stddev = statistics.pstdev(frametimes_ms) if len(frametimes_ms) > 1 else 0.0

print(f"Samples:            {len(fps_values)}")
print(f"Durchschnitts-FPS:  {avg:.1f}")
print(f"1%-Low FPS:         {onepct_low:.1f}")
print(f"Frametime-StdDev:   {frametime_stddev:.2f} ms  (niedriger = konsistenter)")
PYEOF
