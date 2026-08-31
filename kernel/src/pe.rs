// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — a minimal PE32+ (x86-64) loader.
//!
//! Maps a **statically linked** Win64 `.exe` — no imports, no TLS — and jumps
//! to its entry point in ring 3. Base relocations (`IMAGE_REL_BASED_DIR64`) are
//! applied, so a PE that ships a `.reloc` section can load away from its
//! preferred `ImageBase`. Imports (data dir 1) are still rejected; resolving
//! them against `ntdll`/`kernel32` is the next step and the start of the NT
//! personality.
//!
//! **Every input is treated as hostile.** A malformed header, an out-of-range
//! offset, an overflowing size — all produce `Err`, never a slice panic.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::process::Process;

pub struct PeImage {
    pub entry: u64,
    /// Win64 TEB virtual address — becomes the thread's `%gs` base.
    pub teb: u64,
    #[allow(dead_code)]
    pub base: u64,
    #[allow(dead_code)]
    pub size: u64,
}

const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;
const IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE: u16 = 0x0040;
const IMAGE_REL_BASED_ABSOLUTE: u16 = 0;
const IMAGE_REL_BASED_DIR64: u16 = 10;

/// Fixed non-zero shift used when a relocatable PE must move off its preferred
/// base. A real availability check / ASLR arrives with the DLL loader.
const ALT_BASE_SHIFT: u64 = 0x1000_0000;

const MAX_IMAGE_SIZE: u64 = 256 * 1024 * 1024;
const MAX_SECTIONS: usize = 96;

/// The builtin "NT stub" page mapped into every PE process. Each imported Win32
/// function resolves to one 16-byte trampoline here:
///
/// ```text
///   mov eax, NT_BASE | idx     ; select the NT call
///   mov r10, rcx               ; preserve Win64 arg0 before syscall clobbers rcx
///   syscall
///   ret
/// ```
///
/// `nt::dispatch` reads the remaining Win64 args (`rdx`, `r8`, `r9`, stack)
/// off the frame. This is the seed of the NT personality; real `ntdll` and
/// more calls grow from here.
const NT_STUB_BASE: u64 = 0x0000_7FF0_0000_0000;
const NT_STUB_STRIDE: u64 = 16;

/// Win64 TEB / PEB, one page each, just past the stub page. `%gs` points at the
/// TEB for a PE thread. Minimal fill — enough that a CRT's early
/// `gs:[0x30]` (self), `gs:[0x60]` (PEB), stack-bound and `LastError` reads do
/// not fault.
const PE_TEB_ADDR: u64 = NT_STUB_BASE + 0x1000;
const PE_PEB_ADDR: u64 = NT_STUB_BASE + 0x2000;
/// One page holding `RTL_USER_PROCESS_PARAMETERS`, `PEB_LDR_DATA`, one
/// `LDR_DATA_TABLE_ENTRY` (the exe), plus the strings they point at.
const PE_PARAMS_ADDR: u64 = NT_STUB_BASE + 0x3000;
const PARAMS_OFF: u64 = 0x000; // RTL_USER_PROCESS_PARAMETERS
const LDR_OFF: u64 = 0x100; // PEB_LDR_DATA
const MOD_OFF: u64 = 0x180; // LDR_DATA_TABLE_ENTRY (the exe)
const WSTR_OFF: u64 = 0x200; // UTF-16 strings
const ANSI_CMDLINE_OFF: u64 = 0x400; // ANSI command line
const ENV_OFF: u64 = 0x500; // environment block (empty)
/// `GetCommandLineA` returns this.
pub const PE_ANSI_CMDLINE_ADDR: u64 = PE_PARAMS_ADDR + ANSI_CMDLINE_OFF;
const ANSI_CMDLINE: &[u8] = b"PE argv0 pe-hello.exe\n\0";

