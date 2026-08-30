// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — a minimal ELF64 loader.
//!
//! Static `ET_EXEC` only: iterate `PT_LOAD` segments, map fresh user frames at
//! `p_vaddr`, copy `p_filesz` bytes from the image, zero the rest (bss). No
//! dynamic linking, no `PT_INTERP`, no `ET_DYN`/PIE relocation yet — that comes
//! with the userland loader (`ld.so` equivalent) in the POSIX personality.

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::process::Process;

pub struct Image {
    pub entry: u64,
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

pub fn load(proc: &Process, image: &[u8]) -> Result<Image, &'static str> {
    if image.len() < 64 || &image[0..4] != b"\x7FELF" {
        return Err("not an ELF");
    }
    if image[4] != 2 {
        return Err("not ELF64");
    }
    if image[5] != 1 {
        return Err("not little-endian");
    }
    if u16le(&image[16..]) != 2 {
        return Err("not ET_EXEC");
    }
    if u16le(&image[18..]) != 0x3E {
        return Err("not x86-64");
    }

    let e_entry = u64le(&image[24..]);
    let e_phoff = u64le(&image[32..]) as usize;
    let e_phentsize = u16le(&image[54..]) as usize;
    let e_phnum = u16le(&image[56..]) as usize;

    for i in 0..e_phnum {
        let ph = &image[e_phoff + i * e_phentsize..];
        if u32le(&ph[0..]) != 1 {
            continue; // PT_LOAD only
        }
        let flags = u32le(&ph[4..]);
        let p_offset = u64le(&ph[8..]);
        let p_vaddr = u64le(&ph[16..]);
        let p_filesz = u64le(&ph[32..]);
        let p_memsz = u64le(&ph[40..]);
        map_segment(proc, image, p_offset, p_vaddr, p_filesz, p_memsz, flags);
    }

    Ok(Image { entry: e_entry })
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
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame for ELF segment");
        let phys = frame.start_address();
        let dst = phys_to_virt(phys).as_mut_ptr::<u8>();
        unsafe { core::ptr::write_bytes(dst, 0, 4096) };

        let copy_lo = page.max(vstart);
        let copy_hi = (page + 4096).min(vstart + p_filesz);
        if copy_hi > copy_lo {
            let src = (p_offset + (copy_lo - vstart)) as usize;
            let n = (copy_hi - copy_lo) as usize;
            let dst_off = (copy_lo - page) as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(image[src..src + n].as_ptr(), dst.add(dst_off), n);
            }
        }

        // Map writable for now (a real loader tightens to W^X per p_flags once
        // relocations are applied).
        proc.map(page, phys.as_u64(), true, exec);
        page += 4096;
    }
}
