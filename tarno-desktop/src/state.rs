//! Compositor-State und Render-Loop.
//!
//! Struktur/Wayland-Protokoll-Verdrahtung folgt eng dem offiziellen
//! smithay-`minimal`-Beispiel (winit-Backend, XDG-Shell, SHM, Seat) — das
//! ist der von smithay selbst dokumentierte, korrekte Weg, einen
//! Compositor mit Client-Fenster-Unterstützung aufzusetzen; hier
//! zusätzlich um die direkt eingebaute Taskleiste erweitert (siehe
//! `taskbar.rs`).
//!
//! **Stage-1-Scope** (siehe docs/month-desktop.md): läuft über das
//! winit-Backend (genestet in einem bestehenden X11/Wayland-Fenster —
//! funktioniert unter Xvfb, siehe dortige Scope-Notiz). Ein echter
//! DRM/KMS-Backend-Pfad für Bare-Metal-Boot auf dem M6700 (kein
//! Eltern-Compositor vorhanden) ist Stage-2-Arbeit.

use std::os::unix::io::OwnedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, InputEvent, KeyboardKeyEvent, MouseButton, PointerButtonEvent,
};
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::delegate_compositor;
use smithay::delegate_data_device;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_xdg_shell;
use smithay::input::keyboard::FilterResult;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::{self, WlSurface};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::utils::{Rectangle, Serial, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_surface_tree_downward, CompositorClientState, CompositorHandler, CompositorState,
    SurfaceAttributes, TraversalAction,
};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::tarnod_client::{self, SharedStatus};
use crate::taskbar::{Taskbar, HEIGHT as TASKBAR_HEIGHT};

/// Wayland-Socket-Name, unter dem `tarno-desktop` erreichbar ist. Clients
/// verbinden sich per `WAYLAND_DISPLAY=<name>`.
const SOCKET_NAME: &str = "tarno-desktop-0";

struct TarnoDesktop {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,

    taskbar: Taskbar,
    tarnod_status: SharedStatus,
    /// Zwischengespeicherte Taskleisten-Textur + die Breite, für die sie
    /// gerendert wurde — nur neu rastern, wenn sich die Fensterbreite
    /// ändert oder eine Sekunde vergangen ist (Uhrzeit), nicht jeden
    /// Frame. Performance-Grund, siehe Modul-Kommentar in `taskbar.rs`.
    taskbar_texture: Option<(TextureBuffer<smithay::backend::renderer::gles::GlesTexture>, i32)>,
    taskbar_rendered_at: Option<Instant>,