/// Index of the stub implementing `dll!func`, or `None` if THOS does not
/// provide it yet (the loader then rejects the PE rather than mislinking).
fn resolve_import(dll: &str, func: &str) -> Option<u16> {
    use crate::nt::*;
    let dll = dll.trim_end_matches('\0').to_ascii_lowercase();
    match (dll.as_str(), func) {
        ("kernel32.dll", "ExitProcess") => Some(NT_EXITPROCESS),
        ("kernel32.dll", "GetStdHandle") => Some(NT_GETSTDHANDLE),
        ("kernel32.dll", "WriteFile") => Some(NT_WRITEFILE),
        ("kernel32.dll", "GetLastError") => Some(NT_GETLASTERROR),
        ("kernel32.dll", "SetLastError") => Some(NT_SETLASTERROR),
        ("kernel32.dll", "CreateFileA") => Some(NT_CREATEFILEA),
        ("kernel32.dll", "ReadFile") => Some(NT_READFILE),
        ("kernel32.dll", "CloseHandle") => Some(NT_CLOSEHANDLE),
        ("kernel32.dll", "GetCommandLineA") => Some(NT_GETCOMMANDLINEA),
        ("kernel32.dll", "GetModuleHandleA") => Some(NT_GETMODULEHANDLEA),
        ("kernel32.dll", "VirtualAlloc") => Some(NT_VIRTUALALLOC),
        ("kernel32.dll", "VirtualFree") => Some(NT_VIRTUALFREE),
        ("kernel32.dll", "VirtualProtect") => Some(NT_VIRTUALPROTECT),
        ("kernel32.dll", "GetProcessHeap") => Some(NT_GETPROCESSHEAP),
        ("kernel32.dll", "HeapAlloc") => Some(NT_HEAPALLOC),
        ("kernel32.dll", "HeapFree") => Some(NT_HEAPFREE),
        _ => None,
    }
}

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

