# tarno-installer

Natives GUI (egui/eframe) zum Schreiben eines Tarno-OS-USB-Boot-Images
(`output/images/sdcard.img` aus [`../tarno-br2-external/`](../tarno-br2-external/))
auf einen Wechseldatenträger.

**Läuft nicht auf Tarno OS selbst** — das ist ein Werkzeug für den Rechner,
mit dem der Stick erstellt wird (vergleichbar mit Raspberry Pi Imager,
Rufus, balenaEtcher).

## Sicherheitsmodell

- Nur Geräte, die der Kernel als `removable` markiert (`/sys/block/<dev>/removable == 1`), werden überhaupt zur Auswahl angeboten.
- Das Gerät, das die Root-Partition trägt, wird zusätzlich explizit ausgeschlossen (Heuristik über `/proc/mounts`).
- Vor dem Schreiben muss der Nutzer eine explizite Bestätigung mit vollem Geräte-Label (Pfad, Hersteller, Modell, Größe) anhaken.
- Schreibt in reinem Rust (kein `dd`-Subprozess) mit `O_SYNC`, damit Daten physisch committet sind, bevor der Vorgang als "fertig" gemeldet wird.

Details/Begründung: [`../docs/architecture.md`](../docs/architecture.md#tarno-installer-natives-gui-läuft-nicht-auf-tarno-os-selbst).

## Verwendung

```sh
cargo build --release
cargo test              # Kopier-Engine + Geräte-Erkennung, ohne root testbar
sudo ./target/release/tarno-installer
```

`cargo test` läuft ohne root (die Kopier-Engine wird gegen normale Dateien
getestet, nicht gegen echte Blockgeräte) und prüft u. a. direkt gegen das
reale `/sys/block` des Testsystems, dass das Root-Gerät nie in der
Geräteliste auftaucht.
