# Desktop-Modus (`tarno-desktop`) — Detailplan

Übersicht siehe [`ROADMAP.md`](../ROADMAP.md) und [`architecture.md`](architecture.md#dual-mode-gaming-vs-desktop). Umgesetzt als eigenständiges Crate [`../tarno-desktop/`](../tarno-desktop/).

## Warum ein zweiter Compositor statt nur `cage`

`cage` (siehe [`month1-foundation.md`](month1-foundation.md)) bleibt für den
Gaming-Modus bestehen: ein Kiosk-Compositor, der genau ein Fenster
fullscreen zeigt, ohne Fensterverwaltung, ohne Taskleiste, ohne jeden
zusätzlichen Render-Pfad. Das ist für Spiele exakt richtig und wird nicht
angetastet.

Für den Alltagsbetrieb (Browser, Dateimanager, `tarnod-ui`, `tarno-installer`
parallel offen) reicht ein reiner Kiosk-Compositor nicht — dafür braucht es
echte Fensterverwaltung und eine sichtbare Statusanzeige. Zwei Optionen
standen zur Wahl:

1. Ein bestehender General-Purpose-Compositor (z. B. `sway`) + ein separater
   Panel-Client über `wlr-layer-shell` (z. B. `waybar`).
2. Ein eigener, minimaler Compositor, der die Taskleiste **direkt im
   Compositor-Prozess** rendert statt als eigenen Client.

Entscheidung: **Option 2**, aus reinen Performance-Gründen (das ist die
Leitlinie des gesamten Projekts — "in jeder Ritze" leistungsgetrimmt):

- Kein zweiter Prozess, kein zweiter Event-Loop, kein zweiter
  GL/EGL-Context für die Taskleiste — sie ist ein einziges texturiertes
  Rechteck, das der Compositor ohnehin schon pro Frame rendert.
- Keine Wayland-IPC-Overhead zwischen Compositor und Panel (kein
  `wlr-layer-shell`-Protokoll-Roundtrip, keine zusätzliche
  Serialisierung von Statusdaten über einen Socket).
- `sway`/`waybar` bringen Funktionsumfang (Tiling-Layouts, Konfigsprache,
  IPC-Protokoll für Drittanbieter-Panels), den Tarno OS nicht braucht —
  jede ungenutzte Funktion ist potenziell mehr RAM/Binärgröße als nötig.
- Nachteil, bewusst in Kauf genommen: kein Ökosystem an fertigen
  Panel-Widgets, keine Nutzer-Konfigurierbarkeit der Taskleiste ohne
  Rust-Änderung. Für ein auf ein einziges Zielgerät (Dell Precision M6700)
  getrimmtes OS mit festem Funktionsumfang ist das akzeptabel.

## Architektur

`tarno-desktop` ist ein [smithay](https://github.com/Smithay/smithay)-basierter
Wayland-Compositor (0.7), analog zu smithays eigenem `examples/minimal.rs`
aufgebaut:

- `CompositorState`, `XdgShellState`, `ShmState`, `SeatState`,
  `DataDeviceState` — Standard-Wayland-Protokollhandler.
- `render_elements!`-Makro (`OutputRenderElement`) kombiniert die
  Client-Fenster-Renderelemente (`WaylandSurfaceRenderElement`) mit dem
  Taskleisten-Textur-Element (`TextureRenderElement`) in einem einzigen
  Draw-Call-Batch — kein Compositing-Zwischenschritt.
- Die Taskleiste (`src/taskbar.rs`) ist **kein Widget-Toolkit**, sondern
  ein flacher RGBA8-Software-Puffer (Hintergrund + Text via
  [`fontdue`](https://github.com/mooman219/fontdue), reiner Rust-Rasterizer,
  keine FreeType/HarfBuzz-Abhängigkeit), der höchstens einmal pro Sekunde
  neu gerendert und als Textur hochgeladen wird (`build_taskbar_element` in
  `src/state.rs`, gedrosselt über `Instant`-Timestamp) — nicht jedes Frame,
  da sich Uhrzeit/Status selten ändern.
- `src/tarnod_client.rs`: Hintergrund-Thread pollt `tarnod` alle 2s über
  `tarnod-protocol` (denselben Unix-Socket wie `tarnod-ui`/`tarnoctl`),
  rein lesend (`GetGamingMode`, `SecurityStatus`) — die Taskleiste zeigt
  live, ob `tarnod` erreichbar ist, ob `isolcpus` aktiv ist und ob der
  eBPF-Wächter läuft.
- Font: [DejaVu Sans Mono](../tarno-desktop/assets/FONT-LICENSE.md)
  (Bitstream-Vera-Ableitung, frei redistribuierbar) statt Segoe UI —
  Windows-11-Systemfonts sind proprietär und dürfen nicht mitverteilt
  werden.

## Entwicklungs-/Test-Backend

`tarno-desktop` läuft in dieser Phase über smithays `winit`-Backend
(genestet in einem bestehenden X11/Wayland-Host-Fenster — funktioniert
unter Xvfb, siehe Verifikation unten). Ein DRM/KMS-Backend für echten
Bare-Metal-Boot auf dem M6700 (ohne Host-Compositor, direkt gegen
`/dev/dri/card0`) ist **Stage 2**, siehe Scope-Hinweis.

**Kritischer Stolperstein** (dokumentiert in `src/state.rs`, da es beim
ersten Anlauf zu einem stillen, fensterlosen Hang führte): `winit::init()`
muss laufen, **bevor** `WAYLAND_DISPLAY` auf den eigenen Socket-Namen
gesetzt wird. Wird die Env-Var vorher gesetzt, versucht winits interne
Host-Fenster-Erstellung, sich selbst als Wayland-**Client** mit dem eigenen,
noch nicht bedienenden Compositor-Socket zu verbinden — ein
Henne-Ei-Deadlock statt des gewünschten X11-Fallbacks.

## Scope: Stage 1 (diese Runde) vs. Stage 2

**Stage 1 — verifiziert in dieser Sandbox (Xvfb + echter laufender `tarnod`
im Dry-Run):**
- Compositor startet, öffnet ein Fenster (winit-Backend), rendert den
  Hintergrund in `tarno-ui-theme::BG_APP`.
- Taskleiste wird fusioniert gerendert: Wordmark, Verbindungsstatus-Punkt
  (grün/rot), `isolcpus`-Status, eBPF-Status, live laufende Uhrzeit —
  alles mit echten Daten von einem tatsächlich laufenden `tarnod`-Prozess
  über den echten Unix-Socket bestätigt (per Screenshot verifiziert, siehe
  `git log` zu diesem Commit für den Verifikationsablauf).
- `ListeningSocket::bind("tarno-desktop-0")` exponiert einen echten
  `WAYLAND_DISPLAY`-Socket, gegen den sich Clients grundsätzlich verbinden
  können (Compositor-Seite ist vollständig verdrahtet: `CompositorState`,
  `XdgShellState`, `ShmState`, Buffer-Handling, Frame-Callbacks).

**Stage 2 — NICHT in dieser Runde verifiziert, ausdrücklich offen:**
- Ein echter XDG-Shell-Client (z. B. `weston-terminal`, ein Browser) wurde
  in dieser Sandbox **nicht** gegen `tarno-desktop-0` verbunden und
  gerendert getestet. Der Code-Pfad dafür existiert (XDG-Toplevel-Handling,
  `render_elements_from_surface_tree`), ist aber nur auf
  Kompiliert-Ebene, nicht auf Laufzeit-Ebene mit einem echten Client
  verifiziert.
- Fenster-Interaktion (Verschieben, Größe ändern, Fokus-Wechsel per Klick
  statt nur Pointer-Motion), Tastatur-Layout-Konfiguration über das
  Nutzer-Profil hinaus.
- DRM/KMS-Backend für Bare-Metal-Boot ohne Host-Compositor.
- Start/Stop-Integration in den Boot-Prozess (welcher Modus — `cage` oder
  `tarno-desktop` — beim Login gestartet wird, und wie zwischen beiden
  gewechselt wird) ist noch nicht in `tarno-br2-external` verdrahtet.

## Verwendung (Entwicklung)

```sh
cd tarno-desktop
cargo build
cargo test               # 8 Tests: Uhrzeit-Format, Text-Rasterizer, Taskleisten-Rendering
XDG_RUNTIME_DIR=/tmp/xdg-runtime ./target/debug/tarno-desktop
```

`XDG_RUNTIME_DIR` muss existieren und `0700` sein, sonst bricht smithay
beim Start ab (`mkdir -p /tmp/xdg-runtime && chmod 0700 /tmp/xdg-runtime`).
