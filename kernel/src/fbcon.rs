// SPDX-License-Identifier: GPL-2.0-or-later
//! Framebuffer text console — a mirror of the serial output.
//!
//! The Acer has no serial port, so without this THOS would be invisible there.
//! Everything written through `serial` is also drawn here with an embedded 8x16
//! PSF1 font. Handles `\n \r \t \b` and the few CSI sequences BusyBox's line
//! editor emits (`K`, `J`, `H`, `C`, `D`); other escapes are swallowed.
//!
//! Writes go straight to the Limine framebuffer address, which is only mapped
//! while Limine's tables are live and again after `gdi::init` re-maps it, so
//! `suspend()`/`resume()` bracket the page-table switch in `kmain`.

use spin::Mutex;

static FONT: &[u8] = include_bytes!("../font/Lat15-Terminus16.psf");
const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;
const FG: u32 = 0x00C8_C8C8;
const BG: u32 = 0x0000_0000;

#[derive(Clone, Copy, PartialEq)]
enum Esc {
    None,
    Esc,
    Csi,
}

struct Con {
    base: *mut u8,
    pitch: usize,
    cols: usize,
    rows: usize,
    cx: usize,
    cy: usize,
    active: bool,
    esc: Esc,
    param: [usize; 2],
    nparam: usize,
    /// UTF-8 decoder: code point so far and continuation bytes still expected.
    cp: u32,
    need: u8,
}

// The framebuffer pointer is only touched under the `CON` lock.
unsafe impl Send for Con {}

static CON: Mutex<Option<Con>> = Mutex::new(None);

/// Take over the framebuffer: clear it to black and start at the top left.
/// Only 32 bpp is supported (what Limine/VBE hands out); otherwise stays off.
pub fn init(fb: &limine::framebuffer::Framebuffer) {
    if fb.bpp != 32 || FONT.len() < 4 + 256 * GLYPH_H || FONT[0] != 0x36 || FONT[1] != 0x04 {
        return;
    }
    let mut c = Con {
        base: fb.address() as *mut u8,
        pitch: fb.pitch as usize,
        cols: fb.width as usize / GLYPH_W,
        rows: fb.height as usize / GLYPH_H,
        cx: 0,
        cy: 0,
        active: true,
        esc: Esc::None,
        param: [0; 2],
        nparam: 0,
        cp: 0,
        need: 0,
    };
    if c.cols == 0 || c.rows == 0 {
        return;
    }
    for y in 0..c.rows * GLYPH_H {
        for x in 0..fb.width as usize {
            c.px(x, y, BG);
        }
    }
    c.cursor(true);
    *CON.lock() = Some(c);
}

/// Stop drawing (the framebuffer mapping is about to change).
pub fn suspend() {
    if let Some(c) = CON.lock().as_mut() {
        c.active = false;
    }
}

/// Resume drawing once the framebuffer is mapped again.
pub fn resume() {
    if let Some(c) = CON.lock().as_mut() {
        c.active = true;
    }
}

pub fn write(bytes: &[u8]) {
    let mut g = CON.lock();
    let Some(c) = g.as_mut() else { return };
    if !c.active {
        return;
    }
    c.cursor(false);
    for &b in bytes {
        c.utf8(b);
    }
    c.cursor(true);
}

impl Con {
    fn px(&mut self, x: usize, y: usize, v: u32) {
        unsafe { core::ptr::write_volatile(self.base.add(y * self.pitch + x * 4) as *mut u32, v) }
    }

    fn glyph(&mut self, col: usize, row: usize, ch: u8) {
        let g = &FONT[4 + ch as usize * GLYPH_H..4 + (ch as usize + 1) * GLYPH_H];
        for (dy, bits) in g.iter().enumerate() {
            for dx in 0..GLYPH_W {
                let on = bits & (0x80 >> dx) != 0;
                self.px(col * GLYPH_W + dx, row * GLYPH_H + dy, if on { FG } else { BG });
            }
        }
    }

    /// Draw/erase the block cursor by inverting its cell.
    fn cursor(&mut self, _show: bool) {
        let (x0, y0) = (self.cx.min(self.cols - 1) * GLYPH_W, self.cy * GLYPH_H);
        for dy in 0..GLYPH_H {
            for dx in 0..GLYPH_W {
                let p = unsafe { self.base.add((y0 + dy) * self.pitch + (x0 + dx) * 4) as *mut u32 };
                unsafe { core::ptr::write_volatile(p, core::ptr::read_volatile(p) ^ 0x00FF_FFFF) };
            }
        }
    }

    fn clear_cells(&mut self, row: usize, from: usize, to: usize) {
        for col in from..to {
            self.glyph(col, row, b' ');
        }
    }

    fn scroll(&mut self) {
        let row_bytes = self.pitch * GLYPH_H;
        unsafe {
            core::ptr::copy(self.base.add(row_bytes), self.base, row_bytes * (self.rows - 1));
        }
        let last = self.rows - 1;
        self.clear_cells(last, 0, self.cols);
    }

