// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — a minimal PE32+ (x86-64) loader.
//!
//! Enough to map a **statically linked** Win64 `.exe` — one with no imports, no
//! base relocations, no TLS — at its preferred `ImageBase` and jump to its
//! entry point in ring 3. The NT personality (PEB/TEB, `gs` base, `Nt*`
//! dispatch, `ntdll`) comes next; this proves the container format: DOS/PE
//! headers, section table, per-section protection, BSS zero-fill.
//!
//! Mirrors `elf::load`: fresh user frames, copy raw data, zero the tail.

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::process::Process;

pub struct PeImage {
    pub entry: u64,
    /// Preferred load address the sections landed at (relocs / W^X will use it).
    #[allow(dead_code)]
    pub base: u64,
    #[allow(dead_code)]
    pub size: u64,
}

const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

pub fn load(proc: &Process, image: &[u8]) -> Result<PeImage, &'static str> {
    if image.len() < 0x40 || &image[0..2] != b"MZ" {
        return Err("not an MZ image");
    }
    let pe_off = u32le(&image[0x3C..]) as usize;
    if pe_off + 0x18 > image.len() || &image[pe_off..pe_off + 4] != b"PE\0\0" {
        return Err("no PE signature");
    }

    let coff = pe_off + 4;
    let machine = u16le(&image[coff..]);
    if machine != 0x8664 {
        return Err("not x86-64 (PE Machine != 0x8664)");
    }
    let num_sections = u16le(&image[coff + 2..]) as usize;
    let opt_size = u16le(&image[coff + 16..]) as usize;

    let opt = coff + 20;
    if u16le(&image[opt..]) != 0x20B {
        return Err("not PE32+ (Optional Magic != 0x20B)");
    }
    let entry_rva = u32le(&image[opt + 16..]) as u64;
    let image_base = u64le(&image[opt + 24..]);
    let size_of_image = u32le(&image[opt + 56..]) as u64;
    let size_of_headers = u32le(&image[opt + 60..]) as u64;
    let n_dirs = u32le(&image[opt + 108..]) as usize;
    let dirs = opt + 112;

    // Data directory 1 = imports, 5 = base relocs, 9 = TLS. This first cut
    // handles none of them — reject rather than silently mislink.
    let dir = |i: usize| -> (u32, u32) {
        if i >= n_dirs {
            return (0, 0);
        }
        (u32le(&image[dirs + i * 8..]), u32le(&image[dirs + i * 8 + 4..]))
    };
    if dir(1).1 != 0 {
        return Err("PE imports not supported yet");
    }
    if dir(9).1 != 0 {
        return Err("PE TLS directory not supported yet");
    }

    // Map the header page(s) read-only at the image base — a PE reads its own
    // headers through __ImageBase.
    map_range(proc, image, 0, image_base, size_of_headers, size_of_headers, false, false);

    let sect_tbl = opt + opt_size;
    for i in 0..num_sections {
        let s = sect_tbl + i * 40;
        if s + 40 > image.len() {
            return Err("section table truncated");
        }
        let vsize = u32le(&image[s + 8..]) as u64;
        let vaddr = u32le(&image[s + 12..]) as u64;
        let raw_size = u32le(&image[s + 16..]) as u64;
        let raw_ptr = u32le(&image[s + 20..]) as u64;
        let chars = u32le(&image[s + 36..]);

        let mem_size = vsize.max(raw_size);
        let file_size = vsize.min(raw_size); // don't copy past the section's virtual extent
        map_range(
            proc,
            image,
            raw_ptr,
            image_base + vaddr,
            file_size,
            mem_size,
            chars & IMAGE_SCN_MEM_WRITE != 0,
            chars & IMAGE_SCN_MEM_EXECUTE != 0,
        );
    }

    Ok(PeImage {
        entry: image_base + entry_rva,
        base: image_base,
        size: size_of_image,
    })
}

/// Map `[vaddr, vaddr+mem_size)` as fresh zeroed user pages, copying `file_size`
/// bytes from `image[file_off..]` into the front. `writable` is currently forced
/// on (like the ELF loader) until W^X is applied after relocation.
fn map_range(
    proc: &Process,
    image: &[u8],
    file_off: u64,
    vaddr: u64,
    file_size: u64,
    mem_size: u64,
    _writable: bool,
    exec: bool,
) {
    let vstart = vaddr;
    let vend = vaddr + mem_size.max(1);
    let mut page = vstart & !0xFFF;

    while page < vend {
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame for PE section");
        let phys = frame.start_address();
        let dst = phys_to_virt(phys).as_mut_ptr::<u8>();
        unsafe { core::ptr::write_bytes(dst, 0, 4096) };

        let copy_lo = page.max(vstart);
        let copy_hi = (page + 4096).min(vstart + file_size);
        if copy_hi > copy_lo {
            let src = (file_off + (copy_lo - vstart)) as usize;
            let n = (copy_hi - copy_lo) as usize;
            let dst_off = (copy_lo - page) as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(image[src..src + n].as_ptr(), dst.add(dst_off), n);
            }
        }

        proc.map(page, phys.as_u64(), true, exec);
        page += 4096;
    }
}
