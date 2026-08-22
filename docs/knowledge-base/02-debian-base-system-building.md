# Debian-Basissystem bauen — `debootstrap` & `live-build`

Siehe [`00-index.md`](00-index.md) für Zweck/Status: reine Recherche,
keine Code-Änderung.

## `debootstrap`

`debootstrap` installiert ein minimales Debian-Basissystem in ein
beliebiges Zielverzeichnis, ohne dass ein Paketmanager-Setup, ein
laufendes Debian-System als Host oder ein Installer-ISO nötig wäre —
direkt aus den offiziellen Debian-Repositories per HTTP.

Grundform:

```sh
debootstrap --arch=amd64 --variant=minbase bookworm /pfad/zum/zielverzeichnis http://deb.debian.org/debian
```

Wichtige Punkte:

- **`--variant=minbase`**: installiert nur das absolute Minimum
  (`Essential: yes`-Pakete + `apt` selbst), nicht das größere
  `build-essential`-artige Standard-Set. Das ist der direkte Analog-Ansatz
  zu Buildroots "nur einkompilieren, was wirklich gebraucht wird"
  (vgl. Kernel-Config-Stripping in
  [`docs/month1-foundation.md`](../month1-foundation.md)) — Ziel: möglichst
  kleiner, auditierbarer Ausgangszustand statt eines vollen
  Standard-Systems.
- Das Ergebnis ist ein **chroot-fähiges** Verzeichnis: man kann direkt mit
  `chroot /pfad/zum/zielverzeichnis /bin/bash` hineinwechseln und von dort
  aus mit `apt` weitere Pakete installieren, Konfiguration anpassen usw.
- `debootstrap` selbst trifft keine Entscheidung über Bootloader, Kernel
  oder Init — das bleibt der aufrufenden Pipeline überlassen (typischerweise
  `live-build`, siehe unten, oder ein manuelles Skript).

## `live-build` (Debian Live Manual)

`live-build` ist Debians offizielles Werkzeug, um aus einem
`debootstrap`-artigen Basissystem ein bootfähiges Live-Image (ISO oder
USB-Image) zu bauen — inklusive Kernel-Auswahl, Bootloader-Integration
und optionalem "persistence"-Layer. Es kapselt im Wesentlichen dieselben
Schritte, die man auch von Hand mit `debootstrap` + Bootloader-Setup +
`squashfs`/`live-boot` durchführen würde, als wiederholbare,
konfigurierbare Pipeline.

Typischer Ablauf (aus dem offiziellen Debian Live Manual, Beispiel-Kapitel):

```sh
lb config
lb build
```

`lb config` erzeugt ein `config/`-Verzeichnis mit Unterordnern für
Paketlisten, Bootloader-Optionen, Hooks (Skripte, die während des Builds
im chroot laufen — vergleichbar mit Buildroots Post-Build/Post-Image-Hooks,
siehe `tarno-br2-external/board/tarno-m6700/post-image.sh`), `lb build`
führt den kompletten Build aus (intern: `debootstrap` → Paketinstallation
→ Kernel/Bootloader-Einrichtung → `squashfs`-Kompression des Root-FS →
finales ISO/IMG).

## `live-boot`

`live-boot` ist das Gegenstück zur Build-Zeit auf der **Boot-Zeit**-Seite:
ein Satz initramfs-Hooks/Skripte, die beim Systemstart erkennen, dass es
sich um ein Live-Medium handelt, das komprimierte `squashfs`-Root-Dateisystem
vom Boot-Medium (USB-Stick, ISO) einbinden (typischerweise mit einem
`overlayfs` darüber für Schreibzugriffe zur Laufzeit, die beim nächsten
Boot standardmäßig wieder verworfen werden — es sei denn, "persistence"
ist explizit konfiguriert) und erst dann das eigentliche Root-Dateisystem
wechseln (`switch_root`). Das ist ein grundsätzlich anderer Mechanismus
als ein klassisches initramfs/initrd (siehe
[`03-bootloader-init-kernel-basics.md`](03-bootloader-init-kernel-basics.md)),
das direkt auf ein beschreibbares Root-Dateisystem umschaltet.

## Init-Wahl auf einer debootstrap-Basis

Ein frisches `debootstrap`-System hat noch **kein** Init-System
vorkonfiguriert im Sinne einer festen Entscheidung — `apt install`
zieht standardmäßig `systemd` als Debians Default-Init, aber `sysvinit`
und `openrc` sind offizielle, paketierte Alternativen
(`apt install openrc`, siehe Debian-Wiki). Der Wechsel des Default-Inits
auf einem Debian-System läuft über `update-alternatives`/das Paket
`orphan-sysvinit-scripts` bzw. bei OpenRC über das `openrc`-Paket selbst,
das den entsprechenden `init`-Symlink setzt.

**Das obige ist die vereinfachte Darstellung — der nächste Abschnitt
korrigiert sie.** In der Praxis läuft der Init-Wechsel auf Debian nicht so
sauber ab, wie "offizielle Alternative" suggeriert.

## Das systemd-Default-Problem auf vanilla Debian