    /// Letzte bekannte Zeigerposition in Bildschirmkoordinaten (aus
    /// `PointerMotionAbsolute`) — für den Klick-Hittest gegen die
    /// Settings-Zone der Taskleiste, siehe `handle_pointer_button`.
    pointer_pos: (f64, f64),
    /// Aktuelle Bildschirmgröße, für denselben Hittest.
    output_size: (i32, i32),
    /// Prozess-Handle der gespawnten Settings-App (`tarnod-ui`), falls
    /// gerade offen — verhindert doppeltes Spawnen bei mehrfachem Klick.
    settings_child: Option<std::process::Child>,
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl TarnoDesktop {
    /// Hittest der zuletzt bekannten Zeigerposition gegen die
    /// Settings-Zone der Taskleiste (linke Wordmark, siehe
    /// `taskbar::SETTINGS_HIT_WIDTH`) und Spawnen/Fokussieren von
    /// `tarnod-ui` als echtem Wayland-Client dieses Compositors.
    fn handle_settings_click(&mut self) {
        if !is_in_settings_zone(self.pointer_pos, self.output_size.1) {
            return;
        }
        self.launch_settings();
    }

    fn launch_settings(&mut self) {
        // Schon offen (Prozess läuft noch)? Dann nichts tun — kein
        // zweites Fenster pro Klick. `try_wait()` gibt `Ok(None)` zurück,
        // solange der Kindprozess noch läuft.
        if let Some(child) = &mut self.settings_child {
            match child.try_wait() {
                Ok(None) => return,
                _ => self.settings_child = None,
            }
        }

        let bin = std::env::var("TARNO_DESKTOP_SETTINGS_BIN").unwrap_or_else(|_| "tarnod-ui".to_string());
        // WAYLAND_DISPLAY ist im eigenen Prozess-Environment schon auf
        // unseren Socket gesetzt (siehe run_inner) und wird an das Kind
        // vererbt. WINIT_UNIX_BACKEND=wayland erzwingt zusätzlich, dass
        // eframes winit-Backend Wayland statt X11 wählt, falls (wie im
        // Xvfb-Testaufbau) zusätzlich noch DISPLAY gesetzt ist —
        // ansonsten würde winit X11 bevorzugen und tarnod-ui liefe als
        // eigenständiges Fenster statt als Client dieses Compositors.
        match std::process::Command::new(&bin)
            .env("WINIT_UNIX_BACKEND", "wayland")
            .env_remove("DISPLAY")
            .spawn()
        {
            Ok(child) => self.settings_child = Some(child),
            Err(e) => eprintln!("tarno-desktop: Settings-App '{bin}' konnte nicht gestartet werden: {e}"),
        }
    }
}

impl BufferHandler for TarnoDesktop {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for TarnoDesktop {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {}
}

impl SelectionHandler for TarnoDesktop {
    type SelectionUserData = ();
}

impl DataDeviceHandler for TarnoDesktop {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for TarnoDesktop {}
impl ServerDndGrabHandler for TarnoDesktop {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl CompositorHandler for TarnoDesktop {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
    }
}

impl ShmHandler for TarnoDesktop {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for TarnoDesktop {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: smithay::input::pointer::CursorImageStatus) {}
}

delegate_xdg_shell!(TarnoDesktop);
delegate_compositor!(TarnoDesktop);
delegate_shm!(TarnoDesktop);
delegate_seat!(TarnoDesktop);
delegate_data_device!(TarnoDesktop);

// Vereint Client-Fenster-Elemente (XDG-Toplevels) und unsere eigene
// Taskleisten-Textur in einer gemeinsamen Render-Liste. Offizielles
// smithay-Makro statt handgeschriebener `Element`/`RenderElement`-Impls
// (siehe `smithay::desktop::space::SpaceRenderElements` für dasselbe Muster).
smithay::backend::renderer::element::render_elements! {
    OutputRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Taskbar=TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>,
}

pub fn run() {
    if let Err(e) = run_inner() {
        eprintln!("tarno-desktop: fataler Fehler: {e}");
        std::process::exit(1);
    }
}

fn run_inner() -> Result<(), Box<dyn std::error::Error>> {
    let mut display: Display<TarnoDesktop> = Display::new()?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<TarnoDesktop>(&dh);
    let shm_state = ShmState::new::<TarnoDesktop>(&dh, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "tarno-seat");

    let tarnod_status = tarnod_client::spawn();

    let mut state = TarnoDesktop {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<TarnoDesktop>(&dh),
        shm_state,
        seat_state,
        data_device_state: DataDeviceState::new::<TarnoDesktop>(&dh),
        seat,
        taskbar: Taskbar::new(),
        tarnod_status,
        taskbar_texture: None,
        taskbar_rendered_at: None,
        pointer_pos: (0.0, 0.0),
        output_size: (0, 0),
        settings_child: None,
    };

    let listener = ListeningSocket::bind(SOCKET_NAME)?;
    let mut clients = Vec::new();

    // WICHTIG: winit::init() MUSS vor dem Setzen von WAYLAND_DISPLAY
    // laufen. winit::init() öffnet unser Host-Fenster (hier: X11 unter
    // Xvfb) — würde WAYLAND_DISPLAY schon auf unseren eigenen Socket
    // zeigen, versucht winit stattdessen ÜBER Wayland zu verbinden und
    // hängt sich an unserem eigenen, noch nicht laufenden Compositor auf
    // (der Socket existiert zwar, aber niemand ruft vor der Event-Loop
    // unten `listener.accept()` auf).
    let (mut backend, mut winit_input) = winit::init::<GlesRenderer>()?;

    std::env::set_var("WAYLAND_DISPLAY", SOCKET_NAME);
    eprintln!("tarno-desktop: WAYLAND_DISPLAY={SOCKET_NAME}");

    let start_time = Instant::now();
    let keyboard = state.seat.add_keyboard(Default::default(), 200, 200)?;

    loop {
        let current_size = backend.window_size();
        state.output_size = (current_size.w, current_size.h);

        let status = winit_input.dispatch_new_events(|event| match event {
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                keyboard.input::<(), _>(&mut state, event.key_code(), event.state(), 0.into(), 0, |_, _, _| {
                    FilterResult::Forward
                });
            }
            WinitEvent::Input(InputEvent::PointerMotionAbsolute { event }) => {
                let logical = smithay::utils::Size::from((state.output_size.0, state.output_size.1));
                let pos = event.position_transformed(logical);
                state.pointer_pos = (pos.x, pos.y);

                if let Some(surface) = state.xdg_shell_state.toplevel_surfaces().first().cloned() {
                    let surface = surface.wl_surface().clone();
                    keyboard.set_focus(&mut state, Some(surface), 0.into());
                }
            }
            WinitEvent::Input(InputEvent::PointerButton { event }) => {
                if event.state() == ButtonState::Pressed && event.button() == Some(MouseButton::Left) {
                    state.handle_settings_click();
                }
            }
            _ => {}
        });

        match status {
            smithay::reexports::winit::platform::pump_events::PumpStatus::Continue => {}
            smithay::reexports::winit::platform::pump_events::PumpStatus::Exit(_) => return Ok(()),
        }

        let size = current_size;
        let damage = Rectangle::from_size(size);

        {
            let (renderer, mut framebuffer) = backend.bind()?;

            // Client-Fenster (XDG-Toplevels) zuerst — Taskleiste kommt
            // danach und liegt damit immer sichtbar obenauf. Echte
            // Platzreservierung (Fenster weichen der Taskleiste aus) ist
            // Stage-2-Arbeit, siehe Modul-Kommentar.
            let mut elements: Vec<OutputRenderElement> = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .flat_map(|surface| {
                    render_elements_from_surface_tree::<_, OutputRenderElement>(
                        renderer,
                        surface.wl_surface(),
                        (0, 0),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect();

            let taskbar_element = build_taskbar_element(&mut state, renderer, size.w, size.h);
            elements.push(OutputRenderElement::from(taskbar_element));

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            let bg = tarno_ui_theme::BG_APP;
            frame.clear(
                Color32F::new(
                    f32::from(bg.r()) / 255.0,
                    f32::from(bg.g()) / 255.0,
                    f32::from(bg.b()) / 255.0,
                    1.0,
                ),
                &[damage],
            )?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            let _ = frame.finish()?;

            for surface in state.xdg_shell_state.toplevel_surfaces() {
                send_frames_surface_tree(surface.wl_surface(), start_time.elapsed().as_millis() as u32);
            }

            if let Some(stream) = listener.accept()? {
                let client = display.handle().insert_client(stream, Arc::new(ClientState::default()))?;
                clients.push(client);
            }

            display.dispatch_clients(&mut state)?;
            display.flush_clients()?;
        }

        backend.submit(Some(&[damage]))?;
    }
}

/// Rendert die Taskleiste (bei Bedarf neu, siehe `taskbar_rendered_at`) und
/// lädt sie als GL-Textur hoch, positioniert am unteren Bildschirmrand.
fn build_taskbar_element(
    state: &mut TarnoDesktop,
    renderer: &mut GlesRenderer,
    output_width: i32,
    output_height: i32,
) -> TextureRenderElement<smithay::backend::renderer::gles::GlesTexture> {
    let needs_refresh = match (&state.taskbar_texture, state.taskbar_rendered_at) {
        (Some((_, w)), Some(at)) => *w != output_width || at.elapsed() >= Duration::from_secs(1),
        _ => true,
    };

    if needs_refresh {
        let status = state.tarnod_status.lock().expect("tarnod status mutex poisoned").clone();
        let pixels = state.taskbar.render(output_width.max(1) as u32, &status);
        if let Ok(buffer) = TextureBuffer::from_memory(
            renderer,
            &pixels,
            Fourcc::Abgr8888,
            (output_width.max(1), TASKBAR_HEIGHT as i32),
            false,
            1,
            Transform::Normal,
            None,
        ) {
            state.taskbar_texture = Some((buffer, output_width));
            state.taskbar_rendered_at = Some(Instant::now());
        }
    }

    let (buffer, _) = state
        .taskbar_texture
        .as_ref()
        .expect("Taskleisten-Textur wird oben bei Bedarf immer gesetzt");
    let y = f64::from((output_height - TASKBAR_HEIGHT as i32).max(0));
    TextureRenderElement::from_texture_buffer(
        (0.0, y),
        buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
}

fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

/// Reine Hittest-Funktion (kein Compositor-State nötig) für die
/// Settings-Zone der Taskleiste: unten links, `SETTINGS_HIT_WIDTH` breit,
/// `TASKBAR_HEIGHT` hoch. Ausgelagert aus `handle_settings_click`, damit
/// sich der Klickbereich ohne laufenden Compositor testen lässt.
fn is_in_settings_zone(pointer: (f64, f64), output_height: i32) -> bool {
    let (px, py) = pointer;
    let taskbar_top = f64::from(output_height) - f64::from(TASKBAR_HEIGHT);
    let in_taskbar = py >= taskbar_top && py <= f64::from(output_height);
    let in_settings_zone = px >= 0.0 && px <= f64::from(crate::taskbar::SETTINGS_HIT_WIDTH);
    in_taskbar && in_settings_zone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_inside_wordmark_hits_settings_zone() {
        assert!(is_in_settings_zone((40.0, 780.0), 800));
    }

    #[test]
    fn click_outside_taskbar_misses() {
        assert!(!is_in_settings_zone((40.0, 400.0), 800));
    }

    #[test]
    fn click_in_taskbar_but_right_of_wordmark_misses() {
        assert!(!is_in_settings_zone((500.0, 780.0), 800));
    }

    #[test]
    fn click_at_zone_edges_hits() {
        assert!(is_in_settings_zone((0.0, 760.0), 800));
        assert!(is_in_settings_zone(
            (f64::from(crate::taskbar::SETTINGS_HIT_WIDTH), 800.0),
            800
        ));
    }
}
