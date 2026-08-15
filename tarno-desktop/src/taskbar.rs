//! Komposition der Taskleiste: rendert Hintergrund + Uhrzeit + `tarnod`-
//! Status in einen einzelnen RGBA8-Software-Puffer, der pro Frame (nur bei
//! Änderung, siehe `state.rs`) als eine einzelne Textur hochgeladen wird —
//! kein separates Widget-Toolkit, keine Retained-Mode-Szene, nur ein
//! flacher Pixelpuffer für ein einziges, immer gleich aufgebautes Element.

use tarno_ui_theme::Color32;

use crate::clock::current_time_hh_mm_ss;
use crate::tarnod_client::TarnodStatus;
use crate::text::TextRenderer;

pub const HEIGHT: u32 = 40;

pub struct Taskbar {
    text: TextRenderer,
}

impl Taskbar {
    pub fn new() -> Self {
        Self {
            text: TextRenderer::new(),
        }
    }

    /// Rendert die komplette Taskleiste für die gegebene Fensterbreite neu.
    pub fn render(&self, width: u32, status: &TarnodStatus) -> Vec<u8> {
        let w = width.max(1) as usize;
        let h = HEIGHT as usize;
        let mut buf = vec![0u8; w * h * 4];

        fill_bg(&mut buf, tarno_ui_theme::BG_PANEL);
        draw_top_highlight(&mut buf, w);

        let accent = tarno_ui_theme::ACCENT;
        self.text
            .draw(&mut buf, w, h, 14, 25, "TARNO", 17.0, rgb(accent));

        let (dot_color, status_text): (Color32, &str) = if status.connected {
            (tarno_ui_theme::SUCCESS, "verbunden")
        } else {
            (tarno_ui_theme::DANGER, "getrennt")
        };
        draw_dot(&mut buf, w, 100, 20, 4, dot_color);
        let muted = tarno_ui_theme::TEXT_MUTED;
        self.text
            .draw(&mut buf, w, h, 112, 25, &format!("tarnod {status_text}"), 13.0, rgb(muted));

        let mut next_x = 260;
        if let Some(cpus) = &status.isolated_cpus {
            if !cpus.is_empty() {
                let success = tarno_ui_theme::SUCCESS;
                let label = format!("isolcpus {cpus}");
                self.text.draw(&mut buf, w, h, next_x, 25, &label, 13.0, rgb(success));
                next_x += self.text.text_width(&label, 13.0) as i32 + 16;
            }
        }
        if let Some(ebpf_active) = status.ebpf_active {
            let (color, label) = if ebpf_active {
                (tarno_ui_theme::SUCCESS, "eBPF aktiv")
            } else {
                (tarno_ui_theme::TEXT_MUTED, "eBPF inaktiv")
            };
            self.text.draw(&mut buf, w, h, next_x, 25, label, 13.0, rgb(color));
        }

        let clock_text = current_time_hh_mm_ss();
        let clock_width = self.text.text_width(&clock_text, 16.0);
        let primary = tarno_ui_theme::TEXT_PRIMARY;
        self.text.draw(
            &mut buf,
            w,
            h,
            width as i32 - clock_width as i32 - 16,
            25,
            &clock_text,
            16.0,
            rgb(primary),
        );

        buf
    }
}

impl Default for Taskbar {
    fn default() -> Self {
        Self::new()
    }
}

fn rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

fn fill_bg(buf: &mut [u8], color: Color32) {
    for px in buf.chunks_exact_mut(4) {
        px[0] = color.r();
        px[1] = color.g();
        px[2] = color.b();
        px[3] = 255;
    }
}

/// Selbe billige "Glass"-Andeutung wie in `tarno-ui-theme::card` — ein
/// dünner, halbtransparenter Strich statt echtem Blur.
fn draw_top_highlight(buf: &mut [u8], width: usize) {
    for x in 0..width {
        let idx = x * 4;
        buf[idx] = 255;
        buf[idx + 1] = 255;
        buf[idx + 2] = 255;
        buf[idx + 3] = 40;
    }
}

fn draw_dot(buf: &mut [u8], width: usize, cx: i32, cy: i32, radius: i32, color: Color32) {
    let h = buf.len() / 4 / width;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            let x = cx + dx;
            let y = cy + dy;
            if x < 0 || y < 0 || x as usize >= width || y as usize >= h {
                continue;
            }
            let idx = (y as usize * width + x as usize) * 4;
            buf[idx] = color.r();
            buf[idx + 1] = color.g();
            buf[idx + 2] = color.b();
            buf[idx + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_produces_correctly_sized_buffer() {
        let taskbar = Taskbar::new();
        let status = TarnodStatus::default();
        let buf = taskbar.render(800, &status);
        assert_eq!(buf.len(), 800 * HEIGHT as usize * 4);
    }

    #[test]
    fn render_handles_zero_width_without_panicking() {
        let taskbar = Taskbar::new();
        let status = TarnodStatus::default();
        let _ = taskbar.render(0, &status);
    }

    #[test]
    fn connected_status_renders_differently_from_disconnected() {
        let taskbar = Taskbar::new();
        let connected = TarnodStatus {
            connected: true,
            isolated_cpus: None,
            ebpf_active: None,
        };
        let disconnected = TarnodStatus::default();
        let buf_a = taskbar.render(400, &connected);
        let buf_b = taskbar.render(400, &disconnected);
        assert_ne!(buf_a, buf_b, "verbunden/getrennt muss sich visuell unterscheiden");
    }
}
