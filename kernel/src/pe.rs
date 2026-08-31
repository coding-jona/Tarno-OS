// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — a minimal PE32+ (x86-64) loader.
//!
//! Enough to map a **statically linked** Win64 `.exe` — one with no imports, no
//! base relocations, no TLS — at its preferred `ImageBase` and jump to its
//! entry point in ring 3. The NT personality (PEB/TEB, `gs` base, `Nt*`
//! dispatch, `ntdll`) comes next; this proves the container format: DOS/PE
//! headers, section table, per-section protection, BSS zero-fill.
//!
//! **Every input is treated as hostile.** A malformed header, an out-of-range
//! offset, an overflowing size — all produce `Err`, never a slice panic. A
//! broken `.exe` must not be able to fault the kernel.

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

/// Upper bounds — a real Win64 `.exe` is comfortably inside these; anything past
/// them is either corrupt or hostile.
const MAX_IMAGE_SIZE: u64 = 256 * 1024 * 1024;
const MAX_SECTIONS: usize = 96;

/// Bounds-checked little-endian reads. `off` is a byte offset into the image.
fn rd_u16(img: &[u8], off: usize) -> Result<u16, &'static str> {
    img.get(off..off + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or("PE: truncated (u16)")
}
fn rd_u32(img: &[u8], off: usize) -> Result<u32, &'static str> {
    img.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or("PE: truncated (u32)")
}
fn rd_u64(img: &[u8], off: usize) -> Result<u64, &'static str> {
    img.get(off..off + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or("PE: truncated (u64)")
}

pub fn load(proc: &Process, image: &[u8]) -> Result<PeImage, &'static str> {
    if image.get(0..2) != Some(b"MZ") {
        return Err("PE: not an MZ image");
    }
    let pe_off = rd_u32(image, 0x3C)? as usize;
    if image.get(pe_off..pe_off + 4) != Some(b"PE\0\0") {
        return Err("PE: no PE signature");
    }

    // COFF file header (20 bytes) at pe_off + 4.
    let coff = pe_off + 4;
    if rd_u16(image, coff)? != 0x8664 {
        return Err("PE: not x86-64 (Machine != 0x8664)");
    }
    let num_sections = rd_u16(image, coff + 2)? as usize;
    if num_sections == 0 || num_sections > MAX_SECTIONS {
        return Err("PE: absurd NumberOfSections");
    }
    let opt_size = rd_u16(image, coff + 16)? as usize;

    // Optional header (PE32+) at coff + 20.
    let opt = coff + 20;
    if opt_size < 112 {
        return Err("PE: optional header too small");
    }
    if rd_u16(image, opt)? != 0x20B {
        return Err("PE: not PE32+ (Optional Magic != 0x20B)");
    }
    let entry_rva = rd_u32(image, opt + 16)? as u64;
    let image_base = rd_u64(image, opt + 24)?;
    let sect_align = rd_u32(image, opt + 32)? as u64;
    let size_of_image = rd_u32(image, opt + 56)? as u64;
    let size_of_headers = rd_u32(image, opt + 60)? as u64;
    let n_dirs = rd_u32(image, opt + 108)? as usize;

    if image_base & 0xFFF != 0 {
        return Err("PE: ImageBase not page-aligned");
    }
    if image_base == 0 || image_base >= 0x0000_8000_0000_0000 {
        return Err("PE: ImageBase outside the user half");
    }
    if size_of_image == 0 || size_of_image > MAX_IMAGE_SIZE {
        return Err("PE: absurd SizeOfImage");
    }
    if sect_align < 0x1000 || sect_align & (sect_align - 1) != 0 {
        return Err("PE: bad SectionAlignment");
    }
    if entry_rva >= size_of_image {
        return Err("PE: entry point outside the image");
    }
    if image_base.checked_add(size_of_image).is_none() {
        return Err("PE: ImageBase + SizeOfImage overflows");
    }

    // Data directory 1 = imports, 5 = base relocs, 9 = TLS. This first cut
    // handles none of them — reject rather than silently mislink.
    let dirs = opt + 112;
    let dir_size = |i: usize| -> Result<u32, &'static str> {
        if i >= n_dirs {
            return Ok(0);
        }
        rd_u32(image, dirs + i * 8 + 4)
    };
    if dir_size(1)? != 0 {
        return Err("PE: imports not supported yet");
    }
    if dir_size(9)? != 0 {
        return Err("PE: TLS directory not supported yet");
    }

    // Header page(s), mapped read-only at the image base — a PE reads its own
    // headers through __ImageBase.
    let hdr_len = size_of_headers.min(image.len() as u64);
    map_range(proc, image, 0, image_base, hdr_len, hdr_len, false)?;

    let sect_tbl = opt + opt_size;
    for i in 0..num_sections {
        let s = sect_tbl + i * 40;
        if image.get(s..s + 40).is_none() {
            return Err("PE: section table truncated");
        }
        let vsize = rd_u32(image, s + 8)? as u64;
        let vaddr = rd_u32(image, s + 12)? as u64;
        let raw_size = rd_u32(image, s + 16)? as u64;
        let raw_ptr = rd_u32(image, s + 20)? as u64;
        let chars = rd_u32(image, s + 36)?;

        let mem_size = vsize.max(raw_size);
        if vaddr >= size_of_image || vaddr.checked_add(mem_size).map_or(true, |e| e > size_of_image) {
            return Err("PE: section outside the image");
        }
        // Only copy what the file actually contains.
        let avail = (image.len() as u64).saturating_sub(raw_ptr);
        let file_size = raw_size.min(vsize).min(avail);
        let file_off = if file_size == 0 { 0 } else { raw_ptr };

        map_range(
            proc,
            image,
            file_off,
            image_base + vaddr,
            file_size,
            mem_size,
            chars & IMAGE_SCN_MEM_EXECUTE != 0,
        )?;
        let _ = chars & IMAGE_SCN_MEM_WRITE; // honoured once W^X lands after relocs
    }

    Ok(PeImage {
        entry: image_base + entry_rva,
        base: image_base,
        size: size_of_image,
    })
}

/// Map `[vaddr, vaddr+mem_size)` as fresh zeroed user pages, copying `file_size`
/// bytes from `image[file_off..]` into the front. Caller has already checked
/// that `[file_off, file_off+file_size)` is inside `image`; this re-checks
/// anyway. `writable` is currently forced on (like the ELF loader) until W^X is
/// applied after relocation.
fn map_range(
    proc: &Process,
    image: &[u8],
    file_off: u64,
    vaddr: u64,
    file_size: u64,
    mem_size: u64,
    exec: bool,
) -> Result<(), &'static str> {
    let vstart = vaddr;
    let vend = vaddr.checked_add(mem_size.max(1)).ok_or("PE: section vaddr overflow")?;
    let mut page = vstart & !0xFFF;

    while page < vend {
        let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames")?;
        let phys = frame.start_address();
        let dst = phys_to_virt(phys).as_mut_ptr::<u8>();
        unsafe { core::ptr::write_bytes(dst, 0, 4096) };

        let copy_lo = page.max(vstart);
        let copy_hi = (page + 4096).min(vstart + file_size);
        if copy_hi > copy_lo {
            let src = (file_off + (copy_lo - vstart)) as usize;
            let n = (copy_hi - copy_lo) as usize;
            let dst_off = (copy_lo - page) as usize;
            let chunk = image.get(src..src + n).ok_or("PE: section data out of range")?;
            unsafe {
                core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.add(dst_off), n);
            }
        }

        proc.map(page, phys.as_u64(), true, exec);
        page += 4096;
    }
    Ok(())
}