    fn newline(&mut self) {
        self.cx = 0;
        if self.cy + 1 >= self.rows {
            self.scroll();
        } else {
            self.cy += 1;
        }
    }

    fn csi_final(&mut self, f: u8) {
        let n = |s: &Self, i: usize| if s.nparam > i { s.param[i] } else { 0 };
        match f {
            b'K' => {
                let (cy, cx, cols) = (self.cy, self.cx, self.cols);
                match n(self, 0) {
                    0 => self.clear_cells(cy, cx, cols),
                    1 => self.clear_cells(cy, 0, (cx + 1).min(cols)),
                    _ => self.clear_cells(cy, 0, cols),
                }
            }
            b'J' => {
                let (cy, cx, cols, rows) = (self.cy, self.cx, self.cols, self.rows);
                match n(self, 0) {
                    0 => {
                        self.clear_cells(cy, cx, cols);
                        for r in cy + 1..rows {
                            self.clear_cells(r, 0, cols);
                        }
                    }
                    _ => {
                        for r in 0..rows {
                            self.clear_cells(r, 0, cols);
                        }
                        self.cx = 0;
                        self.cy = 0;
                    }
                }
            }
            b'H' | b'f' => {
                self.cy = n(self, 0).max(1).saturating_sub(1).min(self.rows - 1);
                self.cx = n(self, 1).max(1).saturating_sub(1).min(self.cols - 1);
            }
            b'C' => self.cx = (self.cx + n(self, 0).max(1)).min(self.cols - 1),
            b'D' => self.cx = self.cx.saturating_sub(n(self, 0).max(1)),
            _ => {} // `m` (colours) and anything else: ignored
        }
    }

    /// Feed one byte of UTF-8; the font is Latin-15, so box drawing and
    /// punctuation fall back to ASCII look-alikes and unknown glyphs to `?`.
    fn utf8(&mut self, b: u8) {
        if b < 0x80 {
            self.need = 0;
            return self.put(b);
        }
        if self.need > 0 && b & 0xC0 == 0x80 {
            self.cp = (self.cp << 6) | (b & 0x3F) as u32;
            self.need -= 1;
            if self.need == 0 {
                let g = fallback(self.cp);
                self.put(g);
            }
            return;
        }
        match b {
            0xC0..=0xDF => (self.cp, self.need) = ((b & 0x1F) as u32, 1),
            0xE0..=0xEF => (self.cp, self.need) = ((b & 0x0F) as u32, 2),
            0xF0..=0xF7 => (self.cp, self.need) = ((b & 0x07) as u32, 3),
            _ => {
                self.need = 0;
                self.put(b'?');
            }
        }
    }

    fn put(&mut self, b: u8) {
        match self.esc {
            Esc::Esc => {
                if b == b'[' {
                    self.esc = Esc::Csi;
                    self.param = [0; 2];
                    self.nparam = 0;
                } else {
                    self.esc = Esc::None;
                }
                return;
            }
            Esc::Csi => {
                match b {
                    b'0'..=b'9' => {
                        if self.nparam == 0 {
                            self.nparam = 1;
                        }
                        let i = self.nparam - 1;
                        self.param[i] = self.param[i].saturating_mul(10) + (b - b'0') as usize;
                    }
                    b';' => self.nparam = (self.nparam.max(1) + 1).min(2),
                    0x40..=0x7E => {
                        self.esc = Esc::None;
                        self.csi_final(b);
                    }
                    _ => {}
                }
                return;
            }
            Esc::None => {}
        }
        match b {
            0x1B => self.esc = Esc::Esc,
            b'\n' => self.newline(),
            b'\r' => self.cx = 0,
            0x08 => self.cx = self.cx.saturating_sub(1),
            b'\t' => {
                self.cx = (self.cx + 8) & !7;
                if self.cx >= self.cols {
                    self.newline();
                }
            }
            0x07 => {}
            _ => {
                if self.cx >= self.cols {
                    self.newline();
                }
                let (cx, cy) = (self.cx, self.cy);
                self.glyph(cx, cy, b);
                self.cx += 1;
            }
        }
    }
}

/// Glyph byte (Latin-15 slot) for a Unicode code point.
fn fallback(cp: u32) -> u8 {
    match cp {
        0xA0..=0xFF => cp as u8, // Latin-1 range: same slots as Latin-15 for the common letters
        0x2500 | 0x2501 | 0x2504..=0x2509 | 0x254C..=0x254F => b'-',
        0x2502 | 0x2503 | 0x250A..=0x250B | 0x2551 => b'|',
        0x2550 => b'=',
        0x2500..=0x257F => b'+',
        0x2010..=0x2015 => b'-',
        0x2018 | 0x2019 => b'\'',
        0x201C | 0x201D => b'"',
        0x2022 => b'*',
        0x2026 => b'.',
        0x2190 => b'<',
        0x2192 => b'>',
        0x20AC => 0xA4, // euro sign in Latin-15
        _ => b'?',
    }
}
