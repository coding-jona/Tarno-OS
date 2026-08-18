# tarno-desktop

**Primäre OS-Experience von Tarno OS.** Eigener, minimaler
Wayland-Compositor (Rust/[smithay](https://github.com/Smithay/smithay)),
mit direkt in den Compositor-Prozess eingebauter Taskleiste (kein
separater Panel-Client, kein `wlr-layer-shell`, kein zweiter
Prozess/Event-Loop/GL-Context) — das ist, was beim normalen Login startet.

Der Gaming-Modus nutzt weiterhin `cage` (Kiosk-Compositor, ein Fenster
fullscreen, kein Fensterverwaltungs-Overhead) als dedizierten, minimalen
Vollbild-Pfad — aber `tarno-desktop` ist der Standardfall.

Details/Begründung: [`../docs/architecture.md`](../docs/architecture.md#dual-mode-gaming-vs-desktop),
[`../docs/month-desktop.md`](../docs/month-desktop.md).

## Settings-App: `tarnod-ui` läuft im Desktop

`tarnod-ui` (Dashboard, Gaming-Mode, Security, API-Keys) ist die einzige
Einstellungs-Oberfläche von Tarno OS — kein eigenständiges Programm mehr,
sondern aus der Taskleiste heraus gestartet:

- Klick auf die "TARNO"-Wordmark (links, mit Akzentlinie als
  Start-Knopf-Affordanz markiert) spawnt `tarnod-ui` als Kindprozess.
  `WAYLAND_DISPLAY` wird vererbt — die App verbindet sich als echter
  Wayland-Client gegen den eigenen Compositor-Socket und rendert als
  XDG-Toplevel-Fenster **innerhalb** von `tarno-desktop`.
- `TARNO_DESKTOP_SETTINGS_BIN` überschreibt den Pfad zur Settings-Binary
  (Default: `tarnod-ui` aus `$PATH`).
- Wiederholtes Klicken öffnet kein zweites Fenster (`Child::try_wait()`-
  Check in `state.rs`).

## Scope

**Verifiziert:** Compositor bootet, rendert Hintergrund + fusionierte
Taskleiste (Wordmark, `tarnod`-Verbindungsstatus, `isolcpus`- und
eBPF-Status, live Uhrzeit — gegen einen echten laufenden `tarnod`
bestätigt), exponiert einen echten `WAYLAND_DISPLAY`-Socket. **Echtes
XDG-Client-Rendering ist jetzt mit `tarnod-ui` als realem Client
verifiziert** (per simuliertem Klick unter Xvfb) — vormals der einzige
offene Punkt.

**Offen:** DRM/KMS-Backend für Bare-Metal-Boot ohne Host-Compositor;
Boot-Integration (Moduswahl `cage` vs. `tarno-desktop`); Zeiger-Events
werden noch nicht an Client-Fenster weitergereicht (nur Tastatur) — echte
Maus-Interaktion *innerhalb* der Settings-App ist entsprechend noch nicht
verifiziert, nur dass das Fenster korrekt rendert. Details:
[`../docs/month-desktop.md`](../docs/month-desktop.md#scope-was-in-dieser-runde-verifiziert-wurde).

## Verwendung

```sh
cargo build
cargo test               # 12 Tests, ohne Wayland-Socket/GPU: Uhrzeit, Text-Rasterizer, Taskleiste, Klick-Hittest
mkdir -p /tmp/xdg-runtime && chmod 0700 /tmp/xdg-runtime
# tarnod-ui muss im $PATH liegen (oder TARNO_DESKTOP_SETTINGS_BIN setzen)
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
