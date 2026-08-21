# Bootloader, Init & Kernel — Grundlagen

Siehe [`00-index.md`](00-index.md) für Zweck/Status: reine Recherche,
keine Code-Änderung. Quellen: LFS-Buch (Bootloader-/Kernel-Kapitel) und
das Debian Administrator's Handbook (Kernel-Kompilierungs-Kapitel) —
Links am Ende.

## BIOS vs. UEFI

- **BIOS** (Legacy): Firmware lädt den ersten Sektor eines Bootmediums
  (den **MBR**, 512 Byte, davon 440 Byte Bootstrap-Code) und führt ihn
  aus. Dieser Code muss selbst wissen, wie er den eigentlichen Bootloader
  nachlädt (z. B. SYSLINUX' `mbr.bin`).
- **UEFI**: Firmware selbst versteht ein Dateisystem (FAT auf einer
  **EFI System Partition**, ESP) und lädt daraus direkt eine
  `.efi`-Programmdatei — kein 440-Byte-Bootstrap-Code-Limit, keine
  MBR-Abhängigkeit. UEFI unterstützt zusätzlich Secure Boot (signierte
  Bootloader).

Tarno OS bootet heute klassisch per BIOS/SYSLINUX (siehe
`tarno-br2-external/board/tarno-m6700/syslinux.cfg` und `genimage.cfg`) —
passend zur Zielhardware (Dell Precision M6700, Ivy Bridge, unterstützt
zwar UEFI, aber BIOS-Boot ist für ein einzelnes bekanntes Zielgerät
einfacher und robuster als UEFI-Secure-Boot-Fallstricke zu behandeln).

## MBR vs. GPT

- **MBR** (Master Boot Record): alte Partitionstabelle, max. 4 primäre
  Partitionen, max. 2 TiB Plattengröße, liegt im ersten Sektor.
- **GPT** (GUID Partition Table): moderne Partitionstabelle, praktisch
  beliebig viele Partitionen, keine 2-TiB-Grenze, redundante Kopie am
  Ende der Platte. UEFI setzt in der Praxis GPT voraus (auch wenn technisch
  UEFI+MBR möglich ist); BIOS-Boot funktioniert mit beiden.

Das bestehende `genimage.cfg` von Tarno OS nutzt ein klassisches
`hdimage`-Layout mit MBR-Partitionstyp-Bytes (`0xC` für die vfat-Boot-
Partition, `0x83` für ext4-Rootfs) — konsistent mit dem BIOS/SYSLINUX-Weg.

## Bootloader: SYSLINUX/extlinux vs. GRUB

- **SYSLINUX** (und seine Varianten `extlinux` für ext-Dateisysteme,
  `isolinux` für ISO-Images): schlank, einfache Textkonfiguration
  (`syslinux.cfg`), BIOS-fokussiert, kein eigenes Skriptsystem. Genau das
  Muster, das Tarno OS heute nutzt: `board/tarno-m6700/syslinux.cfg`
  definiert einen einzigen Boot-Eintrag (`LABEL tarno-os`) mit
  `root=/dev/sda2`-Kernel-Parameter; `post-image.sh` schreibt zusätzlich
  den MBR-Bootstrap-Code (`mbr.bin`) per `dd` in das fertige Image (siehe
  Kommentar dort zum offenen Verifikations-TODO).
- **GRUB** (GRUB2): deutlich mächtiger — versteht selbst Dateisysteme,
  unterstützt BIOS **und** UEFI, hat eine eigene Konfigurationssprache
  und interaktive Shell, wird von den meisten Desktop-Distributionen
  (inkl. Debian standardmäßig) genutzt. `live-build` (siehe
  [`02-debian-base-system-building.md`](02-debian-base-system-building.md))
  nutzt standardmäßig eine Kombination aus `isolinux` (BIOS) und
  `grub-efi` (UEFI) für Live-Images, je nach Zielmedium.

Für eine reine BIOS-Zielhardware wie den M6700 ist SYSLINUX/extlinux die
einfachere Wahl (kein Dateisystem-Treiber-Overhead im Bootloader selbst);
GRUB würde erst relevant, falls UEFI-Boot oder mehrere Kernel-Versionen
parallel unterstützt werden sollen.

## Kernel-Konfiguration & Kompilierung

Der Linux-Kernel wird über eine `.config`-Datei konfiguriert (Tausende
`CONFIG_*`-Optionen, meist `y`/`m`/`n` bzw. Zahlen/Strings) — Werkzeuge wie
`make menuconfig` bieten dafür ein interaktives Menü. Für ein Zielsystem
mit bekannter, fester Hardware ist der übliche Weg: mit einer sinnvollen
Basis-Config starten (z. B. `make defconfig` oder die Distributions-Config
des Zielsystems) und dann gezielt Optionen für die konkrete Hardware
aktivieren/deaktivieren — genau das Muster, das Tarno OS bereits mit
`board/tarno-m6700/linux.config.fragment` fährt (AHCI, i915, PS/2, eBPF
gezielt an, alles andere implizit aus der Basis-Config).

Zwei Kompilierungswege:

- **Eingebaut** (`=y`): Treiber/Feature ist fest Teil des Kernel-Images,
  immer verfügbar, kein Modul-Nachladen nötig — reduziert Boot-Komplexität,
  vergrößert aber das Kernel-Image.
- **Modul** (`=m`): wird als separate `.ko`-Datei gebaut, zur Laufzeit per
  `modprobe`/`insmod` nachgeladen — kleineres Kernel-Image, aber die
  initiale Root-Filesystem-Erkennung (Storage-Treiber!) muss entweder
  eingebaut sein oder über eine initramfs bereitgestellt werden, die genau
  dieses Modul schon enthält (klassisches Henne-Ei-Problem, das initramfs
  ursprünglich löst).

Tarno OS' `linux.config.fragment` setzt sicherheitsrelevante/boot-kritische
Treiber (AHCI/SATA, PS/2-Eingabe) bewusst als `=y` fest ein, nicht als
Modul — vermeidet genau dieses initramfs-Henne-Ei-Problem, siehe unten.

## initramfs vs. initrd vs. `live-boot`

- **initrd** (älter): ein Block-Device-Image (Dateisystem-Image), das der
  Kernel vor dem eigentlichen Root-Filesystem mountet.
- **initramfs** (heute Standard): ein cpio-Archiv, das direkt in den
  Kernel-Speicher entpackt wird (kein Block-Device-Mount nötig) — enthält
  ein minimales Userspace (oft `busybox`-basiert) mit dem Auftrag, die
  nötigen Treiber/Module zu laden und dann per `switch_root` auf das
  eigentliche Root-Dateisystem umzuschalten.
- **`live-boot`** (siehe
  [`02-debian-base-system-building.md`](02-debian-base-system-building.md)):
  ist selbst eine Sammlung von initramfs-Hooks — nutzt denselben
  initramfs-Mechanismus, aber mit der zusätzlichen Logik "finde das
  Live-Boot-Medium, mounte das komprimierte squashfs-Root-Image von dort,
  lege ein overlayfs für Schreibzugriffe darüber, dann `switch_root`".

Tarno OS braucht heute **keine** initramfs: alle boot-kritischen Treiber
(AHCI, PS/2) sind fest eingebaut (`=y`), das Root-Dateisystem liegt direkt
auf der zweiten Partition des Images und wird per Kernel-Parameter
(`root=/dev/sda2`) referenziert — der Kernel kann das Root-FS also ohne
Zwischenschritt selbst mounten. Ein künftiges Debian-Live-Image (siehe
[`04-tarno-os-debian-migration-notes.md`](04-tarno-os-debian-migration-notes.md))
bräuchte dagegen zwingend eine initramfs mit `live-boot`, weil das
Root-Dateisystem dort komprimiert vom Boot-Medium kommt statt direkt
gemountet zu werden.

## Quellen

- Linux From Scratch, Version 12.1 (Bootloader- und Kernel-Kapitel): <https://www.linuxfromscratch.org/lfs/downloads/12.1/LFS-BOOK-12.1.pdf>
- The Debian Administrator's Handbook (Kernel-Kompilierungs-Kapitel): <https://debian-handbook.info/> (PDF: <https://debian-handbook.info/download/buster/debian-handbook.pdf>)
