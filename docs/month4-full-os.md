# Monat 4 — Vollständiges Betriebssystem-Erlebnis (Detailplan)

Übersicht siehe [`ROADMAP.md`](../ROADMAP.md#monat-4--vollständiges-betriebssystem-erlebnis-über-den-ursprünglichen-3-monats-rahmen-hinaus). Dieser Monat liegt außerhalb des ursprünglichen 3-Monats-Rahmens — er entstand aus der Klarstellung, dass der USB-Stick nicht nur ein Boot-Medium sein soll, sondern eine echte Installation ermöglichen muss, die sich danach auch aktualisieren lässt, Apps installieren kann und die Basis-Funktionen bietet, die man von einem alltagstauglichen System erwartet.

## Festplatten-Installer (`tarno-disk-installer`)

**Wichtig, Abgrenzung zu `tarno-installer`:** `tarno-installer` läuft auf dem Rechner, der den USB-Stick *erstellt* (z. B. Windows) und schreibt ein fertiges `sdcard.img` auf den Stick. `tarno-disk-installer` ist ein **eigenständiges, neues Werkzeug**, das AUF Tarno OS selbst läuft — nachdem man vom Stick gebootet hat — und das laufende System auf die interne Platte installiert. Zwei verschiedene Programme, zwei verschiedene Ausführungsorte.

Ablauf:
1. **Zielgerät wählen**: alle physischen Platten außer dem aktuell gebooteten Gerät (dem Stick selbst) — Umkehrung der Logik aus `tarno-installer/src/devices.rs` (dort: nur removable, Root ausschließen; hier: alle physischen Platten, aktuelles Root-Gerät ausschließen).
2. **Partitionieren**: dasselbe bewährte Zwei-Partitionen-Layout wie im USB-Image (`genimage.cfg`) — kleine VFAT-Boot-Partition + ext4-Rootfs-Partition —, aber dynamisch auf die tatsächliche Größe der Zielplatte skaliert, über `sfdisk` (kein Reimplementieren von Partitionierungslogik in Rust — ein Kommandozeilenwerkzeug, das seit Jahrzehnten korrekt ist).
3. **Formatieren**: `mkfs.vfat` / `mkfs.ext4` auf den neuen Partitionen.
4. **Kopieren**: das aktuell laufende Root-Dateisystem (das vom Stick gebootete, funktionierende System) wird 1:1 auf die neue Rootfs-Partition übertragen — via `rsync -aHAX --one-file-system`, unter Ausschluss von `/proc`, `/sys`, `/dev`, `/run` und dem Mountpoint des Sticks selbst. Das ist robuster als ein separates Root-Image mitzuliefern: was gerade läuft, ist beweisbar ein funktionierendes System.
5. **Bootloader**: SYSLINUX/`extlinux --install` auf die neue Boot-Partition, MBR-Bootstrap-Code (`mbr.bin`) auf die Zielplatte — analog zu `post-image.sh`, nur gegen ein reales Blockgerät statt eine Image-Datei.

Sicherheitsmodell: dieselbe explizite Bestätigung mit vollem Geräte-Label wie bei `tarno-installer`, zusätzlich eine deutliche Warnung, dass die komplette Zielplatte überschrieben wird (kein Dual-Boot-Schutz in Version 1).

## System-Updates + App-Marktplatz — ein gemeinsamer Paketmanager

Bewusste Entscheidung: **kein separates Update-System und kein separates Marktplatz-System** — beides läuft über denselben Paketmanager, weil beides strukturell dasselbe Problem ist ("hole ein signiertes Paket aus einem Repository und installiere/aktualisiere es"). Buildroot bringt `BR2_PACKAGE_OPKG` bereits mit (ein für eingebettete Systeme entwickelter, sehr schlanker `apt`-Verwandter) — kein Grund, ein eigenes Paketformat zu erfinden.

- **System-Updates**: Kernpakete (Kernel, `tarnod`, Compositor, …) liegen im selben Repository wie Apps, nur mit einer eigenen Kategorie/Priorität.
- **App-Marktplatz**: eine GUI (`tarnod-ui` bekommt ein neues Panel, oder ein eigenständiges Werkzeug — Entscheidung fällt bei der Umsetzung) listet verfügbare Pakete aus dem Repository, zeigt Beschreibung/Größe, installiert per Klick über denselben `opkg`-Aufruf.
- **Kein A/B-Partitionsschema** (wie ChromeOS/Android) in Version 1 — passt nicht zur "so schlank wie möglich, ein Zielgerät"-Linie des Projekts und würde den Festplatten-Installer oben verkomplizieren (zwei Rootfs-Partitionen statt einer). Rollback-Fähigkeit bei fehlgeschlagenen Updates ist ein bekannter Kompromiss, der später nachgerüstet werden kann, falls nötig.

## Terminal

`foot` — ein sehr schlankes, natives Wayland-Terminal (kein X11-Unterbau nötig, passt direkt zu `tarno-desktop`) — als Buildroot-Paket, **kein Eigenbau**. Ein Terminal-Emulator ist ein gelöstes Problem; Zeit lieber in projekteigene Teile stecken. Taskleisten-Integration: ein weiteres Icon neben der Settings-Wordmark (derselbe Spawn-Mechanismus wie bei `tarnod-ui`, siehe [`month-desktop.md`](month-desktop.md#settings-app-als-teil-des-desktops-kein-separates-fenster-konzept)).

## Netzwerk (WLAN, Bluetooth)

- **WLAN**: `iwd` (von Intel entwickelt, deutlich schlanker als die klassische Kombination NetworkManager+wpa_supplicant, passt zur RAM-Trimm-Linie des Projekts) statt eines schwereren Netzwerk-Stacks.
- **Bluetooth**: `bluez` (der de-facto-Standard-Linux-Bluetooth-Stack, keine schlankere ausgereifte Alternative verfügbar).
- **UI**: neues Panel in `tarnod-ui` (WLAN-Scan/Verbinden, Bluetooth-Pairing) — folgt demselben Muster wie die bestehenden Panels (Dashboard/Gaming-Mode/Security/API-Keys), IPC über `tarnod` und `tarnod-protocol` wie gehabt.

## Scope-Hinweis

Dieser Monat ist zum Zeitpunkt dieses Dokuments **Planungsstand, nicht Umsetzungsstand** — `tarno-disk-installer` ist das erste Stück, das tatsächlich implementiert wird (siehe eigener Fortschritt in `tarno-disk-installer/`, falls das Verzeichnis existiert). Paketmanager/Marktplatz, Terminal-Integration und Netzwerk-Panel folgen danach. Wie beim restlichen Buildroot-Teil gilt: Partitionierungs-/Formatierungs-/Bootloader-Logik lässt sich gegen Loopback-Devices in einer Sandbox echt testen: die realen Endziele (Boot von der internen Platte, echtes `opkg`-Repository, echte WLAN-Hardware) brauchen reale Hardware bzw. Infrastruktur, die hier nicht verfügbar ist.
