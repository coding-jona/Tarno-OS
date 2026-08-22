# Tarno OS → Devuan: Migrationsnotizen (Brücke)

Siehe [`00-index.md`](00-index.md) für Zweck/Status. **Explizit
nochmal, weil dies das Dokument mit dem größten Missverständnis-Risiko
ist: der überwiegende Teil dieses Dokuments (Kernel, Boot-Layout,
tarnod-Paketierung) blieb reine Planungsnotiz, keine Code-Änderung.**
Ausnahme: der Init-Abschnitt unten hat inzwischen einen ersten, echten
(aber ungetesteten) Code-Niederschlag in
[`../../tarno-devuan-live/`](../../tarno-devuan-live/) — siehe dessen
README für Status/Scope. Der Rest bleibt Vorbereitung für einen späteren
Zeitpunkt, an dem die in [`ROADMAP.md`](../../ROADMAP.md) ("Zukunft —
Devuan-Basis", vormals "Debian-Basis" — siehe Init-Abschnitt unten, warum
sich das Ziel korrigiert hat) beschriebene Vollmigration tatsächlich
angegangen wird. Buildroot bleibt bis dahin der einzige funktionierende,
ausgelieferte Build-Weg.

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

## Init: OpenRC bleibt eine gültige Option — aber nicht auf vanilla Debian

**Kurskorrektur.** Die ursprüngliche Fassung dieses Abschnitts behauptete
sinngemäß "OpenRC ist offiziell auf Debian paketiert, keine Neukonzeption
nötig" — das ist eine Vereinfachung, die sich bei genauerer Recherche als
zu optimistisch herausstellte. Wie in
[`01-linux-os-architecture.md`](01-linux-os-architecture.md) beschrieben,
nutzt Debian standardmäßig systemd, und `apt install openrc` installiert
zwar das OpenRC-Paket — löst aber **nicht** das eigentliche Problem: das
`init`-Metapaket selbst löst seine Dependency-Alternativen
(`systemd-sysv | sysvinit-core | ...`) standardmäßig zur ersten
Alternative auf, also `systemd-sysv`, sowohl bei `debootstrap` als auch
bei `live-build`. Um das zu übersteuern, bräuchte man fragiles
APT-Pinning oder eine sorgfältig kontrollierte Installationsreihenfolge —
unverifiziert in dieser Sandbox und in der Praxis leicht durch einen
späteren `apt`-Lauf wieder umkippbar. Details/Mechanismus:
[`02-debian-base-system-building.md`](02-debian-base-system-building.md#das-systemd-default-problem-auf-vanilla-debian),
Quelle:
<https://www.notinventedhere.org/articles/linux/debootstrapping-debian-jessie-without-systemd.html>.

**Tatsächliche Auflösung: Devuan statt vanilla Debian.** Devuan ist eine
reale, aktiv gepflegte Debian-Ableitung — identisches
`apt`/`dpkg`/`live-build`-Ökosystem (nur Mirror `deb.devuan.org` statt
`deb.debian.org`), deren `init`-Metapaket standardmäßig zu
`sysvinit-core` auflöst, nicht zu `systemd-sysv`. Devuan bietet zusätzlich
`openrc` als vollwertige, offiziell unterstützte Init-Alternative an,
aktivierbar über den Kernel-Boot-Parameter `init=/sbin/openrc-init` —
konzeptionell identisch zu dem Boot-Parameter-Muster, das
`tarno-br2-external/board/tarno-m6700/syslinux.cfg` bereits heute nutzt
(`root=/dev/sda2` als Kernel-Cmdline-Parameter über den Bootloader). Der
heutige `tarnod`-OpenRC-Service
(`tarno-br2-external/board/tarno-m6700/rootfs-overlay/etc/init.d/tarnod`,
Service-Name `tarnod` — nicht `S60tarnod`, wie eine ältere Fassung dieses
Dokuments fälschlich annahm; OpenRC nutzt symlink-basierte Runlevel statt
`SNN`-Präfixe im Dateinamen selbst) musste dafür tatsächlich **nicht neu
konzipiert werden** — die ursprüngliche Kernaussage stimmt im Ergebnis,
nur der Weg dorthin (Devuan statt vanilla Debian) ist ein anderer als
ursprünglich angenommen. Das Service-Skript ist 1:1 nach
[`../../tarno-devuan-live/config/includes.chroot/etc/init.d/tarnod`](../../tarno-devuan-live/config/includes.chroot/etc/init.d/tarnod)
portiert (echter, aber ungetesteter Code — siehe
[`../../tarno-devuan-live/README.md`](../../tarno-devuan-live/README.md)).

Quellen:
- <https://www.devuan.org/os/init-freedom>
- <https://laskarnix.org/devuan-migrate-from-sysv-to-openrc-init/>
- <https://dev1galaxy.org/viewtopic.php?id=7853>

## Zusammenfassung: Übertragbarkeits-Tabelle

| Artefakt | Übertragbarkeit |
|---|---|
| `linux.config.fragment` (fachliche Treiberliste) | hoch — gleiche Hardware-Anforderungen, anderer Verpackungsweg |
| `genimage.cfg` / `syslinux.cfg` (Boot-Layout-Konzept) | mittel — Konzept ähnlich, Mechanik (`live-build`) anders |
| `post-image.sh` (Buildroot-Hook-Mechanik) | niedrig — Buildroot-spezifisch, entfällt |
| `tarnod.mk` (fachliche Anforderung: Rust-Binaries + eBPF-Objekt einbetten) | hoch als Anforderung, niedrig als Mechanik (Cargo-Package-Infra vs. `.deb`/chroot-Build) |
| OpenRC als Init | hoch als Service-Skript (1:1 nach `tarno-devuan-live/` portiert), aber **nicht auf vanilla Debian** — vanilla Debians `init`-Metapaket löst standardmäßig zu `systemd-sysv` auf; Auflösung: Ziel-Distribution auf Devuan geändert (`init=/sbin/openrc-init`-Boot-Parameter), siehe Abschnitt "Init" oben |

## Quellen

Siehe [`01-linux-os-architecture.md`](01-linux-os-architecture.md),
[`02-debian-base-system-building.md`](02-debian-base-system-building.md)
und [`03-bootloader-init-kernel-basics.md`](03-bootloader-init-kernel-basics.md)
für die zugrundeliegenden Primärquellen. Repo-interne Referenzen:
[`docs/architecture.md`](../architecture.md),
[`docs/month1-foundation.md`](../month1-foundation.md),
`tarno-br2-external/`, [`../../tarno-devuan-live/`](../../tarno-devuan-live/)
(erster echter, ungetesteter Code-Niederschlag des Init-Abschnitts oben).