pub fn load(proc: &Process, file: &[u8], stack_top: u64) -> Result<PeImage, &'static str> {
    if file.get(0..2) != Some(b"MZ") {
        return Err("PE: not an MZ image");
    }
    let pe_off = rd_u32(file, 0x3C)? as usize;
    if file.get(pe_off..pe_off + 4) != Some(b"PE\0\0") {
        return Err("PE: no PE signature");
    }

    let coff = pe_off + 4;
    if rd_u16(file, coff)? != 0x8664 {
        return Err("PE: not x86-64 (Machine != 0x8664)");
    }
    let num_sections = rd_u16(file, coff + 2)? as usize;
    if num_sections == 0 || num_sections > MAX_SECTIONS {
        return Err("PE: absurd NumberOfSections");
    }
    let opt_size = rd_u16(file, coff + 16)? as usize;

    let opt = coff + 20;
    if opt_size < 112 {
        return Err("PE: optional header too small");
    }
    if rd_u16(file, opt)? != 0x20B {
        return Err("PE: not PE32+ (Optional Magic != 0x20B)");
    }
    let entry_rva = rd_u32(file, opt + 16)? as u64;
    let image_base = rd_u64(file, opt + 24)?;
    let sect_align = rd_u32(file, opt + 32)? as u64;
    let size_of_image = rd_u32(file, opt + 56)? as u64;
    let size_of_headers = rd_u32(file, opt + 60)? as u64;
    let dll_chars = rd_u16(file, opt + 70)?;
    let n_dirs = rd_u32(file, opt + 108)? as usize;

    if image_base & 0xFFF != 0 || image_base == 0 || image_base >= 0x0000_8000_0000_0000 {
        return Err("PE: bad ImageBase");
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

    let dirs = opt + 112;
    let dir = |i: usize| -> Result<(u32, u32), &'static str> {
        if i >= n_dirs {
            return Ok((0, 0));
        }
        Ok((rd_u32(file, dirs + i * 8)?, rd_u32(file, dirs + i * 8 + 4)?))
    };
    if dir(9)?.1 != 0 {
        return Err("PE: TLS directory not supported yet");
    }
    let (import_rva, import_size) = dir(1)?;
    let (reloc_rva, reloc_size) = dir(5)?;

    // --- materialise the full image at RVA 0, then relocate, then map ---
    let mut img: Vec<u8> = vec![0u8; size_of_image as usize];

    let hdr_n = (size_of_headers as usize).min(file.len()).min(img.len());
    img[..hdr_n].copy_from_slice(&file[..hdr_n]);

    let sect_tbl = opt + opt_size;
    // (rva, mem_size, exec) per section, for the mapping pass.
    let mut segs: Vec<(u64, u64, bool)> = Vec::with_capacity(num_sections);
    for i in 0..num_sections {
        let s = sect_tbl + i * 40;
        if file.get(s..s + 40).is_none() {
            return Err("PE: section table truncated");
        }
        let vsize = rd_u32(file, s + 8)? as u64;
        let vaddr = rd_u32(file, s + 12)? as u64;
        let raw_size = rd_u32(file, s + 16)? as u64;
        let raw_ptr = rd_u32(file, s + 20)? as u64;
        let chars = rd_u32(file, s + 36)?;

        let mem_size = vsize.max(raw_size);
        if vaddr >= size_of_image
            || vaddr.checked_add(mem_size).map_or(true, |e| e > size_of_image)
        {
            return Err("PE: section outside the image");
        }
        let avail = (file.len() as u64).saturating_sub(raw_ptr);
        let fsize = raw_size.min(vsize).min(avail) as usize;
        if fsize > 0 {
            let src = raw_ptr as usize;
            img[vaddr as usize..vaddr as usize + fsize]
                .copy_from_slice(&file[src..src + fsize]);
        }
        segs.push((vaddr, mem_size, chars & IMAGE_SCN_MEM_EXECUTE != 0));
        let _ = chars & IMAGE_SCN_MEM_WRITE; // honoured once W^X lands
    }

    // Preferred base unless the PE opted into relocation — then move it, to
    // actually exercise the fixup path (a real availability check comes with
    // the DLL loader).
    let relocatable = reloc_size != 0 && dll_chars & IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE != 0;
    let load_base = if relocatable {
        image_base
            .checked_add(ALT_BASE_SHIFT)
            .filter(|b| b + size_of_image < 0x0000_8000_0000_0000)
            .ok_or("PE: no room for an alternative base")?
    } else {
        image_base
    };
    let delta = load_base.wrapping_sub(image_base);

    if delta != 0 && reloc_size != 0 {
        apply_relocs(&mut img, reloc_rva as u64, reloc_size as u64, delta, size_of_image)?;
    } else if delta != 0 {
        return Err("PE: needs relocation but has no .reloc");
    }

    // Resolve the import table against THOS's builtin NT stubs and patch the
    // IAT in place (post-relocation). No real DLL files yet.
    if import_size != 0 {
        resolve_imports(&mut img, import_rva as u64, size_of_image)?;
    }

    // --- map the finished image (headers first, sections win on any overlap) ---
    map_seg(proc, &img, load_base, 0, hdr_n as u64, false)?;
    for (rva, mem_size, exec) in segs {
        map_seg(proc, &img, load_base + rva, rva, mem_size, exec)?;
    }
    map_stub_page(proc)?;
    map_teb_peb(proc, load_base, load_base + entry_rva, size_of_image, stack_top)?;

    Ok(PeImage {
        entry: load_base + entry_rva,
        teb: PE_TEB_ADDR,
        base: load_base,
        size: size_of_image,
    })
}

