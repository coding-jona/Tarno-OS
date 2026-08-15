//! Minimaler Text-Renderer für die Taskleiste: rasterisiert Text direkt in
//! einen RGBA8-Pixelpuffer (Software, via `fontdue`) statt über eine
//! GPU-Text-Shaping-Pipeline (kein Pango/HarfBuzz/Fontconfig) — die
//! Taskleiste hat wenige, kurze, meist statische Textelemente (Uhrzeit,
//! ein bis zwei Status-Zeilen), da lohnt sich der große Font-Stack nicht.
//! Eingebetteter Font: DejaVu Sans Mono, siehe `assets/FONT-LICENSE.md`.

use fontdue::Font;

pub struct TextRenderer {
    font: Font,
}

impl TextRenderer {
    pub fn new() -> Self {
        let bytes = include_bytes!("../assets/DejaVuSansMono.ttf");
        let font = Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
            .expect("eingebetteter Font DejaVuSansMono.ttf muss parsbar sein");
        Self { font }
    }

    /// Zeichnet `text` in `size` Pixel Höhe, mit `(x, y)` als linker
    /// Baseline-Punkt, in `buf` (RGBA8, `width`×`height`, straight alpha).
    /// Deckkraft-Blending gegen den vorhandenen (als opak angenommenen)
    /// Hintergrund in `buf`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        buf: &mut [u8],
        width: usize,
        height: usize,
        x: i32,
        y: i32,
        text: &str,
        size: f32,
        color: [u8; 3],
    ) {
        let mut cursor_x = x as f32;
        for ch in text.chars() {
            let (metrics, bitmap) = self.font.rasterize(ch, size);
            let glyph_x0 = cursor_x.round() as i32 + metrics.xmin;
            let glyph_y0 = y - metrics.height as i32 - metrics.ymin;

            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let coverage = bitmap[gy * metrics.width + gx];
                    if coverage == 0 {
                        continue;
                    }
                    let px = glyph_x0 + gx as i32;
                    let py = glyph_y0 + gy as i32;
                    if px < 0 || py < 0 || px as usize >= width || py as usize >= height {
                        continue;
                    }
                    let idx = (py as usize * width + px as usize) * 4;
                    let a = f32::from(coverage) / 255.0;
                    for c in 0..3 {
                        let bg = f32::from(buf[idx + c]);
                        let fg = f32::from(color[c]);
                        buf[idx + c] = (fg * a + bg * (1.0 - a)).round() as u8;
                    }
                    buf[idx + 3] = 255;
                }
            }
            cursor_x += metrics.advance_width;
        }
    }

    /// Breite, die `text` bei `size` einnehmen würde (für Rechtsbündig-Ausrichtung).
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        text.chars()
            .map(|c| self.font.metrics(c, size).advance_width)
            .sum()
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_embedded_font() {
        let _ = TextRenderer::new();
    }

    #[test]
    fn draws_text_without_panicking_and_touches_pixels() {
        let renderer = TextRenderer::new();
        let (w, h) = (200usize, 40usize);
        let mut buf = vec![0u8; w * h * 4];
        renderer.draw(&mut buf, w, h, 10, 25, "tarno 12:34", 15.0, [255, 255, 255]);
        // Mindestens ein Pixel muss durch die Glyphen verändert worden sein.
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn text_width_grows_with_length() {
        let renderer = TextRenderer::new();
        let short = renderer.text_width("ab", 15.0);
        let long = renderer.text_width("abcdef", 15.0);
        assert!(long > short);
    }

    #[test]
    fn out_of_bounds_position_does_not_panic() {
        let renderer = TextRenderer::new();
        let (w, h) = (10usize, 10usize);
        let mut buf = vec![0u8; w * h * 4];
        // Bewusst weit außerhalb des Puffers zeichnen -> darf nicht crashen.
        renderer.draw(&mut buf, w, h, -500, -500, "x", 15.0, [255, 255, 255]);
        renderer.draw(&mut buf, w, h, 5000, 5000, "x", 15.0, [255, 255, 255]);
    }
}
