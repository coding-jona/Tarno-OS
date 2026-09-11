// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — GDI32/User32: the real framebuffer, now window-relative.
//!
//! `GetDC(0)` still hands back the fixed screen DC (`1`) — draw in screen
//! coordinates, clipped only to the screen. `GetDC(hwnd)` for a real window
//! (`crate::window`) hands back a DC tagged with that `hwnd`
//! ([`WINDOW_DC_TAG`]); every draw through it is in *client* coordinates —
//! offset by the window's `(x, y)` and clipped to its rect intersected with
//! the screen — exactly like real Win32. Still no compositor (windows don't
//! occlude each other, there's no z-order) and every DC still just carries
//! one current brush colour, no real GDI object table.

use alloc::collections::BTreeMap;

use spin::{Mutex, Once};

struct FbInfo {
    virt: u64,
    width: u32,
    height: u32,
    pitch: u32,
    r_shift: u8,
    g_shift: u8,
    b_shift: u8,
}

static FB: Once<FbInfo> = Once::new();

/// Tag bit OR-ed into a window's `hwnd` to make a `GetDC(hwnd)` handle —
/// distinguishes a window DC from the fixed screen DC (`1`) in every
/// drawing call. `hwnd`s are small (`window::NEXT_HWND` starts at 1), so
/// this bit is always free.
pub const WINDOW_DC_TAG: u64 = 0x8000_0000;

/// Each DC's current brush colour (`COLORREF`), keyed by the DC handle
/// itself; defaults to white the first time a DC is drawn through or
/// selected into. No real GDI object table — a brush "handle" is just a
/// colour (see [`create_solid_brush`]), so there is nothing else to store.
static BRUSHES: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());

pub const SM_CXSCREEN: i64 = 0;
pub const SM_CYSCREEN: i64 = 1;

/// Map the boot framebuffer into THOS's own page tables — `vmm::map_mmio`
/// derives the virtual address from the same HHDM offset Limine used for
/// `fb.address()`, so this lands at the identical VA, just backed by THOS's
/// own PTEs — and record its geometry. Call once, after `vmm::init` (needs
/// `vmm::kernel_pml4_phys`). Only 32bpp is supported so far — that is what
/// every target THOS boots on today (QEMU, real hardware EFI GOP) reports.
pub fn init(fb: &limine::framebuffer::Framebuffer, hhdm: u64) {
    if fb.bpp != 32 {
        crate::kprintln!("THOS: gdi FAIL         framebuffer is {}bpp, only 32bpp supported", fb.bpp);
        return;
    }
    let phys = fb.address() as u64 - hhdm;
    let len = fb.pitch * fb.height;
    let virt = crate::vmm::map_mmio(phys, len);
    FB.call_once(|| FbInfo {
        virt,
        width: fb.width as u32,
        height: fb.height as u32,
        pitch: fb.pitch as u32,
        r_shift: fb.red_mask_shift,
        g_shift: fb.green_mask_shift,
        b_shift: fb.blue_mask_shift,
    });
    crate::kprintln!(
        "THOS: gdi ok           {}x{} @ 32bpp mapped for GDI32/User32",
        fb.width, fb.height
    );
}

fn fb() -> Option<&'static FbInfo> {
    FB.get()
}

/// `GetSystemMetrics(SM_CXSCREEN|SM_CYSCREEN)`'s backing data.
pub fn screen_size() -> (u32, u32) {
    fb().map_or((0, 0), |f| (f.width, f.height))
}

/// `COLORREF` (`0x00BBGGRR`) → this framebuffer's native 32-bit pixel.
fn pack(colorref: u32) -> u32 {
    let Some(f) = fb() else { return 0 };
    let r = colorref & 0xFF;
    let g = (colorref >> 8) & 0xFF;
    let b = (colorref >> 16) & 0xFF;
    (r << f.r_shift) | (g << f.g_shift) | (b << f.b_shift)
}

/// The inverse of [`pack`] — native pixel → `COLORREF`, for `GetPixel`.
fn unpack(native: u32) -> u32 {
    let Some(f) = fb() else { return 0 };
    let r = (native >> f.r_shift) & 0xFF;
    let g = (native >> f.g_shift) & 0xFF;
    let b = (native >> f.b_shift) & 0xFF;
    r | (g << 8) | (b << 16)
}

fn ptr_at(x: u32, y: u32) -> Option<*mut u32> {
    let f = fb()?;
    if x >= f.width || y >= f.height {
        return None;
    }
    Some((f.virt + y as u64 * f.pitch as u64 + x as u64 * 4) as *mut u32)
}

/// A DC's resolved drawing bounds: where its own `(0, 0)` lands on screen,
/// and the screen rectangle drawing through it must stay inside (already
/// intersected with the screen, so callers never need to check separately).
struct DcBounds {
    origin: (i32, i32),
    clip: (i32, i32, i32, i32), // left, top, right, bottom — screen coords, exclusive
}

