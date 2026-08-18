# Tarno OS – Architektur

Dieses Dokument beschreibt die technische Gesamtarchitektur der in [`ROADMAP.md`](../ROADMAP.md) skizzierten Komponenten. Details je Monat stehen in [`month1-foundation.md`](month1-foundation.md), [`month2-gaming-tuning.md`](month2-gaming-tuning.md) und [`month3-tarno-layer.md`](month3-tarno-layer.md).

## Komponentenübersicht

```
┌─────────────────────────────────────────────────────────────────┐
│ Tarno OS (Buildroot-Image, Kernel für Dell Precision M6700)      │
│                                                                   │
│  ┌───────────────┐   Unix-Socket    ┌────────────────────────┐  │
│  │   tarnoctl    │◄────/run/tarnod/────►│        tarnod        │  │
│  │  (CLI-Client) │   tarnod.sock    │   (Root-Daemon, Rust)   │  │
│  ├───────────────┤        ▲          │                          │  │
│  │   tarnod-ui   │────────┘          │                          │  │
│  │ (natives GUI) │                   │                          │  │
│  └───────────────┘   0600, tokio    │                          │  │
│                                     │  ┌──────────┐ ┌────────┐ │  │
│                                     │  │ vault.rs │ │gaming.rs│ │  │
│                                     │  │ API-Keys │ │isolcpus/│ │  │
│                                     │  │ im RAM   │ │chrt/gov │ │  │
│                                     │  └──────────┘ └────────┘ │  │
│                                     │  ┌──────────────────────┐│  │
│                                     │  │ security/ebpf_loader ││  │
│                                     │  │ (aya, RingBuf-Poll)  ││  │
│                                     │  └──────────┬───────────┘│  │
│                                     └─────────────┼────────────┘  │
│                                                    │ attach        │
│                                     ┌──────────────▼────────────┐  │
│                                     │ tarno-guard-ebpf (Kernel) │  │
│                                     │ Tracepoint sched_process_ │  │
│                                     │ exec → RingBuf-Event      │  │
│                                     └───────────────────────────┘  │
│                                                                   │
│  ┌──────────────────────┐   ┌─────────────────────────────────┐  │
│  │ scripts/gaming-mode.sh│   │ cage (Wayland-Compositor)       │  │
│  │ scripts/jvm-launch.sh │   │ → Minecraft/JVM Direct-Fullscreen│  │
│  └──────────────────────┘   └─────────────────────────────────┘  │
│                                                                   │
│                              ┌─────────────────────────────────┐  │
│                              │ tarno-desktop (eigener Compositor)│  │
│                              │ Wayland (smithay) + fusionierte   │  │
│                              │ Taskleiste im selben Prozess      │  │
│                              │ → primäre OS-Experience (Standard)│  │
│                              └─────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Komponenten im Detail

### `tarnod` (Root-Daemon)
- Rust-Binary, läuft als OpenRC-Service (`S60tarnod`), Single-Process, `tokio`-Runtime im `current_thread`-Modus (kein Multi-Thread-Overhead, da IO-lastig statt CPU-lastig).
- Verantwortlich für: IPC-Server, API-Key-Vault, Gaming-Mode-Steuerung, eBPF-Loader/Policy-Engine.
- Warum ein Prozess statt mehrere: eBPF-RingBuf-Polling läuft ohnehin async — dieselbe Runtime bedient den IPC-Socket, statt zwei Concurrency-Modelle (Threads + async) im System zu mischen. Details/Begründung: [`month3-tarno-layer.md`](month3-tarno-layer.md#ipc-design).

### `tarnoctl` (CLI-Client)
- Dünner Client, verbindet sich auf denselben Unix-Socket, schickt JSON-Requests (`GetGamingMode`, `SetGamingMode`, `GetApiKey`, `SecurityStatus`, `ResumeProcess`), zeigt Antwort formatiert an.
- Kein eigener State, keine Privilegien nötig außer Socket-Zugriff (Gruppe `tarno` oder root).

### `tarnod-ui` (natives GUI)
- Eframe/egui-App (ein natives Binary, kein Browser/Electron/Node-Overhead — passt zur "schlank + nativ"-Linie des restlichen Stacks), verbindet sich auf denselben Unix-Socket.
- Vier Panels: Dashboard (Überblick), Gaming-Mode (Governor an/aus + isolcpus-Status), Security (eBPF-Status + Prozess-Resume), API-Keys (Abfrage einzelner Werte zur Kontrolle).
- IPC läuft in einem eigenen OS-Thread synchron (die GUI selbst ist immediate-mode, kein async nötig); Requests/Antworten werden über Channels mit der egui-Render-Schleife verbunden (`tarnod-ui/src/client.rs`).
- Nutzt dieselben Typen wie der Daemon über das gemeinsame Crate `tarnod-protocol` (statt eigener JSON-String-Literale wie in `tarnoctl`).

### `tarnod-protocol` (geteiltes Typen-Crate)
- `Request`/`Response`-Enums (siehe `tarnod/src/ipc.rs` früher, jetzt hier zentral), von `tarnod`, potenziell `tarnoctl` und `tarnod-ui` genutzt — eine Protokolländerung muss nur an einer Stelle nachgezogen werden.

### `tarno-guard-ebpf` (Behavioral Security)
- Eigenständiger 3-Crate-Workspace (Kernel-Space-Programm + Common-Types + Userspace-Loader-Lib), von `tarnod` als optionales Cargo-Feature `ebpf` eingebunden.
- Hook: Tracepoint `sched_process_exec` (kein LSM-Hook in Phase 1 — robuster, keine `CONFIG_BPF_LSM`-Abhängigkeit im Kernel).
- Datenfluss: eBPF-Programm schreibt `ExecEvent{pid, uid, comm, filename}` in eine `RingBuf`-Map → Userspace liest asynchron → Policy-Engine (Allow-/Deny-Liste aus Config) entscheidet → bei Treffer `kill(pid, SIGSTOP)` aus Userspace (**nicht** aus eBPF selbst, siehe Begründung in [`month3-tarno-layer.md`](month3-tarno-layer.md#warum-sigstop-aus-userspace)).

### Gaming-Mode-Skripte
- `scripts/gaming-mode.sh`: schaltet CPU-Isolation/Governor/THP um, unabhängig von `tarnod` aufrufbar (z. B. manuell oder aus einem Login-Hook).
- `scripts/jvm-launch.sh`: Wrapper, der die JVM mit passender Core-Affinität und Priorität startet.
- Diese Skripte sind bewusst **nicht** in `tarnod` verdrahtet (kein IPC-Aufruf nötig) — einfache, auditierbare Shell-Skripte statt zusätzlicher Daemon-Logik für einen einmaligen Vorgang beim Spielstart.

### Buildroot-Integration (`tarno-br2-external/`)
- `BR2_EXTERNAL`-Tree mit eigenem Package `tarnod` (cargo-package-Infra) und Board-Definition `tarno-m6700` (Kernel-Config-Fragment, Rootfs-Overlay).
- Das eBPF-Objekt wird **vorkompiliert** eingebettet (nicht im Buildroot-Cross-Build erzeugt), um Host-BPF-Toolchain-Bootstrapping im Cross-Build zu vermeiden. Details: [`month1-foundation.md`](month1-foundation.md#woche-1-2-basis-system).

### `tarno-installer` (natives GUI, läuft NICHT auf Tarno OS selbst)
- Eframe/egui-App, die auf dem Rechner läuft, der den USB-Stick erstellt (z. B. der Alltags-Rechner des Nutzers) — vergleichbar mit Raspberry Pi Imager/Rufus/balenaEtcher, kein Teil des Tarno-OS-Images.
- Schreibt ein per [`tarno-br2-external`](#buildroot-integration-tarno-br2-external) gebautes `sdcard.img` blockweise auf ein Zielgerät (reine Rust-Kopier-Engine statt `dd`-Subprozess, für volle Kontrolle über Fortschritt/Abbruch).
- Sicherheitsmodell (siehe `tarno-installer/src/devices.rs`): nur Geräte mit `/sys/block/<dev>/removable == 1` (Linux) bzw. `RemovableMedia` (Windows) werden überhaupt zur Auswahl angeboten, das Root-/System-Gerät ist zusätzlich explizit ausgeschlossen, und vor dem Schreiben muss der Nutzer eine explizite Bestätigung mit vollem Geräte-Label anhaken.
- **Läuft nativ auf Linux und Windows**, obwohl Tarno OS selbst Linux-basiert ist — der Rechner, der den Stick erstellt, muss nicht Linux sein. Unter Windows übernimmt `tarno-installer/src/win32.rs` (reines Win32 via `windows-sys`, kein COM/.NET) Geräte-Enumeration und Rohschreibzugriff auf `\\.\PhysicalDriveN` (inkl. Sperren/Dismounten der zugehörigen Laufwerksbuchstaben vor dem Schreiben). Cross-Compile-verifiziert gegen `x86_64-pc-windows-gnu`, nicht auf echter Windows-Hardware laufzeitgetestet. Details: [`../tarno-installer/README.md`](../tarno-installer/README.md#windows-nutzung).
- CI (`.github/workflows/ci.yml`) baut bei jedem Push automatisch fertige `tarno-installer`-Binaries für Linux und Windows als Download — kein eigener Toolchain-Aufbau nötig, um den Installer zu bekommen (das eigentliche `sdcard.img` bleibt ein separater, manueller Buildroot-Schritt, siehe [`month1-foundation.md`](month1-foundation.md)).
- Teilt sich das visuelle Theme (`tarno-ui-theme`) mit `tarnod-ui`, damit alle Tarno-OS-Werkzeuge optisch aus einem Guss wirken.

### `tarno-desktop` (eigener Compositor + fusionierte Taskleiste — **primäre OS-Experience**)
- Eigener, minimaler Wayland-Compositor (Rust/`smithay`) — das ist das, was beim normalen Login startet. `cage` bleibt als dedizierter Kiosk-Pfad nur für den Gaming-Vollbild-Fall bestehen (ein Fenster fullscreen, kein Fensterverwaltungs-Overhead), ist aber nicht mehr der Standardfall.
- Die Taskleiste ist **kein separater Panel-Client** (kein `wlr-layer-shell`, kein zweiter Prozess) — sie wird als einzelnes texturiertes Rechteck direkt im Compositor-Render-Loop gezeichnet, gerendert aus einem flachen RGBA8-Softwarepuffer (`fontdue` für Text, kein Widget-Toolkit). Begründung (Performance statt Funktionsumfang, verglichen mit `sway`+`waybar`): [`month-desktop.md`](month-desktop.md#warum-ein-zweiter-compositor-statt-nur-cage).
- Zeigt live `tarnod`-Status (Verbindung, `isolcpus`, eBPF-Wächter) über einen read-only Poll-Thread gegen denselben Unix-Socket wie `tarnod-ui`/`tarnoctl` (`tarnod-protocol`-Typen, kein Steuer-Zugriff).
- **`tarnod-ui` ist die alleinige Settings-App des Desktops**: Klick auf die "TARNO"-Wordmark in der Taskleiste spawnt sie als echten Wayland-Client gegen den eigenen Socket — sie rendert als XDG-Toplevel-Fenster innerhalb des Compositors, nicht als eigenständiges Programm. Details: [`month-desktop.md`](month-desktop.md#settings-app-als-teil-des-desktops-kein-separates-fenster-konzept).
- Compositor + Taskleiste + echtes XDG-Client-Rendering (verifiziert am Beispiel des Settings-Klicks) sind getestet; DRM/KMS-Bare-Metal-Backend, die Boot-Moduswahl `cage` vs. `tarno-desktop` und allgemeine Zeiger-Weiterleitung an Client-Fenster über den Settings-Fall hinaus sind noch offen. Details: [`month-desktop.md`](month-desktop.md#scope-was-in-dieser-runde-verifiziert-wurde).

## Dual-Mode: Gaming vs. Desktop

Tarno OS startet je nach Anwendungsfall einen von zwei Wayland-Compositors — nie beide gleichzeitig. `tarno-desktop` ist die **Standard-/Hauptversion**; `cage` ist der dedizierte Sonderfall für Gaming:

| | Desktop-Modus (Standard) | Gaming-Modus |
|---|---|---|
| Compositor | `tarno-desktop` | `cage` |
| Zweck | Alltag: Settings (`tarnod-ui`), Browser, `tarno-installer`, mehrere Fenster | Ein Spiel/eine JVM, fullscreen |
| Taskleiste | fusioniert im Compositor-Prozess, mit Settings-Launcher | keine |
| Overhead-Ziel | minimal für echte Fensterverwaltung | minimal möglich (Kiosk) |

Beide Compositors nutzen dasselbe visuelle Theme (`tarno-ui-theme`) für alle darüber laufenden nativen GUIs, damit der Wechsel zwischen den Modi optisch konsistent wirkt. Die Umschaltung selbst (welcher Compositor beim Login startet) ist noch nicht in `tarno-br2-external` verdrahtet (Stage 2, siehe [`month-desktop.md`](month-desktop.md)).

## Sicherheitsmodell (Userspace-Isolation statt Kernel-Vault)

Kein eigener Kernel-Vault nötig, weil:
1. API-Keys liegen ausschließlich im RAM des `tarnod`-Prozesses (aus einer root-only `0600`-Datei beim Start geladen, danach nicht erneut von Platte gelesen).
2. Zugriff nur über den Unix-Socket, der selbst `0600` ist und zusätzlich per `SO_PEERCRED` die UID/GID des anfragenden Prozesses verifiziert (doppelte Absicherung gegen falsch gesetzte Dateirechte).
3. Kein anderer Prozess auf dem System kann den Speicher von `tarnod` ohne `CAP_SYS_PTRACE`/root lesen — das ist auf Tarno OS ohnehin nur der Daemon selbst.