Kurskorrektur gegenüber der ursprünglichen Fassung dieses Dokuments (siehe
auch [`00-index.md`](00-index.md), Abschnitt "Strang A", und
[`04-tarno-os-debian-migration-notes.md`](04-tarno-os-debian-migration-notes.md),
Abschnitt "Init"): Das `init`-Metapaket in Debian ist nicht neutral. Sein
`Depends`-Feld listet eine Alternativen-Kette (`systemd-sysv |
sysvinit-core | ...`), und sowohl `debootstrap` als auch `live-build`
lösen eine solche Alternativen-Kette standardmäßig zur **ersten**
Alternative auf — das ist `systemd-sysv`. Um stattdessen `sysvinit-core`
oder `openrc` als tatsächliches PID-1 zu bekommen, reicht `apt install
openrc` also **nicht**: das installiert zwar die OpenRC-Pakete, ändert
aber nicht automatisch, was beim nächsten `apt upgrade`/einer
Neuinstallation von `init` als Alternative gewinnt. Der korrekte,
aber fragile Weg auf vanilla Debian wäre, das `init`-Paket gezielt vor
`systemd-sysv` zu installieren bzw. über APT-Pinning zu erzwingen, welche
Alternative gewinnt — ein Mechanismus, der in dieser Sandbox nicht gegen
einen echten Debian-Mirror verifizierbar ist und der in der Praxis leicht
durch einen späteren, scheinbar harmlosen `apt`-Lauf (der `init` neu
auflöst) wieder umgekippt werden kann.

Das widerspricht Tarno OS' Kernprämisse "kein systemd → OpenRC" (siehe
README.md, ROADMAP.md, `docs/architecture.md`,
`docs/month1-foundation.md`) — die aus echten RAM-Footprint-Gründen
besteht (systemd zieht typischerweise zusätzliche Daemons/Overhead nach
sich), nicht aus Prinzip allein.

Quelle für den Mechanismus:
<https://www.notinventedhere.org/articles/linux/debootstrapping-debian-jessie-without-systemd.html>
(beschreibt genau dieses Alternativen-Auflösungsproblem beim
`debootstrap`-Bootstrapping eines systemd-freien Debian-Systems; die im
Artikel referenzierte Debian-Version ist älter, der Mechanismus der
`Depends`-Alternativen-Auflösung selbst ist unverändert aktuell).

### Auflösung: Devuan statt vanilla Debian

Statt das Alternativen-Problem auf vanilla Debian fragil zu umgehen,
zieht Tarno OS die Konsequenz und wechselt die Ziel-Distribution: von
vanilla Debian auf **Devuan** — eine reale, aktiv gepflegte
Debian-Ableitung mit identischem `apt`/`dpkg`/`live-build`-Ökosystem
(nur Mirror-Pointer `deb.devuan.org` statt `deb.debian.org` und
Init-Paket-Auswahl unterscheiden sich), die das systemd-Default-Pull-
Problem auf Distributions-Ebene gar nicht erst hat. Devuans `init`-
Metapaket löst standardmäßig zu `sysvinit-core` auf, nicht zu
`systemd-sysv` — kein Pinning-Kunstgriff nötig. Devuan unterstützt
zusätzlich `openrc` als vollwertige Init-Alternative, umschaltbar über
den Kernel-Boot-Parameter `init=/sbin/openrc-init` (analog zu dem
Boot-Parameter-Muster, das
`tarno-br2-external/board/tarno-m6700/syslinux.cfg` heute schon nutzt).

Quellen:
- <https://www.devuan.org/os/init-freedom> — Devuans eigene Darstellung
  der unterstützten Init-Systeme (sysvinit Standard, openrc/runit als
  offizielle Alternativen).
- <https://laskarnix.org/devuan-migrate-from-sysv-to-openrc-init/> —
  konkrete Schritt-für-Schritt-Anleitung: `apt install openrc`, danach
  `init=/sbin/openrc-init` als Boot-Parameter setzen (bei GRUB über
  `GRUB_CMDLINE_LINUX_DEFAULT` + `update-grub`, bei SYSLINUX äquivalent
  über die `APPEND`-Zeile).
- <https://dev1galaxy.org/viewtopic.php?id=7853> — Devuan-Forum-Diskussion
  desselben Wechsels, inkl. dem Hinweis, dass ohne zusätzliche
  agetty-Runlevel-Links (`rc-update add agetty.ttyN default`) ggf. kein
  Konsolen-Login-Prompt erscheint (siehe auch
  `tarno-devuan-live/config/package-lists/tarno.list.chroot`).

Der erste, echte (aber ungetestete) Code dazu liegt in
[`../../tarno-devuan-live/`](../../tarno-devuan-live/) — ein `live-build`-
Konfigurations-Skeleton, das `--distribution excalibur` (aktuelle
Devuan-Stable-Release) statt eines Debian-Codenamens verwendet.

## Quellen

- Debian Live Manual, Beispiele: <https://live-team.pages.debian.net/live-manual/html/live-manual/examples.en.html>
- Will Haley, "Building a Custom Debian Live Environment": <https://www.willhaley.com/blog/custom-debian-live-environment/>
- The Debian Administrator's Handbook: <https://debian-handbook.info/> (PDF, Buster-Ausgabe: <https://debian-handbook.info/download/buster/debian-handbook.pdf>)
- Not Invented Here, "Debootstrapping Debian Jessie Without Systemd": <https://www.notinventedhere.org/articles/linux/debootstrapping-debian-jessie-without-systemd.html> (Init-Alternativen-Auflösungsproblem, siehe Abschnitt "Das systemd-Default-Problem auf vanilla Debian" oben)
- Devuan, "Init Freedom": <https://www.devuan.org/os/init-freedom>
- Laskarnix, "Devuan: Migrate From Sysv to OpenRC Init": <https://laskarnix.org/devuan-migrate-from-sysv-to-openrc-init/>
- Dev1Galaxy-Forum, "How to pure OpenRC-based Devuan?": <https://dev1galaxy.org/viewtopic.php?id=7853>
