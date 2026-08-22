# Linux-OS-Architektur — Grundlagen

Siehe [`00-index.md`](00-index.md) für Zweck/Status dieses Verzeichnisses:
reine Recherche, nichts hiervon ist Code-Änderung an Tarno OS.

Primärquelle für dieses Dokument: das *Linux From Scratch* (LFS) Buch,
Version 12.1 —
[LFS-BOOK-12.1.pdf](https://www.linuxfromscratch.org/lfs/downloads/12.1/LFS-BOOK-12.1.pdf).
LFS baut ein komplettes Linux-System von Quellcode aus per Hand auf und
erklärt dabei jeden Schritt — die didaktisch gründlichste frei verfügbare
Quelle zu "wie ist ein Linux-System aufgebaut".

## Kernel/Userspace-Trennung

Ein Linux-System besteht aus zwei klar getrennten Ebenen:

- **Kernel-Space**: der Linux-Kernel selbst — Prozess-Scheduler,
  Speicherverwaltung, Gerätetreiber, Dateisysteme, Netzwerk-Stack. Läuft
  mit vollen Hardware-Privilegien (Ring 0 auf x86).
- **User-Space**: alles andere — Init-System, Shell, Programme,
  Bibliotheken (allen voran `glibc`/`musl` als C-Standardbibliothek, über
  die praktisch jedes Userspace-Programm mit dem Kernel spricht).

Der Übergang zwischen beiden läuft über **Syscalls** (`open`, `read`,
`execve`, `fork`, …) — ein User-Space-Prozess kann den Kernel nur über
diese fest definierte Schnittstelle ansprechen, nie direkt auf Hardware
zugreifen. Das ist auch die Grundlage, auf der Tarno OS' eBPF-basierte
Behavioral Security aufsetzt (siehe
[`docs/architecture.md`](../architecture.md)): eBPF-Programme hängen sich
an Tracepoints im Kernel (z. B. `sched_process_exec`, ausgelöst bei jedem
`execve`-Syscall) und beobachten so Userspace-Aktivität, ohne selbst
Userspace-Code zu patchen.

## Boot-Prozess: Firmware → Bootloader → Kernel → Init

Grob in dieser Reihenfolge (Details zu Bootloader/Kernel in
[`03-bootloader-init-kernel-basics.md`](03-bootloader-init-kernel-basics.md)):

1. **Firmware** (BIOS oder UEFI) initialisiert die Hardware minimal
   (CPU, RAM-Timing, liest Bootgerät) und übergibt an den Bootloader.
2. **Bootloader** (z. B. SYSLINUX, GRUB) lädt den Kernel (und ggf. eine
   initramfs/initrd) in den Speicher und startet ihn mit den konfigurierten
   Kernel-Parametern (`root=`, `console=`, …).
3. **Kernel** initialisiert sich selbst, erkennt Hardware über seine
   einkompilierten/geladenen Treiber, mountet das Root-Dateisystem
   (direkt oder über eine initramfs als Zwischenschritt) und startet als
   allerersten Userspace-Prozess **PID 1** — das Init-System.
4. **Init** (PID 1) startet alle weiteren Systemdienste in der
   konfigurierten Reihenfolge und bleibt für die Prozesslebensdauer des
   Systems der "Elternprozess" aller verwaisten Prozesse.

## Prozessmodell

Jeder Linux-Prozess entsteht durch `fork()` (Duplikat eines existierenden
Prozesses) gefolgt von `exec()` (Duplikat durch neues Programm-Image
ersetzen) — mit der einzigen Ausnahme von PID 1, das der Kernel direkt
startet. Prozesse bilden dadurch einen Baum mit PID 1 als Wurzel; stirbt
ein Elternprozess, werden seine Kinder von PID 1 (oder einem
Subreaper) "adoptiert" (reparented). Dieses Baummodell ist u. a. relevant
für Tarno OS' `process_ctl`-Modul (SIGSTOP/SIGCONT gegen einzelne PIDs,
siehe [`docs/month3-tarno-layer.md`](../month3-tarno-layer.md)).

