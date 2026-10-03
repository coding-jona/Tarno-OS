// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — a minimal ELF64 loader.
//!
//! `ET_EXEC` and `ET_DYN` (PIE): iterate `PT_LOAD` segments, map fresh user frames at
//! `base + p_vaddr`, copy `p_filesz` bytes from the image, zero the rest (bss). A
//! `PT_INTERP` program (dynamically linked) gets its interpreter (`ld.so`) loaded at a
//! second base and starts there, with `AT_BASE` / `AT_ENTRY` in the aux vector, exactly
//! like Linux. The interpreter does the relocation and the library loading itself.

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::process::Process;

pub struct Image {
    /// First instruction to run: the program's own entry, or the interpreter's for a
    /// dynamically linked program.
    pub entry: u64,
    /// The program's entry point (`AT_ENTRY`).
    pub prog_entry: u64,
    /// Where the interpreter was loaded (`AT_BASE`), 0 if there is none.
    pub interp_base: u64,
    /// Virtual address the program headers landed at (for `AT_PHDR`).
    pub phdr: u64,
    pub phent: u64,
    pub phnum: u64,
}

/// Load address of position-independent (`ET_DYN`) programs, and of their interpreter.
const PIE_BASE: u64 = 0x0000_5555_5555_4000;
const INTERP_BASE: u64 = 0x0000_7fff_f7c0_0000;

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

/// Largest memory footprint a single `PT_LOAD` may claim (user half only).
const MAX_SEG_MEM: u64 = 1 << 30;
const USER_TOP: u64 = 0x0000_8000_0000_0000;

/// Parse and bounds-check every header `load` will touch, without mapping
/// anything. A hostile or merely wrong file (a text file the shell tried to
/// `exec`, a truncated binary) must be an `Err`, never a kernel panic — the
/// PE loader has always been written that way, this brings the ELF side to the
/// same bar. `load` runs it first, so the two can't drift.
pub fn validate(image: &[u8]) -> Result<(), &'static str> {
    if image.len() < 64 || &image[0..4] != b"\x7FELF" {
        return Err("not an ELF");
    }
    if image[4] != 2 {
        return Err("not ELF64");
    }
    if image[5] != 1 {
        return Err("not little-endian");
    }
    let e_type = u16le(&image[16..]);
    if e_type != 2 && e_type != 3 {
        return Err("not ET_EXEC / ET_DYN");
    }
    if u16le(&image[18..]) != 0x3E {
        return Err("not x86-64");
    }
    let e_phoff = u64le(&image[32..]);
    let e_phentsize = u16le(&image[54..]) as u64;
    let e_phnum = u16le(&image[56..]) as u64;
    if e_phentsize < 56 || e_phnum == 0 || e_phnum > 128 {
        return Err("bad program header table");
    }
    let tbl_end = e_phentsize
        .checked_mul(e_phnum)
        .and_then(|n| n.checked_add(e_phoff))
        .ok_or("program header table overflows")?;
    if tbl_end > image.len() as u64 {
        return Err("program headers outside the file");
    }
    let mut loads = 0;
    for i in 0..e_phnum {
        let ph = &image[(e_phoff + i * e_phentsize) as usize..];
        if u32le(&ph[0..]) == 3 {
            // PT_INTERP: a NUL-terminated path inside the file
            let (o, n) = (u64le(&ph[8..]), u64le(&ph[32..]));
            if n == 0 || n > 256 || o.checked_add(n).map_or(true, |e| e > image.len() as u64) {
                return Err("bad PT_INTERP");
            }
            continue;
        }
        if u32le(&ph[0..]) != 1 {
            continue;
        }
        loads += 1;
        let (p_offset, p_vaddr) = (u64le(&ph[8..]), u64le(&ph[16..]));
        let (p_filesz, p_memsz) = (u64le(&ph[32..]), u64le(&ph[40..]));
        if p_filesz > p_memsz || p_memsz > MAX_SEG_MEM {
            return Err("bad segment size");
        }
        if p_offset.checked_add(p_filesz).map_or(true, |e| e > image.len() as u64) {
            return Err("segment data outside the file");
        }
        // ET_EXEC lives at its own absolute addresses; ET_DYN is relocated by a base
        // (PIE_BASE / INTERP_BASE), so its link-time addresses must stay small enough that
        // base + address still lies in the user half.
        let limit = if e_type == 2 { USER_TOP } else { 1 << 40 };
        if (e_type == 2 && p_vaddr == 0) || p_vaddr.checked_add(p_memsz).map_or(true, |e| e >= limit) {
            return Err("segment outside the user address space");
        }
    }
    if loads == 0 {
        return Err("no PT_LOAD segments");
    }
    if u64le(&image[24..]) >= if e_type == 2 { USER_TOP } else { 1 << 40 } {
        return Err("entry point outside the user address space");
    }
    Ok(())
}

