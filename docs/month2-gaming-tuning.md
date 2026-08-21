# Monat 2 — Gaming-Mode-Tuning (Detailplan)

Übersicht siehe [`ROADMAP.md`](../ROADMAP.md#monat-2--gaming-mode-tuning). Umgesetzt als `scripts/gaming-mode.sh`, `scripts/jvm-launch.sh`, `scripts/benchmark.sh` (siehe `scripts/`).

## Woche 5-6: CPU/Scheduling

### Core-Isolation (`isolcpus`)
- Kernel-Boot-Parameter, nicht zur Laufzeit änderbar → wird in `tarno-br2-external/board/tarno-m6700/genimage.cfg`/Bootloader-Config (z. B. GRUB/extlinux `APPEND`-Zeile) gesetzt: `isolcpus=2,3` (Beispiel: 2 von 4 Kernen dem OS entziehen, Minecraft/JVM bekommt sie exklusiv). Konkrete Kern-Zahl hängt vom tatsächlichen M6700-CPU-Modell ab (wird nach erstem Boot in dieses Dokument nachgetragen).
- `scripts/gaming-mode.sh status` liest `/sys/devices/system/cpu/isolated` und meldet, ob der Boot-Parameter aktiv ist — das Skript kann `isolcpus` nicht selbst setzen (Boot-Zeit-Parameter), aber verifizieren und dem Nutzer sagen, was in der Bootloader-Config fehlt, falls nicht aktiv.

### `cset`/`taskset` + `sched_setaffinity`
- `scripts/gaming-mode.sh start` nutzt `taskset` (Teil von util-linux, in Buildroot über `BR2_PACKAGE_UTIL_LINUX_TASKSET=y`), um alle nicht-isolierten Prozesse auf den nicht-isolierten Cores zu halten, und startet die JVM (via `jvm-launch.sh`) explizit mit `taskset -c 2,3`.
- `cset shield` ist eine Alternative mit mehr Automatik (verschiebt automatisch alle laufenden Tasks weg von den geshieldeten Cores), aber zusätzliche Python-Abhängigkeit (`cpuset`-Paket) — für Tarno OS bewusst **nicht** gewählt, um die Paketliste klein zu halten; `taskset` reicht, da Tarno OS ohnehin nur wenige Hintergrundprozesse hat.

### Real-Time-Priorität (`chrt`)
- `scripts/jvm-launch.sh` bietet einen `--rt`-Schalter, der die JVM mit `chrt -f 10` (SCHED_FIFO, moderate Priorität 10 von 99) startet — **nicht** standardmäßig aktiv, da eine falsch konfigurierte RT-Priorität den restlichen Userspace verhungern lassen kann (Watchdog-Risiko, siehe ROADMAP-Warnhinweis). Das Skript prüft vor dem Setzen, ob `CAP_SYS_NICE` vorhanden ist, und gibt sonst eine klare Fehlermeldung statt eines stillen Fallbacks.

### CPU-Governor
- `scripts/gaming-mode.sh start` schreibt `performance` in `/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor` für alle Cores. Fällt der Pfad nicht vorhanden (z. B. `intel_pstate` im passiven vs. aktiven Modus, oder Governor-Sysfs fehlt in virtualisierten Testumgebungen), meldet das Skript das explizit statt einen Fehler zu verschlucken.

## Woche 7-8: Memory & Display

### Transparent HugePages (THP)
- Ziel-Modus: **`madvise`**, nicht `always` (Begründung aus der Roadmap: `always` kann Latenz-Spikes durch Hintergrund-Compaction verursachen — für ein Gaming-System mit Frametime-Konsistenz als Ziel kontraproduktiv).
- Systemweit: `echo madvise > /sys/kernel/mm/transparent_hugepage/enabled` (Teil von `gaming-mode.sh start`).
- JVM-seitig: `-XX:+UseTransparentHugePages` allein reicht nicht — die JVM muss den Heap-Speicher zusätzlich per `madvise(MADV_HUGEPAGE)` markieren, was moderne OpenJDK-Versionen (11+) mit dieser Flag-Kombination automatisch tun. `jvm-launch.sh` setzt zusätzlich `-XX:+AlwaysPreTouch`, damit die HugePages beim Start statt während des Spiels alloziert werden (vermeidet Stotterer durch Nachallozieren mitten im Gameplay).

### Compositor-Bypass / Direct-Scanout
- `cage` (siehe Monat 1) rendert bereits ohne zusätzlichen Compositor-Layer über einen anderen Fenstermanager — das ist der Hauptteil des "Bypass". Zusätzlicher Test: DRM-Direct-Scanout verifizieren mit `WAYLAND_DEBUG=1` oder `wlr-randr`, ob die JVM/das Spiel im Fullscreen tatsächlich einen eigenen DRM-Plane bekommt (kein Extra-Compositing-Pass) — dokumentiert als manueller Verifikationsschritt auf der echten Hardware (GPU-abhängig, in dieser Sandbox ohne GPU nicht sinnvoll testbar).
- `cage` ist aktuell der einzige Compositor im Repo und bleibt bewusst auf den Gaming-/Kiosk-Fall beschränkt (ein Fenster fullscreen, keine Taskleiste/Fensterverwaltung) — der frühere separate Desktop-Compositor `tarno-desktop` wurde im Zuge der GUI-Entfernung aus dem Repo entfernt (siehe ROADMAP.md, Abschnitt "Zurückgestellt").

### FPS/Frametime-Messung
- `scripts/benchmark.sh` parst ein `mangohud`-kompatibles CSV-Log (Mangohud kann Frametimes in eine Datei loggen: `MANGOHUD_CONFIG=output_folder=/path,log_duration=60`) und berechnet: Durchschnitts-FPS, 1%-Low, Frametime-Standardabweichung (Konsistenz-Metrik) — Grundlage für den Vorher/Nachher-Vergleich aus der Roadmap.

## Scope-Hinweis für diese Sandbox

Die Skripte in `scripts/` sind hier **syntaktisch geprüft und im `--dry-run`-Modus lauffähig getestet** (keine reale Hardware nötig für den Kontrollfluss). Reale Wirkung (tatsächliche Governor-Änderung, echte isolierte Cores, GPU-Direct-Scanout, gemessene FPS) kann nur auf dem M6700 selbst verifiziert werden — konkrete Zahlen werden nach erstem Testlauf nachgetragen.