fn resolve_dc(dc: u64) -> DcBounds {
    let (w, h) = screen_size();
    let screen = || DcBounds { origin: (0, 0), clip: (0, 0, w as i32, h as i32) };
    if dc & WINDOW_DC_TAG == 0 {
        return screen();
    }
    let hwnd = (dc & !WINDOW_DC_TAG) as u32;
    match crate::window::rect_of(hwnd) {
        // A window's own client rect, clamped to the screen — a window
        // partially (or fully) off-screen just clips there, like real GDI.
        Some((x, y, cw, ch)) => DcBounds {
            origin: (x, y),
            clip: (x.max(0), y.max(0), (x + cw).min(w as i32), (y + ch).min(h as i32)),
        },
        // A destroyed/unknown hwnd: fall back to the screen rather than a
        // DC that can never draw anything.
        None => screen(),
    }
}

/// `SetPixel(hdc, x, y, colorref)` — `x`/`y` are client-relative for a
/// window DC. `0xFFFF_FFFF` (`CLR_INVALID`) outside the DC's clip rect.
pub fn set_pixel(dc: u64, x: i64, y: i64, colorref: u32) -> u32 {
    let b = resolve_dc(dc);
    let (sx, sy) = (b.origin.0 as i64 + x, b.origin.1 as i64 + y);
    let (l, t, r, bot) = b.clip;
    if sx < l as i64 || sy < t as i64 || sx >= r as i64 || sy >= bot as i64 {
        return u32::MAX;
    }
    match ptr_at(sx as u32, sy as u32) {
        Some(p) => {
            unsafe { p.write_volatile(pack(colorref)) };
            colorref
        }
        None => u32::MAX,
    }
}

/// `GetPixel(hdc, x, y)` — the inverse of [`set_pixel`]'s coordinate mapping.
pub fn get_pixel(dc: u64, x: i64, y: i64) -> u32 {
    let b = resolve_dc(dc);
    let (sx, sy) = (b.origin.0 as i64 + x, b.origin.1 as i64 + y);
    let (l, t, r, bot) = b.clip;
    if sx < l as i64 || sy < t as i64 || sx >= r as i64 || sy >= bot as i64 {
        return u32::MAX;
    }
    match ptr_at(sx as u32, sy as u32) {
        Some(p) => unpack(unsafe { p.read_volatile() }),
        None => u32::MAX,
    }
}

/// `Rectangle(hdc, left, top, right, bottom)`: fill `[left,right) x
/// [top,bottom)`, client-relative for a window DC, with the DC's current
/// brush colour, clamped to its clip rect. `false` if the clamped rectangle
/// is empty — otherwise `true`, matching real GDI's BOOL.
pub fn fill_rect(dc: u64, left: i64, top: i64, right: i64, bottom: i64) -> bool {
    if fb().is_none() {
        return false;
    }
    let b = resolve_dc(dc);
    let color = pack(brush_of(dc));
    let (ox, oy) = (b.origin.0 as i64, b.origin.1 as i64);
    let (cl, ct, cr, cb) = b.clip;
    let l = (ox + left).max(cl as i64).max(0) as u32;
    let t = (oy + top).max(ct as i64).max(0) as u32;
    let r = ((ox + right).min(cr as i64).max(0)) as u32;
    let bot = ((oy + bottom).min(cb as i64).max(0)) as u32;
    if l >= r || t >= bot {
        return false;
    }
    let f = fb().unwrap();
    for y in t..bot {
        let row = (f.virt + y as u64 * f.pitch as u64) as *mut u32;
        for x in l..r {
            unsafe { row.add(x as usize).write_volatile(color) };
        }
    }
    true
}

fn brush_of(dc: u64) -> u32 {
    *BRUSHES.lock().entry(dc).or_insert(0x00FF_FFFF) // default: white
}

/// A brush "handle" is just its colour with a tag bit — there is no real GDI
/// object table yet (nothing to look up beyond a DC's current colour).
const BRUSH_TAG: u64 = 0x9000_0000;

/// `CreateSolidBrush(colorref)`.
pub fn create_solid_brush(colorref: u32) -> u64 {
    BRUSH_TAG | colorref as u64
}

/// `GetStockObject(i)` — only `WHITE_BRUSH` (0) and `BLACK_BRUSH` (4), the
/// pair worth having this early; anything else also comes back white.
pub fn get_stock_object(i: i64) -> u64 {
    create_solid_brush(if i == 4 { 0x0000_0000 } else { 0x00FF_FFFF })
}

/// `SelectObject(hdc, hbrush)`: set `hdc`'s brush, return the previous one
/// (also brush-tagged, like real GDI returning the previous object).
pub fn select_object(dc: u64, hobj: u64) -> u64 {
    let color = (hobj & 0x00FF_FFFF) as u32;
    let old = BRUSHES.lock().insert(dc, color).unwrap_or(0x00FF_FFFF);
    BRUSH_TAG | old as u64
}
