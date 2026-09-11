// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — GDI32/User32 skeleton: one real framebuffer, no windows yet.
//!
//! There is no window manager, no compositor, no WndProc callback mechanism
//! (that needs a way to call back into ring-3 code mid-syscall — a real chunk
//! of work, the next increment). What this gives instead is the actual pixel
//! plumbing: the boot framebuffer Limine handed us, mapped into THOS's own
//! page tables and reachable from the NT personality, plus just enough of a
//! "device context" model (one DC, the whole screen; a brush is just a
//! colour) for `GetDC`/`SetPixel`/`GetPixel`/`Rectangle`/`GetSystemMetrics`
//! to do the real thing. Everything here is plain Rust, not syscall-shaped —
//! `nt::dispatch_gdi32`/`dispatch_user32` are the thin syscall skin on top.

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

/// The one and only DC's current fill colour (`COLORREF`, `0x00BBGGRR`).
/// `GetDC` always returns the fixed handle `1` — there is nothing to look up
/// yet, since there is only one DC.
static CURRENT_BRUSH: Mutex<u32> = Mutex::new(0x00FF_FFFF); // white

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

/// `SetPixel(hdc, x, y, colorref)`. `0xFFFF_FFFF` (`CLR_INVALID`) off-screen.
pub fn set_pixel(x: i64, y: i64, colorref: u32) -> u32 {
    if x < 0 || y < 0 {
        return u32::MAX;
    }
    match ptr_at(x as u32, y as u32) {
        Some(p) => {
            unsafe { p.write_volatile(pack(colorref)) };
            colorref
        }
        None => u32::MAX,
    }
}

/// `GetPixel(hdc, x, y)`. `0xFFFF_FFFF` (`CLR_INVALID`) off-screen.
pub fn get_pixel(x: i64, y: i64) -> u32 {
    if x < 0 || y < 0 {
        return u32::MAX;
    }
    match ptr_at(x as u32, y as u32) {
        Some(p) => unpack(unsafe { p.read_volatile() }),
        None => u32::MAX,
    }
}

/// `Rectangle(hdc, left, top, right, bottom)`: fill `[left,right) x [top,bottom)`
/// with the DC's current brush colour, clamped to the screen. `false` if the
/// (clamped) rectangle is empty — otherwise `true`, matching real GDI's BOOL.
pub fn fill_rect(left: i64, top: i64, right: i64, bottom: i64) -> bool {
    let Some(f) = fb() else { return false };
    let color = pack(*CURRENT_BRUSH.lock());
    let l = left.max(0) as u32;
    let t = top.max(0) as u32;
    let r = (right.max(0) as u32).min(f.width);
    let b = (bottom.max(0) as u32).min(f.height);
    if l >= r || t >= b {
        return false;
    }
    for y in t..b {
        let row = (f.virt + y as u64 * f.pitch as u64) as *mut u32;
        for x in l..r {
            unsafe { row.add(x as usize).write_volatile(color) };
        }
    }
    true
}

/// A brush "handle" is just its colour with a tag bit — there is no real GDI
/// object table yet (nothing to look up: one DC, one current brush).
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

/// `SelectObject(hdc, hbrush)`: set the DC's brush, return the previous one
/// (also brush-tagged, like real GDI returning the previous object).
pub fn select_object(hobj: u64) -> u64 {
    let color = (hobj & 0x00FF_FFFF) as u32;
    let mut b = CURRENT_BRUSH.lock();
    let old = *b;
    *b = color;
    BRUSH_TAG | old as u64
}
