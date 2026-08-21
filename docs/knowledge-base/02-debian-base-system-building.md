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

## Quellen

- Debian Live Manual, Beispiele: <https://live-team.pages.debian.net/live-manual/html/live-manual/examples.en.html>
- Will Haley, "Building a Custom Debian Live Environment": <https://www.willhaley.com/blog/custom-debian-live-environment/>
- The Debian Administrator's Handbook: <https://debian-handbook.info/> (PDF, Buster-Ausgabe: <https://debian-handbook.info/download/buster/debian-handbook.pdf>)