/// Allocate + map the TEB and PEB pages and fill the few fields a Win64 entry /
/// CRT touches immediately.
fn map_teb_peb(
    proc: &Process,
    image_base: u64,
    entry: u64,
    size_of_image: u64,
    stack_top: u64,
) -> Result<(), &'static str> {
    let w = |addr: u64, fill: &dyn Fn(&mut [u8])| -> Result<(), &'static str> {
        let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (TEB/PEB)")?;
        let phys = frame.start_address();
        let page = unsafe {
            let p = phys_to_virt(phys).as_mut_ptr::<u8>();
            core::ptr::write_bytes(p, 0, 4096);
            core::slice::from_raw_parts_mut(p, 4096)
        };
        fill(page);
        proc.map(addr, phys.as_u64(), true, false); // rw, no-exec
        Ok(())
    };
    let put = |b: &mut [u8], off: u64, v: u64| {
        b[off as usize..off as usize + 8].copy_from_slice(&v.to_le_bytes())
    };
    let put16 = |b: &mut [u8], off: u64, v: u16| {
        b[off as usize..off as usize + 2].copy_from_slice(&v.to_le_bytes())
    };

    w(PE_TEB_ADDR, &|t| {
        put(t, 0x08, stack_top); // NT_TIB.StackBase
        put(t, 0x10, stack_top.saturating_sub(64 * 1024)); // NT_TIB.StackLimit
        put(t, 0x30, PE_TEB_ADDR); // NT_TIB.Self
        put(t, 0x60, PE_PEB_ADDR); // ProcessEnvironmentBlock
    })?;
    w(PE_PEB_ADDR, &|p| {
        put(p, 0x10, image_base); // ImageBaseAddress
        put(p, 0x18, PE_PARAMS_ADDR + LDR_OFF); // Ldr
        put(p, 0x20, PE_PARAMS_ADDR + PARAMS_OFF); // ProcessParameters
        put(p, 0x30, crate::nt::PE_PROCESS_HEAP); // ProcessHeap
    })?;

    // A UTF-16LE string placed at WSTR_OFF+cursor; returns (buffer_va, byte_len).
    w(PE_PARAMS_ADDR, &|b| {
        let mut wcur = WSTR_OFF;
        let mut wstr = |b: &mut [u8], s: &str| -> (u64, u16) {
            let va = PE_PARAMS_ADDR + wcur;
            let mut n = 0u64;
            for u in s.encode_utf16() {
                b[(wcur + n) as usize..(wcur + n) as usize + 2].copy_from_slice(&u.to_le_bytes());
                n += 2;
            }
            b[(wcur + n) as usize..(wcur + n) as usize + 2].copy_from_slice(&[0, 0]); // NUL
            wcur += n + 2;
            wcur = (wcur + 1) & !1;
            (va, n as u16)
        };
        let (image_buf, image_len) = wstr(b, "C:\\pe-hello.exe");
        let (cmd_buf, cmd_len) = wstr(b, "pe-hello.exe");
        let (base_buf, base_len) = wstr(b, "pe-hello.exe");
        let (cur_buf, cur_len) = wstr(b, "C:\\");

        // --- RTL_USER_PROCESS_PARAMETERS @ PARAMS_OFF ---
        put(b, PARAMS_OFF + 0x00, 0x1000); // MaximumLength
        put(b, PARAMS_OFF + 0x04, 0x1000); // Length + Flags (fits in the u64)
        put(b, PARAMS_OFF + 0x20, 0); // StandardInput
        put(b, PARAMS_OFF + 0x28, 1); // StandardOutput
        put(b, PARAMS_OFF + 0x30, 2); // StandardError
        put16(b, PARAMS_OFF + 0x38, cur_len); // CurrentDirectory.DosPath.Length
        put16(b, PARAMS_OFF + 0x3A, cur_len + 2);
        put(b, PARAMS_OFF + 0x40, cur_buf);
        put16(b, PARAMS_OFF + 0x60, image_len); // ImagePathName
        put16(b, PARAMS_OFF + 0x62, image_len + 2);
        put(b, PARAMS_OFF + 0x68, image_buf);
        put16(b, PARAMS_OFF + 0x70, cmd_len); // CommandLine
        put16(b, PARAMS_OFF + 0x72, cmd_len + 2);
        put(b, PARAMS_OFF + 0x78, cmd_buf);
        put(b, PARAMS_OFF + 0x80, PE_PARAMS_ADDR + ENV_OFF); // Environment (empty: two NUL words)

        // --- PEB_LDR_DATA @ LDR_OFF (one circular element: the exe) ---
        let ldr = PE_PARAMS_ADDR + LDR_OFF;
        let m = PE_PARAMS_ADDR + MOD_OFF;
        put(b, LDR_OFF + 0x00, 0x58); // Length
        b[(LDR_OFF + 0x04) as usize] = 1; // Initialized
        for (i, list) in [0x10u64, 0x20, 0x30].iter().enumerate() {
            let head = ldr + list;
            let link = m + i as u64 * 0x10; // entry's matching LIST_ENTRY
            put(b, LDR_OFF + list, link); // head.Flink
            put(b, LDR_OFF + list + 8, link); // head.Blink
            put(b, MOD_OFF + i as u64 * 0x10, head); // entry.link.Flink
            put(b, MOD_OFF + i as u64 * 0x10 + 8, head); // entry.link.Blink
        }
        put(b, MOD_OFF + 0x30, image_base); // DllBase
        put(b, MOD_OFF + 0x38, entry); // EntryPoint
        put(b, MOD_OFF + 0x40, size_of_image & 0xFFFF_FFFF); // SizeOfImage (u32, low half of the slot)
        put16(b, MOD_OFF + 0x48, image_len); // FullDllName
        put16(b, MOD_OFF + 0x4A, image_len + 2);
        put(b, MOD_OFF + 0x50, image_buf);
        put16(b, MOD_OFF + 0x58, base_len); // BaseDllName
        put16(b, MOD_OFF + 0x5A, base_len + 2);
        put(b, MOD_OFF + 0x60, base_buf);

        // --- ANSI command line (GetCommandLineA) + empty environment ---
        b[ANSI_CMDLINE_OFF as usize..ANSI_CMDLINE_OFF as usize + ANSI_CMDLINE.len()]
            .copy_from_slice(ANSI_CMDLINE);
        // ENV_OFF: leave the two NUL words already zeroed
    })?;
    Ok(())
}

