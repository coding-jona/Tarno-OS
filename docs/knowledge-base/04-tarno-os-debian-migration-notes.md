# Tarno OS → Debian: Migrationsnotizen (Brücke)

Siehe [`00-index.md`](00-index.md) für Zweck/Status. **Explizit
nochmal, weil dies das Dokument mit dem größten Missverständnis-Risiko
ist: keine Zeile Code wurde für dieses Dokument geändert.** Es ist reine
Vorbereitung für einen späteren Zeitpunkt, an dem die in
[`ROADMAP.md`](../../ROADMAP.md) ("Zukunft — Debian-Basis") beschriebene
Migration tatsächlich angegangen wird. Buildroot bleibt bis dahin der
einzige funktionierende Build-Weg.

Ziel dieses Dokuments: für jedes bestehende Buildroot-Artefakt in
`tarno-br2-external/` konkret benennen, was davon auf eine künftige
`debootstrap`/`live-build`-Pipeline übertragbar ist, was ersetzt werden
müsste, und was ganz wegfällt.

## `board/tarno-m6700/linux.config.fragment`

**Übertragbar, mit Anpassung.** Die fachliche Aussage — welche
Kernel-Features die Zielhardware (M6700: AHCI, i915, PS/2, E1000E/R8169,
eBPF) braucht — bleibt exakt gleich, unabhängig von Buildroot oder
Debian. Zwei Wege, das auf Debian anzuwenden:

1. **Einfacher, empfohlener Startpunkt:** Debians Standard-`linux-image`-
   Paket verwenden (deckt alle hier gelisteten Treiber bereits ab, meist
   als Module `=m` statt `=y`) und auf eigene Kernel-Kompilierung ganz
   verzichten. Verliert die "nur was nötig ist"-Stripping-Eigenschaft
   (mehr RAM/Disk durch generischen Kernel), gewinnt aber Wartungsfreiheit
   (Security-Updates über `apt` statt eigenem Kernel-Rebuild).
2. **Wie bisher, eigener Kernel-Build:** das Fragment als Basis für eine
   eigene `.config` in einem manuellen Kernel-Build innerhalb des
   debootstrap-chroots weiterverwenden (`make menuconfig` + `make
   bindeb-pkg` erzeugt ein `.deb`-Kernel-Paket, das sich sauber per `apt`/
   `dpkg` in das Zielsystem installieren lässt). Näher am heutigen
   Stripping-Ansatz, aber mehr Aufwand als Option 1.

## `board/tarno-m6700/genimage.cfg` + `syslinux.cfg`

**Teilweise übertragbar, eher als Referenz als als 1:1-Wiederverwendung.**
Das MBR/SYSLINUX-BIOS-Boot-Muster (vfat-Boot-Partition + `syslinux.cfg`)
ist konzeptionell identisch zu dem, was `live-build`/`lb config` für ein
BIOS-Ziel ohnehin automatisch erzeugt (siehe
[`02-debian-base-system-building.md`](02-debian-base-system-building.md)) —
`live-build` bringt seine eigene Bootloader-Integration mit
(`isolinux`/`syslinux` für BIOS), das händische `genimage.cfg`-Layout
würde dadurch großteils überflüssig. Die Boot-Parameter-Logik
(`root=/dev/sda2 rootwait ro console=...`) bliebe konzeptionell bestehen,
müsste aber an `live-boot`s andere Root-Mount-Mechanik (squashfs +
overlayfs statt direktem `root=`-Mount, siehe
[`03-bootloader-init-kernel-basics.md`](03-bootloader-init-kernel-basics.md))
angepasst werden — ein Live-Image mountet grundsätzlich anders als das
heutige direkte Root-FS-Layout.

## `board/tarno-m6700/post-image.sh`

**Nicht direkt übertragbar** — das ist reine Buildroot-Post-Image-Hook-
Mechanik (`BR2_ROOTFS_POST_IMAGE_SCRIPT`), die es unter `live-build` in
dieser Form nicht gibt. Das fachliche Ziel dahinter (MBR-Bootstrap-Code
korrekt ins finale Image schreiben) übernimmt `live-build`/`lb build`
selbst als Teil seines Boot-Setups. Der offene TODO-Punkt aus
`genimage.cfg` (ob genimage den `dd`-Schritt mittlerweile selbst
abdeckt) wird durch eine Debian-Migration ohnehin gegenstandslos, weil
das ganze Skript wegfiele.