/// The `PT_INTERP` path of a validated image, if it has one.
fn interp_path(image: &[u8]) -> Option<alloc::string::String> {
    let e_phoff = u64le(&image[32..]);
    let e_phentsize = u16le(&image[54..]) as u64;
    let e_phnum = u16le(&image[56..]) as u64;
    for i in 0..e_phnum {
        let ph = &image[(e_phoff + i * e_phentsize) as usize..];
        if u32le(&ph[0..]) == 3 {
            let (o, n) = (u64le(&ph[8..]) as usize, u64le(&ph[32..]) as usize);
            let raw = &image[o..o + n];
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            return core::str::from_utf8(&raw[..end]).ok().map(alloc::string::String::from);
        }
    }
    None
}

/// Map `image` (and, for a dynamically linked program, its interpreter) into `proc`.
pub fn load(proc: &Process, image: &[u8]) -> Result<Image, &'static str> {
    validate(image)?;
    let base = if u16le(&image[16..]) == 3 { PIE_BASE } else { 0 };
    let mut img = load_one(proc, image, base);
    img.prog_entry = img.entry;
    if let Some(path) = interp_path(image) {
        let bytes = crate::ext2::open()
            .ok()
            .and_then(|fs| fs.read_path(&path))
            .ok_or("interpreter not found")?;
        validate(&bytes)?;
        if u16le(&bytes[16..]) != 3 {
            return Err("interpreter is not ET_DYN");
        }
        let interp = load_one(proc, &bytes, INTERP_BASE);
        img.interp_base = INTERP_BASE;
        img.entry = interp.entry;
    }
    Ok(img)
}

/// Map one validated ELF's `PT_LOAD` segments at `base` (0 for `ET_EXEC`).
fn load_one(proc: &Process, image: &[u8], base: u64) -> Image {
    let e_entry = u64le(&image[24..]);
    let e_phoff = u64le(&image[32..]);
    let e_phentsize = u16le(&image[54..]) as u64;
    let e_phnum = u16le(&image[56..]) as u64;

    let mut phdr_vaddr = 0u64;
    for i in 0..e_phnum {
        let ph = &image[(e_phoff + i * e_phentsize) as usize..];
        if u32le(&ph[0..]) != 1 {
            continue; // PT_LOAD only
        }
        let flags = u32le(&ph[4..]);
        let p_offset = u64le(&ph[8..]);
        let p_vaddr = base + u64le(&ph[16..]);
        let p_filesz = u64le(&ph[32..]);
        let p_memsz = u64le(&ph[40..]);
        // Where did the program headers land? (the PT_LOAD whose file range covers e_phoff)
        if e_phoff >= p_offset && e_phoff < p_offset + p_filesz {
            phdr_vaddr = p_vaddr + (e_phoff - p_offset);
        }
        map_segment(proc, image, p_offset, p_vaddr, p_filesz, p_memsz, flags);
    }
    Image {
        entry: base + e_entry,
        prog_entry: 0,
        interp_base: 0,
        phdr: phdr_vaddr,
        phent: e_phentsize,
        phnum: e_phnum,
    }
}

fn map_segment(
    proc: &Process,
    image: &[u8],
    p_offset: u64,
    p_vaddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    flags: u32,
) {
    let exec = flags & 1 != 0;
    let vstart = p_vaddr;
    let vend = p_vaddr + p_memsz;
    let mut page = vstart & !0xFFF;

    while page < vend {
        // Two segments can share a page (e.g. the end of text and the start of data in a
        // library linked without page-aligned segments): map it once, fill it twice.
        if proc.translate(page).is_none() {
            let frame = FRAME_ALLOC.lock().alloc().expect("no frame for ELF segment");
            let phys = frame.start_address();
            unsafe { core::ptr::write_bytes(phys_to_virt(phys).as_mut_ptr::<u8>(), 0, 4096) };
            // Map writable for now (a real loader tightens to W^X per p_flags once
            // relocations are applied).
            proc.map(page, phys.as_u64(), true, exec);
        }
        let copy_lo = page.max(vstart);
        let copy_hi = (page + 4096).min(vstart + p_filesz);
        if copy_hi > copy_lo {
            let src = (p_offset + (copy_lo - vstart)) as usize;
            let n = (copy_hi - copy_lo) as usize;
            proc.write_user(copy_lo, &image[src..src + n]);
        }
        page += 4096;
    }
}