/// Walk the `.reloc` blocks and add `delta` to every `DIR64` target. Rejects
/// any relocation type other than `ABSOLUTE` (padding) and `DIR64` — those are
/// all a well-formed x64 PE uses.
fn apply_relocs(
    img: &mut [u8],
    reloc_rva: u64,
    reloc_size: u64,
    delta: u64,
    size_of_image: u64,
) -> Result<(), &'static str> {
    let end = reloc_rva
        .checked_add(reloc_size)
        .filter(|&e| e <= size_of_image && e <= img.len() as u64)
        .ok_or("PE: .reloc out of range")?;
    let mut p = reloc_rva as usize;
    while p + 8 <= end as usize {
        let page_rva = u32::from_le_bytes(img[p..p + 4].try_into().unwrap()) as u64;
        let block_size = u32::from_le_bytes(img[p + 4..p + 8].try_into().unwrap()) as usize;
        if block_size < 8 || p + block_size > end as usize {
            return Err("PE: bad .reloc block size");
        }
        let entries = (block_size - 8) / 2;
        for e in 0..entries {
            let raw = u16::from_le_bytes(img[p + 8 + e * 2..p + 8 + e * 2 + 2].try_into().unwrap());
            let typ = raw >> 12;
            let off = (raw & 0x0FFF) as u64;
            match typ {
                IMAGE_REL_BASED_ABSOLUTE => {}
                IMAGE_REL_BASED_DIR64 => {
                    let t = (page_rva + off) as usize;
                    let slot = img
                        .get_mut(t..t + 8)
                        .ok_or("PE: reloc target out of range")?;
                    let v = u64::from_le_bytes(slot.try_into().unwrap()).wrapping_add(delta);
                    slot.copy_from_slice(&v.to_le_bytes());
                }
                _ => return Err("PE: unsupported relocation type"),
            }
        }
        p += block_size;
    }
    Ok(())
}

/// A NUL-terminated ASCII string at `img[off..]`, capped at 256 bytes.
fn cstr_at(img: &[u8], off: u64) -> Result<&str, &'static str> {
    let off = off as usize;
    let slice = img.get(off..(off + 256).min(img.len())).ok_or("PE: string out of range")?;
    let end = slice.iter().position(|&b| b == 0).ok_or("PE: unterminated string")?;
    core::str::from_utf8(&slice[..end]).map_err(|_| "PE: non-UTF8 import name")
}

