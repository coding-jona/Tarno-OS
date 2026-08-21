# Wissensbasis — Index

Dieses Verzeichnis sammelt Recherche zu einer Frage, die noch **nicht**
umgesetzt wird: wie baut man ein Linux-basiertes Betriebssystem von Grund
auf, und was würde es konkret bedeuten, Tarno OS' Basis von
Buildroot-Cross-Compile auf ein **Debian**-Fundament umzustellen?

Kontext: [`ROADMAP.md`](../../ROADMAP.md), Abschnitt "Zukunft —
Debian-Basis". Dort steht die Entscheidung (Debian statt Buildroot,
langfristig) und die Prämisse, dass diese Wissensbasis **vor** einer
möglichen Migration entstehen soll — nicht danach, nicht parallel dazu.

**Wichtig, explizit:** Nichts in diesem Verzeichnis ist implementiert.
Kein Code in `tarnod/`, `tarno-br2-external/` oder sonst irgendwo im Repo
wurde für diese Dateien geändert. Das ist reine Referenz-/Recherche-Doku,
gedacht als Startpunkt für den Tag, an dem die Debian-Migration tatsächlich
angegangen wird — nicht früher.

## Die Dateien

1. [`01-linux-os-architecture.md`](01-linux-os-architecture.md) —
   Grundlagen: Kernel/Userspace, Boot-Prozess, Prozessmodell, FHS,
   Init-Systeme im Vergleich. Allgemeines Linux-Wissen, unabhängig von
   Buildroot oder Debian.
2. [`02-debian-base-system-building.md`](02-debian-base-system-building.md) —
   Wie man mit `debootstrap` und `live-build` ein Debian-basiertes System
   von Grund auf baut.
3. [`03-bootloader-init-kernel-basics.md`](03-bootloader-init-kernel-basics.md) —
   BIOS/UEFI, Partitionstabellen, Bootloader (SYSLINUX/GRUB), Kernel-Build,
   initramfs/initrd/live-boot — mit Bezug auf die bereits im Repo
   vorhandenen Buildroot-Artefakte.
4. [`04-tarno-os-debian-migration-notes.md`](04-tarno-os-debian-migration-notes.md) —
   Die konkrete Brücke: was von den bestehenden Buildroot-Artefakten auf
   eine künftige debootstrap/live-build-Pipeline übertragbar wäre. Auch
   hier: reine Planungsnotiz, keine Code-Änderung.

## Wie diese Dateien zueinander stehen

Datei 1 ist allgemeines Grundlagenwissen (gilt für jedes Linux-System,
auch das heutige Buildroot-basierte Tarno OS). Dateien 2 und 3 sind
Debian-/Boot-spezifisches Detailwissen, aufbauend auf Datei 1. Datei 4
ist die Synthese: sie verbindet das Wissen aus 1–3 mit dem, was im
Tarno-OS-Repo bereits existiert (siehe
[`docs/architecture.md`](../architecture.md),
[`docs/month1-foundation.md`](../month1-foundation.md),
`tarno-br2-external/`).

Reihenfolge zum Lesen: 1 → 2 → 3 → 4. Wer nur die konkrete
Migrationsfrage interessiert, kann direkt zu Datei 4 springen und bei
Bedarf in 1–3 nachschlagen.
