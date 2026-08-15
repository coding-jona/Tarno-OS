# tarno-desktop

Eigener, minimaler Wayland-Compositor (Rust/[smithay](https://github.com/Smithay/smithay))
für den Tarno-OS-Desktop-Modus, mit direkt in den Compositor-Prozess
eingebauter Taskleiste (kein separater Panel-Client, kein `wlr-layer-shell`,
kein zweiter Prozess/Event-Loop/GL-Context).

Der Gaming-Modus nutzt weiterhin `cage` (Kiosk-Compositor, ein Fenster
fullscreen, kein Fensterverwaltungs-Overhead) — `tarno-desktop` ist nur für
den Nicht-Gaming-Alltagsbetrieb (Browser, Dateimanager, `tarnod-ui`,
`tarno-installer` parallel offen).

Details/Begründung: [`../docs/architecture.md`](../docs/architecture.md#dual-mode-gaming-vs-desktop),
[`../docs/month-desktop.md`](../docs/month-desktop.md).

## Scope

**Stage 1 (aktueller Stand):** Compositor bootet, rendert Hintergrund +
fusionierte Taskleiste (Wordmark, `tarnod`-Verbindungsstatus, `isolcpus`-
und eBPF-Status, live Uhrzeit — alles gegen einen echten laufenden `tarnod`
verifiziert), exponiert einen echten `WAYLAND_DISPLAY`-Socket.

**Stage 2 (offen):** echter XDG-Shell-Client wurde in dieser Runde nicht
gegen den Socket verbunden getestet (nur code-seitig verdrahtet); DRM/KMS-
Backend für Bare-Metal-Boot ohne Host-Compositor; Boot-Integration
(Moduswahl `cage` vs. `tarno-desktop`). Details: [`../docs/month-desktop.md`](../docs/month-desktop.md#scope-stage-1-diese-runde-vs-stage-2).

## Verwendung

```sh
cargo build
cargo test               # 8 Tests, ohne Wayland-Socket/GPU: Uhrzeit, Text-Rasterizer, Taskleiste
mkdir -p /tmp/xdg-runtime && chmod 0700 /tmp/xdg-runtime
XDG_RUNTIME_DIR=/tmp/xdg-runtime ./target/debug/tarno-desktop
```

Entwicklungs-Backend ist `winit` (genestet in einem X11/Wayland-Host-
Fenster, läuft auch unter Xvfb). Ein spawnter Wayland-Client kann sich mit
`WAYLAND_DISPLAY=tarno-desktop-0` gegen den laufenden Compositor verbinden.

## Warum kein separater Panel-Client

Siehe [`../docs/month-desktop.md`](../docs/month-desktop.md#warum-ein-zweiter-compositor-statt-nur-cage)
für die volle Begründung — kurz: die Taskleiste ist nur ein texturiertes
Rechteck im ohnehin laufenden Compositor-Render-Loop, statt eines eigenen
Prozesses mit eigenem Event-Loop, eigenem GL-Context und
Wayland-Protokoll-Overhead zum Compositor.
