# tarno-devuan-live

Ein zweiter, paralleler Build-Weg für Tarno OS — die erste echte
Code-Umsetzung von ROADMAP.md, Abschnitt "Zukunft — Devuan-Basis"
(vormals "Debian-Basis", siehe unten, warum sich der Name geändert hat).

**Status: experimentell, real aber ungetestet.** `tarno-br2-external/`
(Buildroot) bleibt der einzige aktuell *funktionierende/ausgelieferte*
Build-Weg für Tarno OS. Dieses Verzeichnis ist echte, nach aktueller
`live-build`-Manual-Syntax korrekte Konfiguration — aber **niemand hat sie
je gebaut**. Kein `lb config`/`lb build`-Lauf ist über diese Dateien je
gelaufen, kein Image wurde je erzeugt.

## Warum Devuan statt Debian?

Die ursprüngliche Recherche in [`docs/knowledge-base/`](../docs/knowledge-base/)
ging von einem reinen Debian-Fundament aus. Weitere Recherche (siehe
[`docs/knowledge-base/02-debian-base-system-building.md`](../docs/knowledge-base/02-debian-base-system-building.md)
und [`docs/knowledge-base/04-tarno-os-debian-migration-notes.md`](../docs/knowledge-base/04-tarno-os-debian-migration-notes.md))
deckte einen echten Konflikt auf: Auf einem vanilla-Debian-System zieht
das `init`-Meta-Paket über seine Dependency-Alternativen standardmäßig
`systemd-sysv` (die erste Alternative in `init`s `Depends`-Zeile), sofern
man das nicht mühsam per Paket-Reihenfolge/APT-Pinning übersteuert — ein
fragiler, in dieser Sandbox nicht verifizierbarer Weg. Das widerspricht
Tarno OS' Kernprämisse "kein systemd → OpenRC" (siehe README.md,
ROADMAP.md, `docs/architecture.md`, `docs/month1-foundation.md`), die aus
echten RAM-Footprint-Gründen besteht, nicht aus Prinzip allein.

