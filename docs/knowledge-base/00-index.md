# Wissensbasis — Index

Dieses Verzeichnis sammelt Recherche zu Fragen, die noch **nicht**
umgesetzt sind — zwei getrennte Themenstränge, beide nach demselben
Prinzip: erst verstehen/recherchieren, dann (später, separat) bauen.

**Wichtig, explizit:** Nichts in diesem Verzeichnis ist implementiert.
Kein Code in `tarnod/`, `tarno-br2-external/` oder sonst irgendwo im Repo
wurde für diese Dateien geändert. Reine Referenz-/Recherche-Doku.

## Strang A — Debian-Basis (Dateien 1-4)

Wie baut man ein Linux-basiertes Betriebssystem von Grund auf, und was
würde es konkret bedeuten, Tarno OS' Basis von Buildroot-Cross-Compile auf
ein **Debian**-Fundament umzustellen?

Kontext: [`ROADMAP.md`](../../ROADMAP.md), Abschnitt "Zukunft —
Debian-Basis". Dort steht die Entscheidung (Debian statt Buildroot,
langfristig) und die Prämisse, dass diese Wissensbasis **vor** einer
möglichen Migration entstehen soll — nicht danach, nicht parallel dazu.

## Strang B — Tarno-AI-Phase-2 (Datei 5)

Wie bindet man Mistral-AI-Cloud-Modelle über deren REST-API in `tarnod`
ein (API-Key-Auth, Chat-Completions-Schema, Rust-Anbindung)?

Kontext: [`month3-tarno-layer.md`](../month3-tarno-layer.md#tarno-ai),
Abschnitt "Phase 2" — Tarno AI läuft **nicht** auf einem lokalen Modell,
sondern auf Mistral-AI-Modellen über einen API-Key (gleiches Vault-Prinzip
wie andere API-Keys). Diese Recherche entstand bewusst, bevor an der
Phase-2-Implementierung Code geschrieben wird.

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
5. [`05-mistral-ai-api-integration.md`](05-mistral-ai-api-integration.md) —
   Mistral-API-Grundlagen (Auth, Endpoint, Request/Response-Schema),
   Modelle/Kosten, Rust-Anbindungsoptionen, und wie das an die bestehende
   `Vault` und den `AiBackend`-Trait aus Tarno-AI-Phase-1 andockt.

## Wie diese Dateien zueinander stehen

Strang A (1-4): Datei 1 ist allgemeines Grundlagenwissen (gilt für jedes
Linux-System, auch das heutige Buildroot-basierte Tarno OS). Dateien 2 und
3 sind Debian-/Boot-spezifisches Detailwissen, aufbauend auf Datei 1.
Datei 4 ist die Synthese: sie verbindet das Wissen aus 1–3 mit dem, was im
Tarno-OS-Repo bereits existiert (siehe
[`docs/architecture.md`](../architecture.md),
[`docs/month1-foundation.md`](../month1-foundation.md),
`tarno-br2-external/`).

Reihenfolge zum Lesen: 1 → 2 → 3 → 4. Wer nur die konkrete
Migrationsfrage interessiert, kann direkt zu Datei 4 springen und bei
Bedarf in 1–3 nachschlagen.

Strang B (5): eigenständig, unabhängig von Strang A lesbar — betrifft
Tarno AI, nicht das Basis-Betriebssystem.