## `tarno-br2-external/package/tarnod/tarnod.mk` (Buildroot-Cargo-Package)

**Konzept übertragbar, Mechanik komplett anders.** Die fachliche
Anforderung — `tarnod`/`tarnoctl` als Rust-Binaries ins Zielsystem
bringen, mit dem vorkompilierten eBPF-Objekt eingebettet — bliebe
identisch. Unter Debian/debootstrap gäbe es dafür zwei plausible Wege:

1. Innerhalb des chroots direkt `cargo build --release` laufen lassen
   (setzt eine Rust-Toolchain im chroot voraus, z. B. über `rustup` oder
   Debians `rustc`/`cargo`-Pakete) und die fertigen Binaries manuell an
   die passenden FHS-Pfade kopieren (`/usr/bin/`, siehe
   [`01-linux-os-architecture.md`](01-linux-os-architecture.md)).
2. Ein eigenes `.deb`-Paket für `tarnod`/`tarnoctl` bauen (`cargo-deb`
   oder ein manuelles `debian/`-Verzeichnis mit `dpkg-buildpackage`) und
   das über eine lokale APT-Repository-Struktur oder direkt per
   `dpkg -i` im chroot installieren. Sauberer (Versionsverwaltung,
   Deinstallierbarkeit), aber mehr initialer Aufwand als Option 1.

Der bewusste Grund, warum das eBPF-Objekt heute vorkompiliert eingebettet
wird (kein Host-BPF-Toolchain-Bootstrapping im Cross-Build, siehe
Kommentar in `tarnod.mk`), entfällt bei einer Debian-Migration übrigens
teilweise: ein debootstrap-chroot läuft nativ auf derselben Architektur
(kein Cross-Compile), eine vollständige BPF-Toolchain (nightly Rust +
`bpf-linker`) direkt im chroot zu installieren wäre also grundsätzlich
möglich — ob das die vorkompilierte Variante ablöst, ist eine spätere
Abwägung, kein Automatismus.

## Init: OpenRC bleibt eine gültige Option

Wie in [`01-linux-os-architecture.md`](01-linux-os-architecture.md)
beschrieben: Debian nutzt standardmäßig systemd, aber OpenRC ist
offiziell paketiert. Der heutige `S60tarnod`-OpenRC-Service
(`board/tarno-m6700/rootfs-overlay/etc/init.d/S60tarnod`) müsste bei
einer Migration nicht neu konzipiert werden — nur der Paketierungsweg
ändert sich (Datei landet im debootstrap-chroot statt im Buildroot-
Rootfs-Overlay).

## Zusammenfassung: Übertragbarkeits-Tabelle

| Artefakt | Übertragbarkeit |
|---|---|
| `linux.config.fragment` (fachliche Treiberliste) | hoch — gleiche Hardware-Anforderungen, anderer Verpackungsweg |
| `genimage.cfg` / `syslinux.cfg` (Boot-Layout-Konzept) | mittel — Konzept ähnlich, Mechanik (`live-build`) anders |
| `post-image.sh` (Buildroot-Hook-Mechanik) | niedrig — Buildroot-spezifisch, entfällt |
| `tarnod.mk` (fachliche Anforderung: Rust-Binaries + eBPF-Objekt einbetten) | hoch als Anforderung, niedrig als Mechanik (Cargo-Package-Infra vs. `.deb`/chroot-Build) |
| OpenRC als Init | hoch — offiziell auf Debian paketiert, keine Neukonzeption nötig |

## Quellen

Siehe [`01-linux-os-architecture.md`](01-linux-os-architecture.md),
[`02-debian-base-system-building.md`](02-debian-base-system-building.md)
und [`03-bootloader-init-kernel-basics.md`](03-bootloader-init-kernel-basics.md)
für die zugrundeliegenden Primärquellen. Repo-interne Referenzen:
[`docs/architecture.md`](../architecture.md),
[`docs/month1-foundation.md`](../month1-foundation.md),
`tarno-br2-external/`.
