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
| `config/package-lists/tarno.list.chroot` | Zusätzliche Pakete: `openrc`, `isolinux` |
| `config/includes.chroot/etc/init.d/tarnod` | OpenRC-Service-Skript für `tarnod`, portiert aus dem Buildroot-Pfad |
| `config/archives/devuan-security.list.{chroot,binary}` | Eigene Security-Archiv-Zeile (live-builds automatische Generierung ist für Devuan an keinem eingebauten Mode korrekt, siehe Update-Verlauf unten) |
| `config/bootloaders/isolinux/` | Eigene ISOLINUX-Boot-Menü-Vorlage — Kopie von live-builds eingebauter Vorlage, aber mit auf das moderne `isolinux`/`syslinux-common`-Paketlayout korrigierten `isolinux.bin`/`vesamenu.c32`-Symlinks (siehe Update-Verlauf unten) |

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
bisher überhaupt laufen könnte.

**Update — erster echter Lauf, echter Fehlschlag (ein konkretes Beispiel
für das "ehrlich verifizieren"-Muster, das dieses Projekt schon an
anderer Stelle praktiziert, siehe z. B.
[`docs/month3-tarno-layer.md`](../docs/month3-tarno-layer.md), Abschnitte
Phase 1/2/3, und den Buildroot-Kernel-Config-Fix in
[`docs/architecture.md`](../docs/architecture.md)):** Der Workflow wurde
inzwischen einmal real auf GitHub Actions ausgelöst (Actions-Run
[32560292687](https://github.com/coding-jona/Tarno-OS/actions/runs/32560292687),
volle Internet-Anbindung, kein Sandbox-Proxy) und ist tatsächlich
losgelaufen — aber nach ~20 Sekunden mit einem echten, root-caused Fehler
gescheitert:

```
P: Running debootstrap (download-only)...
E: No such script: /usr/share/debootstrap/scripts/excalibur
```

Root Cause: Ubuntus (bzw. Debians) eigenes `debootstrap`-Paket kennt unter
`/usr/share/debootstrap/scripts/` nur Debian/Ubuntu-Codenamen — Devuan-
Suite-Namen wie `excalibur` fehlen dort komplett. Bestätigt über einen
echten Debian-Bugreport
([#1067240](https://bugs-devel.debian.org/cgi-bin/bugreport.cgi?bug=1067240),
"debootstrap: Devuan install scripts in /usr/share/debootstrap/scripts/")
und ein unabhängiges Praxis-Rezept
([Gist](https://gist.github.com/CypherpunkSamurai/925f961b13a73a354636b56e2760d150)),
das beide denselben Fix nennen: Devuans eigenes, gepatchtes
`debootstrap`-Paket installieren (erkennbar an einer `+devuanN`-
Versionsendung) statt sich auf Ubuntus/Debians Archiv zu verlassen — plus
Devuans `devuan-keyring`-Paket für die echte GPG-Verifikation der
Release-Datei. Der Fix (Download + `dpkg -i` der Devuan-eigenen Pakete
vor dem `lb config`/`lb build`-Lauf, mit einer nachgelagerten
`debootstrap --version`-Prüfung, die laut abbricht, falls der Tausch
nicht gegriffen hat) ist in
[`.github/workflows/build-devuan-image.yml`](../.github/workflows/build-devuan-image.yml)
umgesetzt.

**Ehrlich gesagt:** Dieser Fix ist auf echter externer Recherche
gegründet (zwei unabhängige Quellen, s. o.), aber **noch durch keinen
erfolgreichen `lb build`-Lauf bestätigt** — weder in dieser
Entwicklungs-Sandbox (derselbe Netzwerk-Proxy blockiert `deb.devuan.org`
weiterhin) noch anderswo. Die echte Verifikation steht mit dem nächsten
`workflow_dispatch`-Lauf noch aus.

**Update — der Devuan-debootstrap-Fix hat real gegriffen:** Der nächste
`workflow_dispatch`-Lauf (Actions-Run
[32560959289](https://github.com/coding-jona/Tarno-OS/actions/runs/32560959289))
kam deutlich weiter als der erste — `debootstrap` lief diesmal komplett
durch ("I: Base system installed successfully.", echte Pakete aus
`deb.devuan.org/merged` inklusive `devuan-keyring`, `sysvinit-core`). Das
ist der erste real bestätigte Beweis, dass der debootstrap-Fix
funktioniert. Danach scheiterte der Lauf aber an einer neuen Stelle,
`lb_chroot_archives` (Einrichten der apt-Quellen im Chroot für die
Paketinstallation):

```
Ign:1 http://security.ubuntu.com/ubuntu excalibur-security InRelease
Err:2 http://security.ubuntu.com/ubuntu excalibur-security Release
  404  Not Found
E: The repository 'http://security.ubuntu.com/ubuntu excalibur-security Release' does not have a Release file.
```

Root Cause (verifiziert nicht per Doku-Vermutung, sondern am tatsächlich
installierten Paket: `apt-get install live-build` liefert auf Ubuntu
Noble — exakt der Unterbau von `runs-on: ubuntu-latest` — Version
`3.0~a57-1ubuntu49.1`, siehe
[Launchpad](https://launchpad.net/ubuntu/noble/amd64/live-build); diese
Version wurde hier lokal installiert und ihr echter Quellcode gelesen,
`/usr/share/live/build/functions/defaults.sh` und
`/usr/lib/live/build/lb_chroot_archives`): live-build kennt keinen
eigenen "devuan"-Mode (nur `debian`, `emdebian`, `progress`, `ubuntu`,
`kubuntu`). Ohne explizites `--mode` **rät** live-build den Mode über
`lsb_release -is` des Build-**Hosts** — auf dem GitHub-Actions-Runner
also "Ubuntu" → `LB_MODE=ubuntu`. Das kippt still eine ganze Reihe an
sich unabhängiger Defaults auf Ubuntu-Werte, unter anderem den
Security-Mirror (`http://security.ubuntu.com/ubuntu/` statt Devuans
`deb.devuan.org`) — daher der 404, denn Ubuntus Archiv kennt Devuans
Suite-Namen `excalibur-security` naturgemäß nicht. `deb.devuan.org/merged
excalibur-updates` (in derselben Logzeile davor) lief dagegen sauber
durch — kein Problem mit dem Mirror selbst, nur mit dieser einen
zusätzlichen, automatisch generierten Zeile.

Der naheliegende Gegen-Fix (`--mode debian` statt `ubuntu` erzwingen)
wurde geprüft und wäre **nicht ausreichend gewesen**: live-builds
"debian"-Zweig generiert die Security-Zeile im alten, seit ca. 2019
obsoleten Debian-Schema `<dist>/updates` statt `<dist>-security` — das
gibt es auf `deb.devuan.org` ebenfalls nicht (bestätigt per Recherche:
Devuans eigene, aktuell dokumentierte sources.list nennt exakt
`excalibur-security` auf `deb.devuan.org/merged`, siehe
[devuan.org/os/packages](https://www.devuan.org/os/packages)). Keiner der
beiden in dieser (von Ubuntu seit Jahren eingefrorenen) live-build-Version
eingebauten Mode-Zweige trifft Devuans tatsächliches, aktuelles Layout
korrekt — schlicht weil "Devuan" für dieses Tool nicht existiert.

**Fix (umgesetzt in `auto/config` und `config/archives/`):**
`--mode debian` erzwungen (behebt daneben auch einen zweiten, noch nicht
aufgetretenen, aber am selben Code verifizierten Bug: unter
`LB_MODE=ubuntu` hätte live-build später versucht, das
Ubuntu-Kernel-Metapaket `linux-generic` zu installieren, das es in
Devuans Archiv nicht gibt — Debian/Devuan heißt es `linux-image-amd64`),
zusammen mit `--security false` (schaltet live-builds eigene, für Devuan
falsche Security-Zeilen-Generierung ab) plus zwei neuen Dateien,
[`config/archives/devuan-security.list.chroot`](config/archives/devuan-security.list.chroot)
und `.list.binary`, die live-builds eigenen, Mode-unabhängigen
"third-party archives"-Mechanismus nutzen, um exakt die von Devuan
dokumentierte Zeile einzutragen: `deb http://deb.devuan.org/merged
excalibur-security main`. Lokal mit dem echten, oben identifizierten
live-build-Paket verifiziert: `lb config` läuft fehlerfrei durch und
erzeugt in `config/chroot` genau `LB_MODE=debian`, `LB_SECURITY=false`,
`LB_LINUX_PACKAGES=linux-image`, `LB_LINUX_FLAVOURS=amd64`; die
Platzhalter-Ersetzung in den neuen `config/archives/`-Dateien wurde
manuell nachvollzogen und ergibt exakt `deb
http://deb.devuan.org/merged excalibur-security main`. Ein echter
`lb build`-Netzwerklauf bleibt weiterhin nur in GitHub Actions möglich
(derselbe Sandbox-Proxy-Block wie überall in diesem Track) — das ist der
nächste noch ausstehende Verifikationsschritt.

**Update — Security-Fix bestätigt, ein Whack-a-Mole-Nebeneffekt
gefunden:** Der nächste `workflow_dispatch`-Lauf (Actions-Run
[32562474234](https://github.com/coding-jona/Tarno-OS/actions/runs/32562474234))
bestätigt den Security-Fix oben eindeutig — im Log sind jetzt echte,
erfolgreiche Zeilen zu sehen:

```
Get:9 http://deb.devuan.org/merged excalibur-security/main amd64 Packages [251 kB]
Get:1 http://deb.devuan.org/merged excalibur-security/main amd64 bsdutils amd64 1:2.41.5-0+deb13u1devuan1 [111 kB]
```

Pakete wurden also real aus `excalibur-security` installiert — keine
404 mehr. Der Build brach aber an neuer Stelle ab, `lb_chroot_linux-image`:

```
[2026-08-22 08:33:13] lb_chroot_linux-image
--2026-08-22 08:33:13--  http://deb.devuan.org/merged/dists/excalibur/Contents-amd64.gz
HTTP request sent, awaiting response... 404 Not Found
gzip: stdin: unexpected end of file
```

Root Cause (wieder am tatsächlich installierten `live-build`-Paket
verifiziert): der `--mode debian`-Fix aus der vorigen Runde hatte einen
zweiten, bis dahin nicht sichtbaren Nebeneffekt. `LB_FIRMWARE_CHROOT`
(steuert, ob live-build automatisch nach Firmware-Paketen sucht)
defaultet laut `functions/defaults.sh` im `ubuntu`-Zweig auf `false`, in
jedem anderen Modus (also auch im hier erzwungenen `debian`) auf `true`.
Aktiviert, lädt `lb_chroot_linux-image` eine `Contents-<arch>.gz`-Datei
von der Mirror-Wurzel, um darin nach `lib/firmware`-Pfaden zu suchen —
diese Datei liegt unter dem von dieser (alten) live-build-Version
erwarteten Pfad auf `deb.devuan.org` nicht vor.

Zusätzlich aufgefallen (kein Aufreger, aber real): der Schritt
`lb build` im Workflow meldete trotz dieses Abbruchs **"success"** —
`sudo lb build 2>&1 | tee build.log` läuft ohne `pipefail` unter
GitHub Actions' Standard-Shell (`bash -e {0}`, verifiziert am echten
Job-Log), der Exit-Code der Pipe ist also der von `tee` (fast nie
fehlerhaft), nicht der von `lb build`. Nur der nachfolgende
"Ergebnis-Image prüfen"-Schritt hat den Fehlschlag über das fehlende
`.iso` überhaupt bemerkt.

**Fix:** `--firmware-chroot false` / `--firmware-binary false` in
`auto/config` — bewusst keine Suche nach dem "richtigen"
Contents-Dateipfad, weil Firmware-Pakete bei Devuan (wie bei Debian)
ohnehin in `non-free-firmware`/`non-free` liegen und dieser Build
bewusst nur `--archive-areas main` nutzt (siehe Abschnitt "Was hier
bewusst NICHT drin ist" unten) — ein erfolgreicher Contents-Abruf hätte
hier ohnehin nichts beigetragen. Zusätzlich `set -o pipefail` vor dem
`lb build`-Aufruf in
[`build-devuan-image.yml`](../.github/workflows/build-devuan-image.yml)
ergänzt, damit ein künftiger Abbruch mitten in `lb build` den Workflow-
Schritt auch tatsächlich als fehlgeschlagen markiert, statt sich auf den
nachgelagerten ISO-Check verlassen zu müssen.

**Update — `pipefail`-Fix bewährt sich sofort, weitester Lauf bisher,
neuer Fehler beim ISOLINUX-Bootloader:** Der nächste
`workflow_dispatch`-Lauf (Actions-Run
[32562958758](https://github.com/coding-jona/Tarno-OS/actions/runs/32562958758))
bestätigt zwei Dinge auf einmal: der `pipefail`-Fix greift (der
`lb build`-Schritt meldete diesmal korrekt "failure" statt fälschlich
"success"), und der Lauf kam deutlich weiter als je zuvor — komplette
Chroot-Paketinstallation inklusive Kernel (`lb_binary_linux-image`),
`memtest86+`, alle isolinux/syslinux-Pakete erfolgreich installiert. Der
Abbruch kam diesmal in `lb_binary_syslinux`, beim Zusammenbau der
ISO-Bootdateien:

```
cp: cannot stat '/root/isolinux/isolinux.bin': No such file or directory
cp: cannot stat '/root/isolinux/vesamenu.c32': No such file or directory
```

Root Cause (wieder am tatsächlich installierten `live-build`-Paket
verifiziert, `/usr/lib/live/build/lb_binary_syslinux` sowie die
Paketinhalte der von Ubuntu Noble bezogenen `isolinux`/
`syslinux-common`-Pakete derselben Upstream-Version wie im Chroot-Log
sichtbar, `3:6.04~git20190206.bf6db5b4+dfsg1-...`): live-builds
eingebaute ISOLINUX-Vorlage
(`/usr/share/live/build/bootloaders/isolinux/`) enthält zwei Symlinks,
`isolinux.bin -> /usr/lib/syslinux/isolinux.bin` und `vesamenu.c32 ->
/usr/lib/syslinux/vesamenu.c32`, die beim Kopieren in den Chroot mit
`cp -aL` aufgelöst werden müssen. Diese Pfade stammen aus der Zeit VOR
dem großen syslinux-5-Split (2014): seitdem liefert **ein separates
Paket namens `isolinux`** `isolinux.bin` unter `/usr/lib/ISOLINUX/`
(nicht mehr Teil von `syslinux` selbst), und `syslinux-common` liefert
`vesamenu.c32` unter `/usr/lib/syslinux/modules/bios/` (BIOS/EFI-Split),
nicht mehr flach unter `/usr/lib/syslinux/`. Diese (von Ubuntu seit
~2012 eingefrorene) live-build-Version kennt beide Umzüge nicht — und
prüft in `lb_binary_syslinux`s `Check_package`-Aufrufen zudem nur auf
`syslinux`/`syslinux-common`, nie auf `isolinux` selbst, weshalb dieses
Paket bislang gar nicht erst installiert wurde.

**Fix:** Zwei Teile, beide nötig:
1. `isolinux` zu
   [`config/package-lists/tarno.list.chroot`](config/package-lists/tarno.list.chroot)
   hinzugefügt (live-build erkennt die fehlende Abhängigkeit nicht
   automatisch, s. o.).
2. Eigene ISOLINUX-Vorlage unter
   [`config/bootloaders/isolinux/`](config/bootloaders/isolinux/) —
   live-build unterstützt genau dafür einen offiziellen lokalen
   Override-Mechanismus (`lb_binary_syslinux`: "Prefer archives from the
   config tree" / "Internal local copy", greift automatisch, sobald
   `config/bootloaders/<bootloader>/` existiert). Identische Kopie der
   eingebauten Vorlage, nur die zwei Symlinks auf die tatsächlichen,
   modernen Paketpfade korrigiert (`isolinux.bin` →
   `/usr/lib/ISOLINUX/isolinux.bin`, `vesamenu.c32` →
   `/usr/lib/syslinux/modules/bios/vesamenu.c32`). Kein Fork von
   live-build nötig, keine Änderung an dessen eigenen Skripten — eine
   von live-build selbst vorgesehene Erweiterungsstelle.

Verifiziert: `sudo lb config` lokal erneut ausgeführt, `config/bootloaders/isolinux/`
bleibt dabei unangetastet erhalten (wie für eingecheckte `config/`-Inhalte
erwartet); `file`/`ls -la` bestätigen, dass beide Symlinks jetzt auf die
per `dpkg-deb -c` gegen die echten `isolinux`/`syslinux-common`-Pakete
verifizierten realen Pfade zeigen.

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