/// Walk the Import Directory Table, resolve each thunk against `resolve_import`,
/// and overwrite its IAT slot with the stub address. Rejects any import THOS
/// does not provide (rather than leaving a dangling IAT entry).
fn resolve_imports(img: &mut [u8], import_rva: u64, size_of_image: u64) -> Result<(), &'static str> {
    let mut idt = import_rva as usize;
    loop {
        let end = idt + 20;
        if end as u64 > size_of_image || end > img.len() {
            return Err("PE: import table truncated");
        }
        let ilt_rva = u32::from_le_bytes(img[idt..idt + 4].try_into().unwrap()) as u64;
        let name_rva = u32::from_le_bytes(img[idt + 12..idt + 16].try_into().unwrap()) as u64;
        let iat_rva = u32::from_le_bytes(img[idt + 16..idt + 20].try_into().unwrap()) as u64;
        if name_rva == 0 && iat_rva == 0 {
            return Ok(()); // null terminator
        }
        let dll = String::from(cstr_at(img, name_rva)?); // owned — img is mutated below

        // The ILT holds the by-name/ordinal descriptors; the IAT is what we
        // patch. If the ILT is absent, the IAT still holds them pre-load.
        let names_rva = if ilt_rva != 0 { ilt_rva } else { iat_rva };
        let mut i = 0u64;
        loop {
            let t_off = (names_rva + i * 8) as usize;
            let p_off = (iat_rva + i * 8) as usize;
            if (t_off + 8) as u64 > size_of_image || t_off + 8 > img.len() {
                return Err("PE: import thunk out of range");
            }
            let thunk = u64::from_le_bytes(img[t_off..t_off + 8].try_into().unwrap());
            if thunk == 0 {
                break;
            }
            let stub_idx = if thunk & 0x8000_0000_0000_0000 != 0 {
                // import by ordinal — no ordinal tables yet
                return Err("PE: import by ordinal not supported yet");
            } else {
                let hint_name_rva = thunk & 0x7FFF_FFFF;
                let func = cstr_at(img, hint_name_rva + 2)?; // skip the 2-byte hint
                match resolve_import(&dll, func) {
                    Some(i) => i,
                    None => {
                        crate::kprintln!("THOS: pe unresolved    {}!{}", dll.as_str(), func);
                        return Err("PE: unresolved import");
                    }
                }
            };
            let addr = NT_STUB_BASE + stub_idx as u64 * NT_STUB_STRIDE;
            img[p_off..p_off + 8].copy_from_slice(&addr.to_le_bytes());
            i += 1;
        }
        idt += 20;
    }
}

/// Map the shared NT stub page into `proc` at [`NT_STUB_BASE`] (exec,
/// read-only), one 16-byte trampoline per NT call index.
fn map_stub_page(proc: &Process) -> Result<(), &'static str> {
    let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (stub page)")?;
    let phys = frame.start_address();
    let dst = phys_to_virt(phys).as_mut_ptr::<u8>();
    unsafe { core::ptr::write_bytes(dst, 0, 4096) };

    for idx in 0..crate::nt::NT_STUB_COUNT {
        let stub = [
            0xB8, 0, 0, 0, 0, // mov eax, imm32  (patched below)
            0x49, 0x89, 0xCA, // mov r10, rcx
            0x0F, 0x05, // syscall
            0xC3, // ret
        ];
        let mut stub = stub;
        let sel = (crate::nt::NT_BASE | idx as u64) as u32;
        stub[1..5].copy_from_slice(&sel.to_le_bytes());
        let at = idx as usize * NT_STUB_STRIDE as usize;
        unsafe { core::ptr::copy_nonoverlapping(stub.as_ptr(), dst.add(at), stub.len()) };
    }

    proc.map(NT_STUB_BASE, phys.as_u64(), false, true);
    Ok(())
}

/// Map `[vaddr, vaddr+mem_size)` as fresh zeroed user pages, copying the front
/// `file_size` bytes from `img[img_off..]` (the already-relocated image).
fn map_seg(
    proc: &Process,
    img: &[u8],
    vaddr: u64,
    img_off: u64,
    file_size: u64,
    exec: bool,
) -> Result<(), &'static str> {
    let vstart = vaddr;
    let vend = vaddr
        .checked_add(file_size.max(1))
        .ok_or("PE: section vaddr overflow")?;
    let mut page = vstart & !0xFFF;

    while page < vend {
        let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames")?;
        let phys = frame.start_address();
        let dst = phys_to_virt(phys).as_mut_ptr::<u8>();
        unsafe { core::ptr::write_bytes(dst, 0, 4096) };

        let copy_lo = page.max(vstart);
        let copy_hi = (page + 4096).min(vstart + file_size);
        if copy_hi > copy_lo {
            let src = (img_off + (copy_lo - vstart)) as usize;
            let n = (copy_hi - copy_lo) as usize;
            let dst_off = (copy_lo - page) as usize;
            let chunk = img.get(src..src + n).ok_or("PE: image data out of range")?;
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.add(dst_off), n) };
        }

        proc.map(page, phys.as_u64(), true, exec);
        page += 4096;
    }
    Ok(())
}
