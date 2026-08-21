# Tarno OS – Realistische 3-Monats-Roadmap

**Prämisse:** Tarno OS ist heute **shell-only** — kein GUI-Layer im Repo (der
frühere GUI/Interface-Kram wurde bewusst entfernt, siehe "Zurückgestellt"
unten). Kein eigener Kernel. Basis ist aktuell ein extrem gestripptes Linux
via Buildroot-Cross-Compile mit eigenem `tarnod`-Daemon und minimaler Shell;
langfristig soll diese Basis durch ein **Debian**-Fundament ersetzt werden
(siehe "Zukunft — Debian-Basis" unten) — das ist ein Kurswechsel, der aktuell
nur dokumentiert, aber noch nicht umgesetzt ist. Erreicht ~90% der Ziele aus
dem Manifest in umsetzbarer Zeit, weil JVM, Treiber, Dateisystem und
Scheduler schon funktionieren und du dich auf Tuning statt Kernel-Entwicklung
konzentrierst.

**Zielhardware:** Dell Precision M6700 (Ivy Bridge, AHCI, PS/2) — läuft problemlos mit Standard-Linux-Kernel + Treibern, kein Custom-Boot nötig.

> Dieses Dokument ist die Übersicht. Die technische Ausarbeitung mit konkreten Befehlen, Paketnamen, Kernel-Configs und Abnahmekriterien steht in [`docs/architecture.md`](docs/architecture.md), [`docs/month1-foundation.md`](docs/month1-foundation.md), [`docs/month2-gaming-tuning.md`](docs/month2-gaming-tuning.md), [`docs/month3-tarno-layer.md`](docs/month3-tarno-layer.md) und [`docs/month4-full-os.md`](docs/month4-full-os.md) (zurückgestellter Detailplan: Festplatten-Installer, Updates/App-Marktplatz, Terminal, Netzwerk). Lauffähiger Code liegt in [`tarnod/`](tarnod/) (Daemon+CLI), [`tarno-guard-ebpf/`](tarno-guard-ebpf/) (eBPF-Security), [`scripts/`](scripts/) (Gaming-Mode-Tuning) und [`tarno-br2-external/`](tarno-br2-external/) (Buildroot-Integration).

---

## Monat 1 — Fundament & Low-RAM-Base

**Woche 1-2: Basis-System**
- Buildroot **oder** Alpine Linux als Startpunkt wählen (Buildroot = volle Kontrolle, mehr Aufwand; Alpine = musl-basiert, schon sehr schlank, schneller Start)
- Kein systemd → OpenRC oder eigenes minimales Init-Script
- Kernel-Config strippen: nur Treiber für M6700-Hardware (AHCI, i915/Intel-GPU, PS/2, Netzwerk) einkompilieren, Rest raus
- Alle Telemetrie/Update-Checker/unnötigen Daemons deaktivieren
- **Anforderung:** Auslieferung/Installation als bootfähiges USB-Stick-Image (kein separater Installer nötig — Image per `dd` auf den Stick schreiben, Stick rein, im BIOS-Bootmenü auswählen, bootet direkt)
- **Meilenstein:** System bootet (auch tatsächlich vom USB-Stick), Idle-RAM messen (Ziel-Check: wo stehst du gegenüber 500MB?)

**Woche 3-4: JVM & Minecraft-Pfad**
- OpenJDK/Temurin für die Zielarchitektur einbinden und testen
- Minecraft-Server oder -Client startklar bringen
- Minimalen Wayland-Compositor (z.B. `cage` oder rohes `wlroots`-Setup) für Direct-Fullscreen ohne Desktop-Overhead einrichten
- **Meilenstein:** Minecraft läuft auf dem gestrippten System, Baseline-FPS gemessen

---

## Monat 2 — Gaming-Mode-Tuning

**Woche 5-6: CPU/Scheduling**
- `isolcpus` + `cset`/`taskset` für Core-Isolation (Kernel-Boot-Parameter, kein eigener Scheduler nötig)
- `sched_setaffinity` beim JVM-Start automatisiert per Wrapper-Script
- Real-Time-Priorität für JVM-Threads via `chrt` testen (vorsichtig, kann System einfrieren wenn falsch konfiguriert)
- CPU-Governor auf `performance` fixieren (P-State-Pinning ohne Kernel-Patch)

**Woche 7-8: Memory & Display**
- Transparent HugePages aktivieren und für JVM-Heap benchmarken (`-XX:+UseTransparentHugePages` bzw. `madvise`-Modus, nicht "always" — always kann Latenz-Spikes verursachen)
- Compositor-Bypass: Direct-Scanout testen (`drm` direct rendering ohne Compositor während des Spiels)
- **Meilenstein:** FPS-Vergleich vorher/nachher dokumentieren, Frametime-Konsistenz prüfen

---

## Monat 3 — Tarno-Layer (Daemon, Security, Tarno AI)

**Woche 9-10: tarnod als Userspace-Service**
- `tarnod` als privilegierter Root-Service (kein Kernel-Modul) in C++/Rust oder .NET Native AOT
- IPC via Unix-Sockets für lokale Kommunikation
- API-Key-Handling: im RAM des Daemons halten, nur über Socket-Proxy erreichbar (kein Kernel-Vault nötig, reicht als Userspace-Isolation mit korrekten Datei-Permissions)

