# tarno-installer

Natives GUI (egui/eframe) zum Schreiben eines Tarno-OS-USB-Boot-Images
(`output/images/sdcard.img` aus [`../tarno-br2-external/`](../tarno-br2-external/))
auf einen Wechseldatenträger.

**Läuft nicht auf Tarno OS selbst** — das ist ein Werkzeug für den Rechner,
mit dem der Stick erstellt wird (vergleichbar mit Raspberry Pi Imager,
Rufus, balenaEtcher), und läuft nativ auf **Linux und Windows** — auch
wenn Tarno OS selbst Linux-basiert ist. Siehe [Windows-Nutzung](#windows-nutzung)
unten, falls der Stick von einem Windows-Rechner aus erstellt wird.

## Sicherheitsmodell

- Nur Geräte, die das Betriebssystem als Wechseldatenträger meldet (Linux: `/sys/block/<dev>/removable == 1`; Windows: `IOCTL_STORAGE_QUERY_PROPERTY` → `RemovableMedia`, siehe `src/win32.rs`), werden überhaupt zur Auswahl angeboten.
- Das Gerät, das die System-/Root-Partition trägt, wird zusätzlich explizit ausgeschlossen (Linux: Heuristik über `/proc/mounts`; Windows: `IOCTL_STORAGE_GET_DEVICE_NUMBER` gegen `%SystemDrive%`).
- Vor dem Schreiben muss der Nutzer eine explizite Bestätigung mit vollem Geräte-Label (Pfad, Hersteller, Modell, Größe) anhaken.
- Schreibt in reinem Rust (kein `dd`-Subprozess): unter Linux mit `O_SYNC`, unter Windows über direkten Zugriff auf `\\.\PhysicalDriveN` mit vorherigem Sperren/Dismounten der zugehörigen Laufwerksbuchstaben (`FSCTL_LOCK_VOLUME`/`FSCTL_DISMOUNT_VOLUME`, dasselbe Vorgehen wie Rufus/balenaEtcher) — beides sorgt dafür, dass Daten physisch committet sind, bevor der Vorgang als "fertig" gemeldet wird.

Details/Begründung: [`../docs/architecture.md`](../docs/architecture.md#tarno-installer-natives-gui-läuft-nicht-auf-tarno-os-selbst).

## Verwendung (Linux)

```sh
cargo build --release
cargo test              # Kopier-Engine + Geräte-Erkennung, ohne root testbar
sudo ./target/release/tarno-installer
```

`cargo test` läuft ohne root (die Kopier-Engine wird gegen normale Dateien
getestet, nicht gegen echte Blockgeräte) und prüft u. a. direkt gegen das
reale `/sys/block` des Testsystems, dass das Root-Gerät nie in der
Geräteliste auftaucht.

## Windows-Nutzung

Drei Wege, eine fertige `tarno-installer.exe` zu bekommen — **kein
manueller Rust-/Compiler-Aufbau nötig**:

1. **`scripts/windows/build-tarno-installer.ps1` ausführen** (empfohlen,
   wenn du direkt am Windows-Rechner sitzt): ein einziges Skript, das
   selbst prüft, ob Git und Rust vorhanden sind, beides bei Bedarf still
   nachinstalliert (Rust mit dem GNU-Toolchain-Profil, damit keine
   Visual-Studio-Build-Tools nötig sind), das Repo klont (falls noch
   nicht vorhanden) und `tarno-installer` im Release-Modus baut — direkt
   auf Windows, kein Cross-Compile.
   ```powershell
   powershell -ExecutionPolicy Bypass -File build-tarno-installer.ps1
   ```
   Die Ablauflogik (Erkennung von vorhandenem Checkout, `cargo build
   --release`, Fehlerbehandlung) ist in dieser Sandbox mit echtem
   PowerShell 7 End-to-End getestet — die beiden Windows-spezifischen
   Installationszweige (Git-for-Windows-/rustup-Download+Silent-Install)
   lassen sich hier naturgemäß nicht ausführen (kein Windows verfügbar),
   folgen aber den offiziellen Silent-Install-Flags von Git for Windows
   (`/VERYSILENT /NORESTART`) bzw. `rustup-init` (`-y --default-host
   x86_64-pc-windows-gnu --profile minimal`).
2. **Vorgebaut aus der CI**: jeder Push auf `main` baut in
   [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) automatisch
   eine `tarno-installer.exe` für Windows (Cross-Compile von Linux aus via
   `mingw-w64`). Im GitHub-Actions-Tab des Repos den neuesten `CI`-Run
   öffnen → Artifact `tarno-installer-windows-x86_64` herunterladen.
3. **Selbst cross-kompilieren** von einem Linux-Rechner/WSL2 aus:
   ```sh
   rustup target add x86_64-pc-windows-gnu
   sudo apt install mingw-w64          # Debian/Ubuntu
   cd tarno-installer
   cargo build --release --target x86_64-pc-windows-gnu
   # -> target/x86_64-pc-windows-gnu/release/tarno-installer.exe
   ```
   Diese exakte Kommandofolge ist in dieser Sandbox gegen das
   `x86_64-pc-windows-gnu`-Target verifiziert: es entsteht eine reale,
   startfähige PE32+-EXE (~14 MB im Release-Build).

## Woher kommt die eigentliche `sdcard.img`?

`tarno-installer` selbst enthält **kein** Betriebssystem (daher die
kleine Dateigröße von ~14 MB) — es ist nur das Flash-Werkzeug. Die
tatsächliche `sdcard.img` (das, was auf den Stick geschrieben wird)
entsteht über einen echten Buildroot-Build aus
[`../tarno-br2-external/`](../tarno-br2-external/), der über
[`.github/workflows/build-os-image.yml`](../.github/workflows/build-os-image.yml)
angestoßen werden kann (Actions-Tab → "Build Tarno OS image" → "Run
workflow") und dauert je nach Paketumfang gut eine bis mehrere Stunden.
Ergebnis liegt danach als Artifact `tarno-os-sdcard-img` zum Download
bereit.

Danach auf dem Windows-Rechner: `tarno-installer.exe` **als Administrator
ausführen** (Rechtsklick → "Als Administrator ausführen" — Rohschreibzugriff
auf ein Laufwerk braucht erhöhte Rechte, siehe `src/win32.rs::is_elevated`),
Pfad zum vorher gebauten `sdcard.img` eintragen, USB-Stick auswählen,
bestätigen.

**Wichtig — zwei getrennte Schritte, die leicht verwechselt werden:**
`tarno-installer` selbst läuft nur auf dem Rechner, mit dem der Stick
erstellt wird (hier: Windows) und **landet nicht auf dem Stick**. Was auf
den Stick geschrieben wird, ist das separate `sdcard.img` — das
eigentliche Tarno-OS-Boot-Image. Dieses Image selbst muss aus
[`../tarno-br2-external/`](../tarno-br2-external/) per Buildroot gebaut
werden, und **Buildroot läuft nicht unter Windows** (kein natives
Windows-Build-System) — dafür wird ein Linux-Rechner, eine Linux-VM oder
WSL2 gebraucht, oder alternativ ein CI-Build. Kurz: das Image bauen
passiert auf Linux, das Image auf den Stick schreiben kann auf Windows
passieren.

**Nicht laufzeitgetestet** auf echter Windows-Hardware (diese
Entwicklungsumgebung hat kein Windows) — nur cross-kompilier-verifiziert.
Rückmeldungen zu echten Testläufen sind entsprechend besonders wertvoll.

### Windows-spezifisches Backend (`src/win32.rs`)

Reines Win32 über die [`windows-sys`](https://crates.io/crates/windows-sys)-Crate (offizielle, schlanke FFI-Bindings, kein COM/.NET-Laufzeitanteil):

- Geräte-Enumeration: `\\.\PhysicalDrive0..31` öffnen, `IOCTL_STORAGE_QUERY_PROPERTY` für Wechselmedium-Status + Hersteller/Modell, `IOCTL_DISK_GET_LENGTH_INFO` für die Größe.
- Root-Geräte-Ausschluss: `IOCTL_STORAGE_GET_DEVICE_NUMBER` gegen `%SystemDrive%`.
- Schreiben: `CreateFileW` auf das physische Laufwerk, davor alle zugehörigen Laufwerksbuchstaben über `IOCTL_STORAGE_GET_DEVICE_NUMBER` identifizieren und per `FSCTL_LOCK_VOLUME`/`FSCTL_DISMOUNT_VOLUME` sperren/dismounten.
- Admin-Erkennung: `IsUserAnAdmin` (Windows-Äquivalent zu `geteuid() == 0`).