## Filesystem Hierarchy Standard (FHS)

Der FHS definiert, wo welche Art von Datei im Dateisystem liegt — z. B.:

| Pfad | Zweck |
|---|---|
| `/bin`, `/sbin` | essentielle Programme (heute auf vielen Systemen nach `/usr/bin` verlinkt, "usr-merge") |
| `/etc` | System-Konfiguration |
| `/var` | veränderliche Laufzeitdaten (Logs, Caches, Spool) |
| `/run` | flüchtige Laufzeitdaten seit dem letzten Boot (PID-Files, Sockets) |
| `/proc`, `/sys` | virtuelle Kernel-Schnittstellen (keine echten Dateien auf Platte) |
| `/usr` | der Großteil des eigentlichen Systems (Programme, Bibliotheken, geteilte Daten) |

Tarno OS folgt dem FHS bereits an den Stellen, die für `tarnod` relevant
sind: Socket unter `/run/tarnod/`, Secrets unter `/etc/tarnod/` (siehe
[`docs/month3-tarno-layer.md`](../month3-tarno-layer.md#api-key-vault)).
`/proc/meminfo` und `/sys/devices/system/cpu/...` — beides virtuelle
FHS-Pfade — sind außerdem die Datenquellen für `gaming.rs` und für Tarno
AI's `SystemContext` (siehe [`docs/month3-tarno-layer.md`](../month3-tarno-layer.md#tarno-ai)).

## Init-Systeme im Vergleich

| Init | Ansatz | Vorteile | Nachteile |
|---|---|---|---|
| **sysvinit** | sequenzielle Shell-Skripte (`/etc/init.d/`), Runlevel-Konzept | einfach, jeder Schritt nachvollziehbar | langsam (streng seriell), kein natives Dependency-Tracking |
| **systemd** | deklarative Unit-Files, paralleler Start, cgroups-Integration, D-Bus-zentriert | schnell, sehr feature-reich (Logging, Timer, Socket-Activation) | groß, viele Abhängigkeiten, "macht mehr als nur Init" (umstritten für minimale Systeme) |
| **OpenRC** | Shell-Skript-basiert wie sysvinit, aber mit explizitem Dependency-Graph und parallelem Start | leichtgewichtig, kein zusätzlicher Daemon-Unterbau nötig, dependency-aware | weniger Feature-Umfang als systemd (kein natives Unit-Timer-System etc.) |

**Bezug zu Tarno OS' bisheriger Entscheidung:** Tarno OS nutzt heute
OpenRC (`board/tarno-m6700/rootfs-overlay/etc/init.d/S60tarnod`, siehe
[`docs/architecture.md`](../architecture.md) und
[`docs/month1-foundation.md`](../month1-foundation.md)) — bewusst gegen
systemd, weil für ein extrem gestripptes Single-Purpose-Gaming-System der
zusätzliche Funktionsumfang von systemd (Timer, Netzwerk-Management,
Login-Management, …) nicht gebraucht wird und RAM/Komplexität kostet, aber
sysvinits rein sequenzieller Start unnötig langsam wäre. Für eine
künftige Debian-Basis (siehe
[`02-debian-base-system-building.md`](02-debian-base-system-building.md))
ist das relevant: Debian nutzt standardmäßig systemd, OpenRC ist aber
paketiert und offiziell unterstützt (`apt install openrc`,
Debian-Wiki-Seite "OpenRC") — die bisherige Init-Entscheidung müsste bei
einem Debian-Wechsel also nicht zwangsläufig aufgegeben werden, sondern
könnte als bewusste Abweichung vom Debian-Default weitergeführt werden
(Details/Abwägung: siehe
[`04-tarno-os-debian-migration-notes.md`](04-tarno-os-debian-migration-notes.md)).

## Quelle

- Linux From Scratch, Version 12.1: <https://www.linuxfromscratch.org/lfs/downloads/12.1/LFS-BOOK-12.1.pdf>
