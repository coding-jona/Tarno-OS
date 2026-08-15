# Monat 1 — Fundament & Low-RAM-Base (Detailplan)

Übersicht siehe [`ROADMAP.md`](../ROADMAP.md#monat-1--fundament--low-ram-base). Dieses Dokument macht jeden Punkt konkret: Paketnamen, Kernel-Config-Optionen, Befehle, Abnahmekriterien.

## Woche 1-2: Basis-System

**Entscheidung: Buildroot statt Alpine.** Grund: volle Kontrolle über Kernel-Config und Init (für die spätere `tarnod`-Integration und Kernel-Strip essenziell), Buildroot bringt außerdem die `cargo-package`-Infrastruktur direkt mit (siehe unten) — Alpine bräuchte dafür ein separates APKBUILD-Setup ohne vergleichbaren Kernel-Config-Workflow.

### Buildroot-Setup
```sh
git clone https://github.com/buildroot/buildroot.git
cd buildroot
git checkout 2025.05        # letzte LTS-artige stabile Version zum Planungszeitpunkt, ggf. aktualisieren
```

### `BR2_EXTERNAL`-Tree (`tarno-br2-external/`, liegt in diesem Repo)
```sh
make BR2_EXTERNAL=/path/to/Tarno-OS/tarno-br2-external tarno_m6700_defconfig
make menuconfig      # optional, zur Kontrolle
make                 # Vollbuild: Toolchain, Kernel, Rootfs, tarnod-Package
```
Details zur Package-Struktur: siehe [`architecture.md`](architecture.md#buildroot-integration-tarno-br2-external) und die tatsächlichen Dateien unter `tarno-br2-external/`.

### Init: OpenRC statt systemd
- `BR2_INIT_OPENRC=y` in der defconfig (zieht automatisch `BR2_PACKAGE_OPENRC`).
- `tarnod` wird über `board/tarno-m6700/rootfs-overlay/etc/init.d/S60tarnod` gestartet (OpenRC-kompatibles SysV-Skript, siehe Datei im Repo).

### Kernel-Config strippen (`board/tarno-m6700/linux.config.fragment`)
Nur was das M6700 (Ivy Bridge, AHCI, PS/2, Intel-GPU, Ethernet) tatsächlich braucht:

| Bereich | Optionen |
|---|---|
| Storage | `CONFIG_ATA=y`, `CONFIG_SATA_AHCI=y`, `CONFIG_ATA_PIIX=y` |
| GPU | `CONFIG_DRM=y`, `CONFIG_DRM_I915=y` |
| Input | `CONFIG_SERIO_I8042=y`, `CONFIG_KEYBOARD_ATKBD=y`, `CONFIG_MOUSE_PS2=y` |
| Netzwerk | `CONFIG_E1000E=y` (onboard Intel NIC), `CONFIG_R8169=y` (falls verbaut) |
| eBPF (für Monat 3) | `CONFIG_BPF=y`, `CONFIG_BPF_SYSCALL=y`, `CONFIG_DEBUG_INFO_BTF=y`, optional `CONFIG_BPF_LSM=y` |
| Explizit AUS | `CONFIG_SOUND` (falls nicht gebraucht), alle nicht-M6700-Treiber (WLAN-Chipsätze anderer Hersteller, andere Storage-Controller, Bluetooth falls ungenutzt), `CONFIG_USB_*` nur PS/2-relevante behalten falls USB-Tastatur/Maus genutzt wird |

Fragment wird via `BR2_LINUX_KERNEL_USE_CUSTOM_CONFIG=y` + `BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILES` eingebunden (siehe `configs/tarno_m6700_defconfig`).

### Telemetrie/unnötige Daemons deaktivieren
- Keine `BR2_PACKAGE_*` für: `chrony`-NTP-Auto-Update-Checker (nur falls gebraucht, sonst raus), `dbus` nur falls ein Paket es zwingend braucht, keine Log-Rotation-Daemons über das Minimum hinaus.
- Busybox statt vollwertiger Coreutils/systemd-Tools (`BR2_PACKAGE_BUSYBOX=y`, Standard in Buildroot).

**Meilenstein-Abnahmekriterium:** System bootet bis zum Login-Prompt, `free -m` zeigt Idle-RSS. Zielkorridor: deutlich unter 500 MB (Referenzwert aus dem Manifest); konkreter Zahlenwert wird nach dem ersten realen Boot auf dem M6700 in dieses Dokument nachgetragen (kann in dieser Sandbox nicht gemessen werden, da kein Boot-Test möglich ist — siehe Scope-Hinweis unten).

## Woche 3-4: JVM & Minecraft-Pfad

### JVM
- `BR2_PACKAGE_...`: Buildroot hat kein offizielles OpenJDK-Package für alle Targets — Standardweg ist, ein vorgebautes **Temurin/Adoptium-JDK-Tarball** (musl- oder glibc-Build passend zur gewählten C-Bibliothek) per eigenem `generic-package` in den `tarno-br2-external`-Tree einzubinden (`package/temurin-jdk/temurin-jdk.mk` mit `TEMURIN_JDK_SITE`, `TEMURIN_JDK_SOURCE` auf die passende Release-Tarball-URL, `_INSTALL_TARGET_CMDS` kopiert nach `/opt/jdk`).
- Falls `glibc` statt `musl` als C-Bibliothek gewählt wird (`BR2_TOOLCHAIN_BUILDROOT_GLIBC=y`), sind die offiziellen Temurin-Linux-x64-Tarballs direkt kompatibel — das ist der pragmatischste Weg für Monat 1, da musl-JVM-Support (z. B. über Alpine-eigene Builds) zusätzliche Kompatibilitätsarbeit bedeutet.

### Minecraft
- Server: reines JAR, läuft mit jeder kompatiblen JVM — kein Zusatzaufwand.
- Client: benötigt LWJGL (native `.so`-Bibliotheken für GL/ALSA) — diese müssen zur Ziel-libc passen (glibc empfohlen, s.o.).

### Compositor: `cage`
- `cage` ist ein Kiosk-Wayland-Compositor, der genau eine Anwendung fullscreen startet — passt exakt zu "Direct-Fullscreen ohne Desktop-Overhead".
- Buildroot-Package `BR2_PACKAGE_CAGE` (sofern in der gewählten Buildroot-Version vorhanden; sonst als eigenes Package im `tarno-br2-external`-Tree ergänzen, analog zum `tarnod`-Package-Pattern).
- Start: `cage -- /opt/jdk/bin/java -jar minecraft-launcher.jar` (bzw. über `scripts/jvm-launch.sh`, siehe [`month2-gaming-tuning.md`](month2-gaming-tuning.md)).

**Meilenstein-Abnahmekriterium:** `java -version` liefert die erwartete Version, Minecraft startet in `cage` fullscreen, Baseline-FPS wird mit `scripts/benchmark.sh` (siehe Monat 2) protokolliert.

## Scope-Hinweis für diese Sandbox

Diese Sandbox kann **keinen echten Buildroot-Build durchführen und keinen Boot-Test auf dem M6700** — das würde mehrere GB an Downloads und mehrere Stunden Bauzeit benötigen, plus reale Zielhardware zum Booten. Die in diesem Repo erzeugten Buildroot-Artefakte (`tarno-br2-external/`) sind syntaktisch korrekt nach aktuellem Buildroot-Manual, aber **ungetestet gegen einen echten Buildroot-Checkout**. Erster praktischer Schritt für den Nutzer: die obigen Befehle auf einer echten Build-Maschine ausführen und Ergebnisse (RAM-Wert, Boot-Fehler) zurückmelden — darauf aufbauend werden die Configs iteriert.