**Auflösung: Devuan statt vanilla Debian.** Devuan ist eine reale, aktiv
gepflegte Debian-Ableitung — dasselbe `apt`/`dpkg`-Ökosystem, dieselbe
`live-build`-Tooling, Mirrors unter `deb.devuan.org` statt
`deb.debian.org` — die das systemd-Default-Pull-Problem auf
Distributions-Ebene gar nicht erst hat. Devuan unterstützt offiziell
`sysvinit` (Standard) und bietet `openrc` als vollwertige Init-Alternative
an, umschaltbar über den Kernel-Boot-Parameter `init=/sbin/openrc-init`
(Quellen: <https://www.devuan.org/os/init-freedom>,
<https://laskarnix.org/devuan-migrate-from-sysv-to-openrc-init/>,
<https://dev1galaxy.org/viewtopic.php?id=7853>). Dieser
Boot-Parameter-Mechanismus ist genau das Muster, das
`tarno-br2-external/board/tarno-m6700/syslinux.cfg` heute schon nutzt
(Kernel-Cmdline-Parameter über den Bootloader) — kein Fremdkonzept,
sondern dieselbe Idee auf einer anderen Distribution.

**Das ist keine Abkehr von der Debian-Recherche** — Devuan verwendet
dasselbe Paketformat, `apt`, `dpkg`, `live-build`; nur Mirror/
Distributions-Pointer und Init-Paket-Auswahl unterscheiden sich. Die
Devuan-Version, auf die hier gepinnt wird, ist die aktuelle Stable-Release
**excalibur** (Devuan 6, Stand: Recherche August 2026) — explizit
benannt statt eines fließenden `stable`-Alias, konsistent mit der
bestehenden Praxis im Repo (Buildroot ist auf `2024.02.10` gepinnt, siehe
[`.github/workflows/build-os-image.yml`](../.github/workflows/build-os-image.yml)).

## Struktur

| Pfad | Zweck |
|---|---|
| `auto/config` | `live-build`-Wrapper-Skript (`lb config`-Aufruf mit allen Flags) — Standard-live-build-Konvention |
| `config/package-lists/tarno.list.chroot` | Zusätzliche Pakete: aktuell nur `openrc` |
| `config/includes.chroot/etc/init.d/tarnod` | OpenRC-Service-Skript für `tarnod`, portiert aus dem Buildroot-Pfad |

`config/` folgt sonst den `live-build`-Standardkonventionen — weitere
Unterverzeichnisse (`config/bootstrap`, `config/chroot` usw.) legt
`lb config` bei Bedarf selbst an, ohne die hier eingecheckten
`package-lists`/`includes.chroot`-Inhalte zu überschreiben.

## Verwendung (nicht in dieser Sandbox lauffähig, siehe unten)

```sh
cd tarno-devuan-live
sudo apt-get install live-build debootstrap squashfs-tools xorriso
sudo lb config
sudo lb build
```

Ergebnis (bei Erfolg): eine `.hybrid.iso`-Datei im selben Verzeichnis —
per `dd` auf einen USB-Stick schreibbar, genau wie
`output/images/sdcard.img` beim Buildroot-Pfad (siehe
[`docs/month1-foundation.md`](../docs/month1-foundation.md)).

## Warum das hier niemand gebaut hat

Genau dieselbe Einschränkung wie beim Buildroot-Pfad, als der noch
ungetestet war (siehe `docs/month1-foundation.md`, Abschnitt
"Scope-Hinweis für diese Sandbox"): der Netzwerk-Proxy dieser
Entwicklungs-Sandbox blockiert den Zugriff auf die echten Paket-Mirrors
(`deb.devuan.org`), die `debootstrap`/`live-build` für einen echten Build
bräuchten. Ein `lb build`-Lauf hier würde beim allerersten
`debootstrap`-Schritt fehlschlagen — kein Bug in dieser Konfiguration,
sondern dieselbe Sandbox-Grenze, die den Buildroot-Build zuvor nur in
GitHub Actions (volle Internet-Anbindung, kein Sandbox-Proxy) real zum
Laufen gebracht hat.

**Konkreter nächster Schritt zur Verifikation:** den neuen Workflow
[`.github/workflows/build-devuan-image.yml`](../.github/workflows/build-devuan-image.yml)
manuell auslösen (Actions-Tab → "Build Tarno OS Devuan image (experimental)" →
"Run workflow"). Das ist der einzige Ort, an dem diese Konfiguration
bisher überhaupt laufen könnte — und das ist noch nicht passiert.

## Was hier bewusst NICHT drin ist (Scope dieses ersten Cuts)

- **Kein `tarnod`-Binary.** `config/includes.chroot/etc/init.d/tarnod`
  referenziert `/usr/bin/tarnod`, aber nichts in diesem Verzeichnis baut
  oder embedded dieses Binary. Zwei plausible Wege für später (siehe
  [`docs/knowledge-base/04-tarno-os-debian-migration-notes.md`](../docs/knowledge-base/04-tarno-os-debian-migration-notes.md#tarno-br2-externalpackagetarnodtarnodmk-buildroot-cargo-package)):
  ein echter `cargo build --release`-Schritt plus
  `config/includes.chroot/usr/bin/tarnod`, oder ein eigenes `.deb`-Paket.
- **Kein eigener Kernel-Config-Fragment-Weg.** Anders als
  `tarno-br2-external/board/tarno-m6700/linux.config.fragment` nutzt
  dieser Track bewusst Devuans Standard-`linux-image`-Paket (Option 1 aus
  der o. g. Migrationsnotiz) statt eines eigenen Kernel-Builds — weniger
  RAM-Stripping, dafür kein Kernel-Wartungsaufwand.
- **Keine agetty-Runlevel-Links.** Reines `openrc-init` ohne zusätzliche
  `rc-update add agetty.ttyN default`-Einträge hat laut Recherche
  potenziell keinen Konsolen-Login-Prompt — in diesem ersten Cut nicht
  behoben, siehe Kommentar in `config/package-lists/tarno.list.chroot`.
- **Kein JVM/Minecraft/`cage`-Stack.** Nur Basis-Boot-Fähigkeit, keine
  Feature-Parität mit dem Buildroot-Pfad.

Diese Lücken sind Folgearbeit, kein versehentliches Weglassen.