**Woche 11: Behavioral Security ohne Kernel-Patch**
- Statt eigenem Kernel-Hook: **eBPF** nutzen (Standard-Linux-Feature) um Syscalls zu überwachen
- Bei verdächtigem Prozess: SIGSTOP via eBPF-Trigger + Userspace-Handler auslösen
- Das gibt dir 80% des "Behavioral Kernel Shield" ohne Kernel-Entwicklung

**Woche 12: Tarno AI + Polish**
- Tarno AI: ein Assistent, direkt in `tarnod` integriert (Shell-Chat-Interface, proaktives Tuning, Intelligenzschicht über der eBPF-Security). Detaillierter Tarno-AI-Plan siehe [`docs/month3-tarno-layer.md`](docs/month3-tarno-layer.md).
- FPS/Frametime-Live-Profiling-Overlay (z.B. via `mangohud` angepasst oder eigenes leichtgewichtiges Overlay)
- Aufräumen, Doku, Reproduzierbarkeit (Build-Script, damit du das System neu bauen kannst)
- Realistischer Abschlussbericht: was wurde erreicht vs. Manifest

---

## Zurückgestellt — Desktop-/GUI-Erlebnis

Der frühere GUI/Interface-Kram (`tarno-desktop`, `tarno-installer`,
`tarno-ui-theme`, `tarnod-ui`) wurde komplett aus dem Repo entfernt
(Git-Historie bleibt als Sicherheitsnetz erhalten). Grund: "Man kann nicht
direkt mit Interfaces anfangen, wenn man ein OS baut" — bevor eine GUI-Schicht
wieder aufgebaut wird, soll erst der darunterliegende Daemon/Security/AI-Kern
(`tarnod`, Monat 3) tragfähig sein.

Die alten Monat-4-Pläne (Festplatten-Installer, System-Updates +
App-Marktplatz, Terminal, Netzwerk) hingen an dieser jetzt entfernten GUI und
sind **on hold, ohne Zeitplan** — nicht gestrichen, nur zurückgestellt, bis
eine GUI-Schicht neu aufgebaut wird. Details/historischer Planungsstand:
[`docs/month4-full-os.md`](docs/month4-full-os.md).

---

## Zukunft — Debian-Basis

Langfristige Entscheidung: die Basis von Tarno OS soll perspektivisch von
Buildroot-Cross-Compile auf ein **Debian**-Fundament (z. B. `debootstrap` +
`live-build`) wechseln — bessere Paketverfügbarkeit, ausgereiftere
Werkzeugkette, weniger Cross-Compile-Eigenheiten. **Explizit: keine
Code-Änderung jetzt.** Buildroot bleibt der einzige aktuell funktionierende
Build-Weg (`tarno-br2-external/`, `.github/workflows/build-os-image.yml`).

Bevor an dieser Migration oder überhaupt am zurückgestellten Boot-Image-Ziel
weitergearbeitet wird, soll erst eine Wissensbasis entstehen ("wie baut man
ein Betriebssystem von A-Z, wie funktioniert Linux, wie ist es aufgebaut") —
künftiger Ort dafür: `docs/knowledge-base/`.

---

## Was aus dem Original-Manifest bewusst gestrichen/verschoben ist
- Eigener Kernel (Multiboot2, eigener Scheduler, Zero-Copy-Framebuffer von Grund auf) → auf unbestimmte Zeit verschoben, nicht in 3 Monaten machbar
- "0% I/O-Overhead" Security → realistisches Ziel: minimaler, messbarer Overhead statt Null
- Eigene Treiber-Pipelines → Standard-Linux-Treiber nutzen, die für die Hardware schon existieren
- GUI/Interface-Schicht (`tarno-desktop`, `tarno-installer`, `tarno-ui-theme`, `tarnod-ui`) → komplett entfernt und zurückgestellt, siehe "Zurückgestellt — Desktop-/GUI-Erlebnis" oben

## Tools/Stack im Überblick
| Bereich | Tool |
|---|---|
| Basis-OS | Alpine Linux oder Buildroot (Debian-Migration als Zukunftsziel, siehe oben) |
| Init | OpenRC / eigenes Minimal-Init |
| Compositor (Gaming) | cage (Kiosk, Direct-Fullscreen) |
| Core-Isolation | isolcpus, cset, sched_setaffinity |
| Security-Monitoring | eBPF |
| Daemon | Rust oder C++ (nativ, kein Electron) |
| Tarno AI | phasenweise, in `tarnod` integriert (`candle` für ein künftiges LLM-Backend) — Details siehe [`docs/month3-tarno-layer.md`](docs/month3-tarno-layer.md) |
| Festplatten-Installer | `tarno-disk-installer` (Rust) — sfdisk/mkfs.vfat/mkfs.ext4/rsync/extlinux, zurückgestellt, siehe [`docs/month4-full-os.md`](docs/month4-full-os.md) |
| Updates + App-Marktplatz | ein gemeinsamer `opkg`-basierter Paketmanager statt zwei getrennter Systeme, zurückgestellt |
| Terminal | `foot` (Wayland-natives Standardwerkzeug, kein Eigenbau), zurückgestellt |
| Netzwerk | `iwd` (WLAN), `bluez` (Bluetooth), zurückgestellt |
