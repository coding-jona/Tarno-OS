// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — a minimal PE32+ (x86-64) loader.
//!
//! Maps a Win64 `.exe`, resolves its imports, and jumps to its entry point in
//! ring 3. Base relocations (`IMAGE_REL_BASED_DIR64`) are applied, so an image
//! that ships a `.reloc` section can load away from its preferred `ImageBase`.
//! Imports bind against the two synthetic system modules (`kernel32.dll`,
//! `ntdll.dll` — trampoline pages built in-kernel) **and** against real on-disk
//! PE DLLs read from `C:\Windows\System32`: [`Loader`] stages each dependency
//! (parse → relocate → parse exports → recurse into *its* imports → map) into a
//! bump arena of virtual space and binds the IAT to the real export addresses.
//! `DllMain` is not run yet.
//!
//! **Every input is treated as hostile.** A malformed header, an out-of-range
//! offset, an overflowing size — all produce `Err`, never a slice panic.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use x86_64::PhysAddr;

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

/// On-disk DLLs load into a bump arena of virtual space, one 64 KiB-aligned
/// slot per module, clear of the exe, the user stack, and the synthetic pages.
const PE_DLL_ARENA: u64 = NT_STUB_BASE + 0x0080_0000;
const PE_DLL_ARENA_END: u64 = 0x0000_7FF8_0000_0000;
/// Windows `\Windows\System32` maps here on THOS's fs.
const SYSTEM32_DIR: &str = "/Windows/System32";
/// Cap on transitive DLL dependency depth.
const MAX_DLL_DEPTH: u32 = 8;

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
/// One page holding `RTL_USER_PROCESS_PARAMETERS`, `PEB_LDR_DATA`, the
/// `LDR_DATA_TABLE_ENTRY`s (the exe + synthetic kernel32), plus their strings.
const PE_PARAMS_ADDR: u64 = NT_STUB_BASE + 0x3000;
const PARAMS_OFF: u64 = 0x000; // RTL_USER_PROCESS_PARAMETERS
const LDR_OFF: u64 = 0x100; // PEB_LDR_DATA
const MOD_OFF: u64 = 0x180; // LDR_DATA_TABLE_ENTRY (the exe)
const WSTR_OFF: u64 = 0x200; // UTF-16 strings
const ANSI_CMDLINE_OFF: u64 = 0x400; // ANSI command line
const ENV_OFF: u64 = 0x500; // environment block (empty)
const MOD2_OFF: u64 = 0x600; // LDR_DATA_TABLE_ENTRY (synthetic kernel32)
const MOD3_OFF: u64 = 0x700; // LDR_DATA_TABLE_ENTRY (synthetic ntdll)

/// A dedicated page for the `LDR_DATA_TABLE_ENTRY`s of DLLs loaded from disk
/// (the params page is full). Entries grow up from 0; their UTF-16 names live
/// in the back half. Enough for a first cut; a multi-page region comes later.
const PE_LDRDATA_ADDR: u64 = NT_STUB_BASE + 0x6000;
const LDR_ENTRY_STRIDE: u64 = 0x80;
const LDRDATA_STR_OFF: usize = 0x800;
const MAX_FILE_LDR_MODS: usize = 12;

/// A ring-3 process-bootstrap page. When any file DLL has an entry point, the
/// PE thread starts here instead of at the exe entry: it calls each
/// `DllMain(base, DLL_PROCESS_ATTACH, 1)` in dependency order, then jumps to
/// the real exe entry. This is the loader's job on Windows (`LdrpInitializeProcess`).
const PE_BOOTSTRAP_ADDR: u64 = NT_STUB_BASE + 0x7000;
const BOOTSTRAP_LIST_OFF: usize = 0x400; // {base,entry} u64 pairs, {0,0}-terminated
const BOOTSTRAP_ENTRY_OFF: usize = 0x800; // real exe entry VA

/// One page holding static-TLS storage: a `ThreadLocalStoragePointer` array
/// (`TEB+0x58` points here), then one zeroed+templated TLS block per module
/// with a `.tls` section. Enough for the exe + a few DLLs; one thread per PE
/// process, no dynamic `TlsAlloc` yet.
const PE_TLS_ADDR: u64 = NT_STUB_BASE + 0x8000;
const MAX_TLS_MODS: usize = 24;
const TLS_BLOCKS_OFF: usize = 0x100; // blocks start here; ptr array is [0, 0x100)

/// Synthetic system DLLs: one page each carrying a minimal PE32+ header, a
/// copy of the NT trampolines, and a real `IMAGE_EXPORT_DIRECTORY` naming
/// each. Both are present in the PEB `Ldr` lists so `GetModuleHandleA` /
/// `LoadLibraryA` hand back their bases and `GetProcAddress` /
/// `LdrGetProcedureAddress` resolve names against them; a PE's IAT is bound
/// straight into the owning module page (as on real Windows).
const PE_KERNEL32_ADDR: u64 = NT_STUB_BASE + 0x4000;
const PE_NTDLL_ADDR: u64 = NT_STUB_BASE + 0x5000;

/// Synthetic `msvcrt.dll` trampoline page (rwx — `_fmode` / `_commode` /
/// `__initenv` are data exports the CRT writes) + its rw scratch page
/// (`__iob_func` FILE array, `errno`, `lconv`, `__getmainargs` argv).
const PE_MSVCRT_ADDR: u64 = NT_STUB_BASE + 0xE000;
pub const PE_CRT_ADDR: u64 = NT_STUB_BASE + 0xF000;
/// r-x stub for `msvcrt!_initterm` — a ring-3 loop that calls each non-null
/// function pointer in `[pfbegin, pfend)` (C++ static ctors / mingw's own
/// init, incl. the one that fills `_argv`). Bound in place of a syscall
/// trampoline by [`Loader::new`].
pub const PE_INITTERM_ADDR: u64 = NT_STUB_BASE + 0x10000;

/// A single worker thread's TEB + entry stub + stack. One extra thread per PE
/// process for now (fixed regions); a real per-thread allocator comes later.
const PE_TEB2_ADDR: u64 = NT_STUB_BASE + 0xC000;
const PE_THREADSTART_ADDR: u64 = NT_STUB_BASE + 0xD000;
const PE_THREAD_STACK_ADDR: u64 = NT_STUB_BASE + 0x0010_0000;
const PE_THREAD_STACK_BYTES: u64 = 0x8000; // 32 KiB
const SYNTH_STUBS_OFF: u64 = 0x180; // trampoline table within a synth DLL page
const SYNTH_EXPDIR_OFF: u64 = 0x400; // IMAGE_EXPORT_DIRECTORY within the page
/// `GetCommandLineA` returns this.
pub const PE_ANSI_CMDLINE_ADDR: u64 = PE_PARAMS_ADDR + ANSI_CMDLINE_OFF;
const ANSI_CMDLINE: &[u8] = b"PE argv0 pe-hello.exe\n\0";

/// Reduce a raw import DLL name to its lowercase base name with a `.dll`
/// extension (`"..\\KERNEL32"` → `"kernel32.dll"`).
fn normalize_dll_name(raw: &str) -> String {
    let base = raw
        .trim_end_matches('\0')
        .rsplit(|c| c == '\\' || c == '/')
        .next()
        .unwrap_or(raw);
    let mut s = base.to_ascii_lowercase();
    if !s.contains('.') {
        s.push_str(".dll");
    }
    s
}

/// One `AddressOfFunctions` slot of a module's export table.
enum Export {
    Empty,
    Addr(u64),       // resolved absolute VA
    Forward(String), // "TargetDll.TargetFunc" or "TargetDll.#123"
}

/// A module already resolved for the process: a synthetic system DLL or a real
/// file loaded from System32. `eat` holds each export indexed by
/// `ordinal - ord_base`; `names` maps an export name to that index. An importer
/// binds by name or by ordinal without touching mapped memory; forwarders are
/// followed through [`Loader::resolve_export_idx`].
struct LoadedModule {
    name: String, // lowercase base name incl. ".dll"
    #[allow(dead_code)]
    full: String, // "C:\\Windows\\System32\\NAME"
    base: u64,
    size: u64,
    entry: u64, // 0 = none / not run
    eat: Vec<Export>,
    ord_base: u32,
    names: BTreeMap<String, usize>,
    is_file: bool,
}

/// A module's parsed `IMAGE_TLS_DIRECTORY` (fields are runtime VAs — already
/// fixed up by `apply_relocs` in the staged image).
struct TlsInfo {
    raw_start_va: u64, // template data [raw_start, raw_end)
    raw_end_va: u64,
    index_ptr_va: u64, // DWORD the loader writes the module's TLS index into
    callbacks_va: u64, // NULL-terminated array of PIMAGE_TLS_CALLBACK
    zero_fill: u32,
}

/// Static-TLS accumulation across the exe + its DLLs during one `load()`.
struct TlsBuild {
    frame_phys: u64, // 0 = the TLS page has not been allocated
    n_mods: usize,   // = next TLS index
    blk_next: usize, // next free offset within the TLS page
}

/// Per-`load()` DLL resolver: owns the dependency graph, the VA arena the
/// on-disk DLLs map into, and the static-TLS layout.
struct Loader<'a> {
    proc: &'a Process,
    fs: Option<crate::ext2::Ext2>,
    arena_next: u64,
    mods: Vec<LoadedModule>,
    depth: u32,
    tls: TlsBuild,
    tls_cbs: Vec<(u64, u64)>, // (module base, callback VA) run at process start
}

impl<'a> Loader<'a> {
    /// Seed with the two synthetic system modules (already mapped by
    /// `map_kernel32_page` / `map_ntdll_page`).
    fn new(proc: &'a Process) -> Self {
        let mut mods = Vec::new();
        for (name, base, table) in [
            ("kernel32.dll", PE_KERNEL32_ADDR, &crate::nt::NT_EXPORTS[..]),
            ("ntdll.dll", PE_NTDLL_ADDR, &crate::nt::NTDLL_EXPORTS[..]),
            ("msvcrt.dll", PE_MSVCRT_ADDR, &crate::nt::MSVCRT_EXPORTS[..]),
        ] {
            // Matches `map_synth_dll`'s export directory: Base 1, EAT[i] = stub i.
            let mut eat = Vec::with_capacity(table.len());
            let mut names = BTreeMap::new();
            for (i, &fname) in table.iter().enumerate() {
                eat.push(Export::Addr(base + SYNTH_STUBS_OFF + i as u64 * NT_STUB_STRIDE));
                names.insert(String::from(fname), i);
            }
            mods.push(LoadedModule {
                full: format!("C:\\Windows\\System32\\{}", name.to_ascii_uppercase()),
                name: String::from(name),
                base,
                size: 0x1000,
                entry: 0,
                eat,
                ord_base: 1,
                names,
                is_file: false,
            });
        }
        // `msvcrt!_initterm` is a userspace loop, not a syscall — point it at the
        // r-x stub page instead of trampoline 12.
        if let Some(m) = mods.iter_mut().find(|m| m.name == "msvcrt.dll") {
            m.eat[12] = Export::Addr(PE_INITTERM_ADDR);
        }
        Loader {
            proc,
            fs: None,
            arena_next: PE_DLL_ARENA,
            mods,
            depth: 0,
            tls: TlsBuild { frame_phys: 0, n_mods: 0, blk_next: TLS_BLOCKS_OFF },
            tls_cbs: Vec::new(),
        }
    }

    /// Give one module (exe or DLL) its static-TLS block: allocate a per-thread
    /// copy of the template, record it in the `ThreadLocalStoragePointer`
    /// array, patch the module's `AddressOfIndex` DWORD in `img` (before it is
    /// mapped), and queue its TLS callbacks. `img` is the staged image, `base`
    /// its load address.
    fn tls_add_module(
        &mut self,
        base: u64,
        img: &mut [u8],
        t: &TlsInfo,
    ) -> Result<(), &'static str> {
        if self.tls.frame_phys == 0 {
            let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (TLS)")?;
            let phys = frame.start_address();
            unsafe { core::ptr::write_bytes(phys_to_virt(phys).as_mut_ptr::<u8>(), 0, 4096) };
            self.tls.frame_phys = phys.as_u64();
        }
        if self.tls.n_mods >= MAX_TLS_MODS {
            return Err("PE: too many TLS modules");
        }
        let raw_off = t.raw_start_va.checked_sub(base).ok_or("PE: TLS data before base")? as usize;
        let raw_len = t.raw_end_va.checked_sub(t.raw_start_va).ok_or("PE: bad TLS range")? as usize;
        let total = raw_len + t.zero_fill as usize;
        let blk = self.tls.blk_next;
        if blk + total > 4096 {
            return Err("PE: static TLS overflows its page");
        }
        let src = img.get(raw_off..raw_off + raw_len).ok_or("PE: TLS template out of range")?;
        let page = phys_to_virt(PhysAddr::new(self.tls.frame_phys)).as_mut_ptr::<u8>();
        let idx = self.tls.n_mods;
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), page.add(blk), raw_len);
            // ptr array entry: ThreadLocalStoragePointer[idx] = block VA
            *(page.add(idx * 8) as *mut u64) = PE_TLS_ADDR + blk as u64;
        }
        // the module's __tls_index
        let ix_off = t.index_ptr_va.checked_sub(base).ok_or("PE: TLS index ptr before base")? as usize;
        img.get_mut(ix_off..ix_off + 4)
            .ok_or("PE: TLS index ptr out of range")?
            .copy_from_slice(&(idx as u32).to_le_bytes());

        // TLS callbacks — an array of fn VAs at callbacks_va, NUL-terminated.
        if t.callbacks_va != 0 {
            let mut off = t.callbacks_va.checked_sub(base).ok_or("PE: TLS callbacks before base")? as usize;
            for _ in 0..64 {
                let fnva = match img.get(off..off + 8) {
                    Some(b) => u64::from_le_bytes(b.try_into().unwrap()),
                    None => break,
                };
                if fnva == 0 {
                    break;
                }
                self.tls_cbs.push((base, fnva));
                off += 8;
            }
        }

        self.tls.n_mods += 1;
        self.tls.blk_next = (blk + total + 15) & !15;
        Ok(())
    }

    fn fs(&mut self) -> Result<&crate::ext2::Ext2, &'static str> {
        if self.fs.is_none() {
            self.fs = Some(crate::ext2::open().map_err(|_| "PE: cannot mount fs for DLL load")?);
        }
        Ok(self.fs.as_ref().unwrap())
    }

    fn arena_alloc(&mut self, size_of_image: u64) -> Result<u64, &'static str> {
        let base = self.arena_next;
        let span = (size_of_image + 0xFFFF) & !0xFFFF;
        let end = base
            .checked_add(span)
            .filter(|&e| e < PE_DLL_ARENA_END)
            .ok_or("PE: DLL arena exhausted")?;
        self.arena_next = end;
        Ok(base)
    }

    /// Index into `self.mods` for `raw_name`, loading it from System32 on a miss.
    fn resolve_module(&mut self, raw_name: &str) -> Result<usize, &'static str> {
        let name = normalize_dll_name(raw_name);
        if let Some(i) = self.mods.iter().position(|m| m.name == name) {
            return Ok(i);
        }
        if self.depth >= MAX_DLL_DEPTH {
            return Err("PE: DLL dependency chain too deep");
        }
        let path = format!("{SYSTEM32_DIR}/{name}");
        let bytes = self.fs()?.read_path(&path).ok_or("PE: DLL not found in System32")?;
        self.depth += 1;
        let r = self.load_dll(&name, &bytes);
        self.depth -= 1;
        r
    }

    /// Stage, register, recursively bind, and map one on-disk DLL.
    fn load_dll(&mut self, name: &str, bytes: &[u8]) -> Result<usize, &'static str> {
        let base = self.arena_alloc(peek_size_of_image(bytes)?)?;
        let mut staged = stage_image(bytes, Some(base))?;
        let (eat, ord_base, names) =
            parse_export_table(&staged.img, base, staged.export_rva, staged.export_size)?;

        // Register before recursing so an import cycle terminates; the export
        // VAs are final already (arena base + RVA), no mapping needed to bind.
        let idx = self.mods.len();
        self.mods.push(LoadedModule {
            name: String::from(name),
            full: format!("C:\\Windows\\System32\\{name}"),
            base,
            size: staged.size_of_image,
            entry: if staged.entry != base { staged.entry } else { 0 },
            eat,
            ord_base,
            names,
            is_file: true,
        });

        if staged.import_size != 0 {
            self.bind_imports(&mut staged.img, staged.import_rva as u64, staged.size_of_image)?;
        }
        if let Some(t) = staged.tls.take() {
            self.tls_add_module(base, &mut staged.img, &t)?;
        }
        map_seg(self.proc, &staged.img, base, 0, staged.hdr_n, false)?;
        for &(rva, mem_size, exec) in &staged.segs {
            map_seg(self.proc, &staged.img, base + rva, rva, mem_size, exec)?;
        }
        Ok(idx)
    }

    /// Walk an image's Import Directory Table and overwrite each IAT slot with
    /// the target VA, pulling in dependency DLLs from System32 as needed.
    fn bind_imports(
        &mut self,
        img: &mut [u8],
        import_rva: u64,
        size_of_image: u64,
    ) -> Result<(), &'static str> {
        let mut idt = import_rva as usize;
        loop {
            if (idt + 20) as u64 > size_of_image || idt + 20 > img.len() {
                return Err("PE: import table truncated");
            }
            let ilt_rva = u32::from_le_bytes(img[idt..idt + 4].try_into().unwrap()) as u64;
            let name_rva = u32::from_le_bytes(img[idt + 12..idt + 16].try_into().unwrap()) as u64;
            let iat_rva = u32::from_le_bytes(img[idt + 16..idt + 20].try_into().unwrap()) as u64;
            if name_rva == 0 && iat_rva == 0 {
                return Ok(()); // null terminator
            }
            let dll = String::from(cstr_at(img, name_rva)?);
            let midx = self.resolve_module(&dll)?;

            let names_rva = if ilt_rva != 0 { ilt_rva } else { iat_rva };
            let mut i = 0u64;
            loop {
                let t_off = (names_rva + i * 8) as usize;
                let p_off = (iat_rva + i * 8) as usize;
                if (t_off + 8) as u64 > size_of_image
                    || t_off + 8 > img.len()
                    || (p_off + 8) as u64 > size_of_image
                    || p_off + 8 > img.len()
                {
                    return Err("PE: import thunk out of range");
                }
                let thunk = u64::from_le_bytes(img[t_off..t_off + 8].try_into().unwrap());
                if thunk == 0 {
                    break;
                }
                let addr = if thunk & 0x8000_0000_0000_0000 != 0 {
                    let ord = (thunk & 0xFFFF) as u16;
                    self.resolve_export_ordinal(midx, ord, 0).map_err(|e| {
                        crate::kprintln!("THOS: pe unresolved    {}#{}", dll.as_str(), ord);
                        e
                    })?
                } else {
                    let func = cstr_at(img, (thunk & 0x7FFF_FFFF) + 2)?; // skip the 2-byte hint
                    self.resolve_export_name(midx, func, 0).map_err(|e| {
                        crate::kprintln!("THOS: pe unresolved    {}!{}", dll.as_str(), func);
                        e
                    })?
                };
                img[p_off..p_off + 8].copy_from_slice(&addr.to_le_bytes());
                i += 1;
            }
            idt += 20;
        }
    }

    /// Absolute VA of `mods[midx]`'s export `name`, following forwarders.
    fn resolve_export_name(
        &mut self,
        midx: usize,
        name: &str,
        depth: u32,
    ) -> Result<u64, &'static str> {
        let idx = *self.mods[midx].names.get(name).ok_or("PE: name not exported")?;
        self.resolve_export_idx(midx, idx, depth)
    }

    /// Absolute VA of `mods[midx]`'s export with ordinal `ord`, following forwarders.
    fn resolve_export_ordinal(
        &mut self,
        midx: usize,
        ord: u16,
        depth: u32,
    ) -> Result<u64, &'static str> {
        let i = (ord as u32)
            .checked_sub(self.mods[midx].ord_base)
            .ok_or("PE: ordinal below Base")? as usize;
        self.resolve_export_idx(midx, i, depth)
    }

    fn resolve_export_idx(
        &mut self,
        midx: usize,
        i: usize,
        depth: u32,
    ) -> Result<u64, &'static str> {
        if depth > 8 {
            return Err("PE: forwarder chain too deep");
        }
        match self.mods[midx].eat.get(i).ok_or("PE: ordinal out of range")? {
            Export::Addr(a) => Ok(*a),
            Export::Empty => Err("PE: empty export slot"),
            Export::Forward(s) => {
                let s = s.clone(); // about to mutate self.mods
                let (tgt_dll, tgt_fn) = s.rsplit_once('.').ok_or("PE: bad forwarder")?;
                let tmidx = self.resolve_module(tgt_dll)?;
                match tgt_fn.strip_prefix('#') {
                    Some(n) => {
                        let ord = n.parse::<u16>().map_err(|_| "PE: bad forwarder ordinal")?;
                        self.resolve_export_ordinal(tmidx, ord, depth + 1)
                    }
                    None => self.resolve_export_name(tmidx, tgt_fn, depth + 1),
                }
            }
        }
    }
}

/// Peek `SizeOfImage` from a PE file without a full parse (arena sizing).
fn peek_size_of_image(file: &[u8]) -> Result<u64, &'static str> {
    let pe_off = rd_u32(file, 0x3C)? as usize;
    if file.get(pe_off..pe_off + 4) != Some(b"PE\0\0") {
        return Err("PE: no PE signature");
    }
    let soi = rd_u32(file, pe_off + 4 + 20 + 56)? as u64;
    if soi == 0 || soi > MAX_IMAGE_SIZE {
        return Err("PE: absurd SizeOfImage");
    }
    Ok(soi)
}

/// Parse an `IMAGE_EXPORT_DIRECTORY` out of a materialised image. Returns
/// `(eat, ord_base, names)`: `eat[i]` is the export with ordinal `ord_base + i`
/// (an [`Export::Addr`], an [`Export::Forward`] string when the RVA points back
/// inside the export directory, or [`Export::Empty`]); `names` maps an export
/// name to its `eat` index.
#[allow(clippy::type_complexity)]
fn parse_export_table(
    img: &[u8],
    base: u64,
    export_rva: u32,
    export_size: u32,
) -> Result<(Vec<Export>, u32, BTreeMap<String, usize>), &'static str> {
    let mut names = BTreeMap::new();
    if export_rva == 0 {
        return Ok((Vec::new(), 1, names));
    }
    let ed = export_rva as usize;
    let ord_base = rd_u32(img, ed + 0x10)?;
    let n_funcs = rd_u32(img, ed + 0x14)? as usize;
    let n_names = rd_u32(img, ed + 0x18)? as usize;
    let eat_rva = rd_u32(img, ed + 0x1C)? as usize;
    let enpt_rva = rd_u32(img, ed + 0x20)? as usize;
    let ords_rva = rd_u32(img, ed + 0x24)? as usize;
    if n_funcs > 64 * 1024 {
        return Err("PE: absurd export count");
    }

    let mut eat = Vec::with_capacity(n_funcs);
    for i in 0..n_funcs {
        let frva = rd_u32(img, eat_rva + i * 4)? as u64;
        eat.push(if frva == 0 {
            Export::Empty
        } else if frva >= export_rva as u64 && frva < export_rva as u64 + export_size as u64 {
            Export::Forward(String::from(cstr_at(img, frva)?))
        } else {
            Export::Addr(base + frva)
        });
    }
    for i in 0..n_names {
        let name = String::from(cstr_at(img, rd_u32(img, enpt_rva + i * 4)? as u64)?);
        let idx = rd_u16(img, ords_rva + i * 2)? as usize;
        if idx < eat.len() {
            names.insert(name, idx);
        }
    }
    Ok((eat, ord_base, names))
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

/// One image (exe or DLL) parsed, materialised at RVA 0, and relocated to
/// `load_base` — but not yet mapped, and with its IAT not yet bound.
struct StagedImage {
    load_base: u64,
    size_of_image: u64,
    entry: u64, // absolute; == load_base when AddressOfEntryPoint is 0
    hdr_n: u64,
    img: Vec<u8>,
    segs: Vec<(u64, u64, bool)>, // (rva, mem_size, exec)
    import_rva: u32,
    import_size: u32,
    export_rva: u32,
    export_size: u32,
    tls: Option<TlsInfo>,
}

/// Parse + materialise + relocate `file`. `want_base` fixes the load address
/// (a DLL arena slot); `None` uses the preferred `ImageBase`, shifting only if
/// the image opted into relocation.
fn stage_image(file: &[u8], want_base: Option<u64>) -> Result<StagedImage, &'static str> {
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
    let (tls_rva, tls_size) = dir(9)?;
    let (export_rva, export_size) = dir(0)?;
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

    // Where does it load? A DLL takes the arena slot the caller picked; an exe
    // keeps its preferred `ImageBase` unless it opted into relocation, in which
    // case it is shifted to exercise the fixup path.
    let relocatable = reloc_size != 0 && dll_chars & IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE != 0;
    let load_base = match want_base {
        Some(b) => b,
        None if relocatable => image_base
            .checked_add(ALT_BASE_SHIFT)
            .filter(|b| b + size_of_image < 0x0000_8000_0000_0000)
            .ok_or("PE: no room for an alternative base")?,
        None => image_base,
    };
    let delta = load_base.wrapping_sub(image_base);
    if delta != 0 {
        if reloc_size == 0 {
            return Err("PE: needs relocation but has no .reloc");
        }
        apply_relocs(&mut img, reloc_rva as u64, reloc_size as u64, delta, size_of_image)?;
    }

    // The TLS directory's address fields are VAs — read them *after* relocation,
    // so they already hold runtime addresses.
    let tls = if tls_size != 0 {
        let d = tls_rva as usize;
        if d + 40 > img.len() {
            return Err("PE: TLS directory out of range");
        }
        Some(TlsInfo {
            raw_start_va: rd_u64(&img, d)?,
            raw_end_va: rd_u64(&img, d + 8)?,
            index_ptr_va: rd_u64(&img, d + 16)?,
            callbacks_va: rd_u64(&img, d + 24)?,
            zero_fill: rd_u32(&img, d + 32)?,
        })
    } else {
        None
    };

    Ok(StagedImage {
        load_base,
        size_of_image,
        entry: load_base + entry_rva,
        hdr_n: hdr_n as u64,
        img,
        segs,
        import_rva,
        import_size,
        export_rva,
        export_size,
        tls,
    })
}

/// Load a Win64 `.exe`: stage it, resolve its imports (pulling in synthetic
/// system modules and real DLLs from `C:\Windows\System32`), map everything,
/// and lay out the TEB / PEB.
/// A DLL loaded from disk, as the PEB `Ldr` list needs it.
struct LdrFileMod {
    base: u64,
    entry: u64,
    size: u64,
    name: String, // lowercase base name incl. ".dll"
}

pub fn load(proc: &Process, file: &[u8], stack_top: u64) -> Result<PeImage, &'static str> {
    let mut ldr = Loader::new(proc);
    let mut staged = stage_image(file, None)?;

    if staged.import_size != 0 {
        ldr.bind_imports(&mut staged.img, staged.import_rva as u64, staged.size_of_image)?;
    }
    if let Some(t) = staged.tls.take() {
        ldr.tls_add_module(staged.load_base, &mut staged.img, &t)?;
    }

    // Map the exe (headers first, sections win on any overlap).
    map_seg(proc, &staged.img, staged.load_base, 0, staged.hdr_n, false)?;
    for &(rva, mem_size, exec) in &staged.segs {
        map_seg(proc, &staged.img, staged.load_base + rva, rva, mem_size, exec)?;
    }
    map_kernel32_page(proc)?;
    map_ntdll_page(proc)?;
    map_msvcrt_page(proc)?;
    map_crt_page(proc)?;
    map_initterm_page(proc)?;
    map_seh_pages(proc)?;
    map_apc_page(proc)?;
    map_thread_start_page(proc)?;

    // Static-TLS page (if any module used `.tls`); TEB.ThreadLocalStoragePointer
    // gets its VA below.
    let tls_ptr = if ldr.tls.frame_phys != 0 {
        proc.map(PE_TLS_ADDR, ldr.tls.frame_phys, true, false); // rw, no-exec
        PE_TLS_ADDR
    } else {
        0
    };

    let file_mods: Vec<LdrFileMod> = ldr
        .mods
        .iter()
        .filter(|m| m.is_file)
        .map(|m| LdrFileMod { base: m.base, entry: m.entry, size: m.size, name: m.name.clone() })
        .collect();
    if file_mods.len() > MAX_FILE_LDR_MODS {
        crate::kprintln!(
            "THOS: pe warn          {} file DLLs, only {} land in the Ldr list",
            file_mods.len(),
            MAX_FILE_LDR_MODS
        );
    }
    map_teb_peb(
        proc,
        staged.load_base,
        staged.entry,
        staged.size_of_image,
        stack_top,
        &file_mods,
        tls_ptr,
    )?;

    // Ring-3 process init, in order: every TLS callback, then every `DllMain`
    // (dependency order — a DLL is registered before its own file-DLL imports,
    // so reverse of load order is deps-first). All share the
    // `(hinst, DLL_PROCESS_ATTACH, 1)` signature.
    let n_tls = ldr.tls_cbs.len();
    let mut init: Vec<(u64, u64)> = ldr.tls_cbs.clone();
    init.extend(
        file_mods
            .iter()
            .rev()
            .filter(|m| m.entry != 0)
            .map(|m| (m.base, m.entry)),
    );
    let entry = if init.is_empty() {
        staged.entry
    } else {
        map_bootstrap(proc, &init, n_tls, staged.entry)?;
        PE_BOOTSTRAP_ADDR
    };

    Ok(PeImage {
        entry,
        teb: PE_TEB_ADDR,
        base: staged.load_base,
        size: staged.size_of_image,
    })
}

/// Build the ring-3 process-bootstrap page: a small loop that calls every
/// `init` entry as `fn(module_base, DLL_PROCESS_ATTACH=1, lpvReserved=1)` —
/// TLS callbacks then `DllMain`s — and then jumps to `exe_entry`. Mapped exec
/// + read-only at [`PE_BOOTSTRAP_ADDR`].
fn map_bootstrap(
    proc: &Process,
    init: &[(u64, u64)],
    n_tls: usize,
    exe_entry: u64,
) -> Result<(), &'static str> {
    let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (bootstrap)")?;
    let phys = frame.start_address();
    let page = unsafe {
        let p = phys_to_virt(phys).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        core::slice::from_raw_parts_mut(p, 4096)
    };

    // {base, entry} pairs then a {0,0} terminator. The first `n_tls` are TLS
    // callbacks (return value ignored); the rest are DllMains (a `FALSE`
    // return aborts process init).
    let mut o = BOOTSTRAP_LIST_OFF;
    for &(base, ent) in init {
        page[o..o + 8].copy_from_slice(&base.to_le_bytes());
        page[o + 8..o + 16].copy_from_slice(&ent.to_le_bytes());
        o += 16;
    }
    if o + 16 > BOOTSTRAP_ENTRY_OFF {
        return Err("PE: too many init routines for the bootstrap page");
    }
    page[BOOTSTRAP_ENTRY_OFF..BOOTSTRAP_ENTRY_OFF + 8].copy_from_slice(&exe_entry.to_le_bytes());

    // code @ offset 0:
    //   rbx = &list; r13 = n_tls; r14 = index
    //   loop: base = [rbx]; if base == 0 -> jmp exe_entry
    //         call [rbx+8](base, DLL_PROCESS_ATTACH, 1)
    //         if r14 >= r13 && eax == 0 -> ExitProcess(STATUS-ish)
    //         r14++; rbx += 16; loop
    let mut c: Vec<u8> = Vec::new();
    c.extend_from_slice(&[0x48, 0xBB]); // mov rbx, imm64  (list)
    c.extend_from_slice(&(PE_BOOTSTRAP_ADDR + BOOTSTRAP_LIST_OFF as u64).to_le_bytes());
    c.extend_from_slice(&[0x41, 0xBD]); // mov r13d, imm32  (n_tls)
    c.extend_from_slice(&(n_tls as u32).to_le_bytes());
    c.extend_from_slice(&[0x45, 0x31, 0xF6]); // xor r14d, r14d  (index)
    let loop_start = c.len();
    c.extend_from_slice(&[0x48, 0x8B, 0x03]); // mov rax, [rbx]
    c.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
    let jz_at = c.len();
    c.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]); // jz .done
    c.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
    c.extend_from_slice(&[0xBA, 1, 0, 0, 0]); // mov edx, 1
    c.extend_from_slice(&[0x41, 0xB8, 1, 0, 0, 0]); // mov r8d, 1
    c.extend_from_slice(&[0x48, 0x8B, 0x73, 0x08]); // mov rsi, [rbx+8]
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    c.extend_from_slice(&[0xFF, 0xD6]); // call rsi
    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    c.extend_from_slice(&[0x45, 0x39, 0xEE]); // cmp r14d, r13d
    c.extend_from_slice(&[0x72, 0x04]); // jb .next   (index < n_tls: a TLS callback, ignore eax)
    c.extend_from_slice(&[0x85, 0xC0]); // test eax, eax
    c.extend_from_slice(&[0x74, 0x0C]); // jz .fail   (DllMain returned FALSE -> 12 bytes to .fail)
    // .next:
    c.extend_from_slice(&[0x41, 0xFF, 0xC6]); // inc r14d
    c.extend_from_slice(&[0x48, 0x83, 0xC3, 0x10]); // add rbx, 16
    let jmp_at = c.len();
    c.extend_from_slice(&[0xE9, 0, 0, 0, 0]); // jmp .loop
    c[jmp_at + 1..jmp_at + 5]
        .copy_from_slice(&((loop_start as i64 - (jmp_at as i64 + 5)) as i32).to_le_bytes());
    // .fail: ExitProcess(0x135) via the NT_EXITPROCESS selector, inline.
    c.extend_from_slice(&[0xB9, 0x35, 0x01, 0, 0]); // mov ecx, 0x135  (exit code)
    let sel = (crate::nt::NT_BASE | crate::nt::NT_EXITPROCESS as u64) as u32;
    c.extend_from_slice(&[0xB8]); // mov eax, imm32
    c.extend_from_slice(&sel.to_le_bytes());
    c.extend_from_slice(&[0x49, 0x89, 0xCA]); // mov r10, rcx
    c.extend_from_slice(&[0x0F, 0x05]); // syscall
    c.extend_from_slice(&[0x0F, 0x0B]); // ud2  (unreachable)
    // .done:
    let done_at = c.len();
    c[jz_at + 2..jz_at + 6]
        .copy_from_slice(&((done_at as i64 - (jz_at as i64 + 6)) as i32).to_le_bytes());
    let mov_at = c.len();
    c.extend_from_slice(&[0x48, 0x8B, 0x05, 0, 0, 0, 0]); // mov rax, [rip+exe_entry_slot]
    c[mov_at + 3..mov_at + 7]
        .copy_from_slice(&((BOOTSTRAP_ENTRY_OFF as i64 - (mov_at as i64 + 7)) as i32).to_le_bytes());
    c.extend_from_slice(&[0xFF, 0xE0]); // jmp rax

    if c.len() > BOOTSTRAP_LIST_OFF {
        return Err("PE: bootstrap code overflow");
    }
    page[..c.len()].copy_from_slice(&c);

    proc.map(PE_BOOTSTRAP_ADDR, phys.as_u64(), false, true); // r-x
    Ok(())
}

/// Write a circular doubly-linked `LIST_ENTRY` ring (`Flink` at +0, `Blink` at
/// +8) for the nodes whose VA falls inside `[page_base, page_base+0x1000)`.
/// `nodes[0]` is the list head; the ring closes back to it.
fn link_ring(page: &mut [u8], page_base: u64, nodes: &[u64]) {
    let n = nodes.len();
    for (p, &va) in nodes.iter().enumerate() {
        if va < page_base || va >= page_base + 4096 {
            continue;
        }
        let off = (va - page_base) as usize;
        page[off..off + 8].copy_from_slice(&nodes[(p + 1) % n].to_le_bytes());
        page[off + 8..off + 16].copy_from_slice(&nodes[(p + n - 1) % n].to_le_bytes());
    }
}

/// Allocate + map the TEB and PEB pages and fill the few fields a Win64 entry /
/// CRT touches immediately, plus the `PEB->Ldr` module list (exe, the two
/// synthetic modules, then every DLL loaded from `C:\Windows\System32`).
fn map_teb_peb(
    proc: &Process,
    image_base: u64,
    entry: u64,
    size_of_image: u64,
    stack_top: u64,
    file_mods: &[LdrFileMod],
    tls_ptr: u64,
) -> Result<(), &'static str> {
    let n_file = file_mods.len().min(MAX_FILE_LDR_MODS);
    // The LIST_ENTRY VAs for list `i` (0=InLoadOrder, 1=InMemoryOrder,
    // 2=InInitOrder): head, exe, kernel32, ntdll, then each file DLL.
    let nodes_for = |i: u64| -> Vec<u64> {
        let mut v = Vec::with_capacity(4 + n_file);
        v.push(PE_PARAMS_ADDR + LDR_OFF + 0x10 + i * 0x10); // head
        v.push(PE_PARAMS_ADDR + MOD_OFF + i * 0x10); // exe
        v.push(PE_PARAMS_ADDR + MOD2_OFF + i * 0x10); // kernel32
        v.push(PE_PARAMS_ADDR + MOD3_OFF + i * 0x10); // ntdll
        for k in 0..n_file {
            v.push(PE_LDRDATA_ADDR + k as u64 * LDR_ENTRY_STRIDE + i * 0x10);
        }
        v
    };
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
        put(t, 0x58, tls_ptr); // ThreadLocalStoragePointer (0 = no static TLS)
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
        let (k32_full_buf, k32_full_len) = wstr(b, "C:\\Windows\\System32\\KERNEL32.DLL");
        let (k32_base_buf, k32_base_len) = wstr(b, "KERNEL32.DLL");
        let (ntd_full_buf, ntd_full_len) = wstr(b, "C:\\Windows\\System32\\NTDLL.DLL");
        let (ntd_base_buf, ntd_base_len) = wstr(b, "NTDLL.DLL");

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

        // --- PEB_LDR_DATA @ LDR_OFF. The three lists each thread
        //     head -> exe -> kernel32 -> ntdll -> file DLLs -> head. The head
        //     LIST_ENTRYs sit at LDR_OFF+0x10/0x20/0x30; an entry's own three
        //     are at its base +0x00/0x10/0x20. `link_ring` writes the links for
        //     whichever page each node lives in; the file-DLL entries and their
        //     links are filled in the separate PE_LDRDATA page below. ---
        put(b, LDR_OFF + 0x00, 0x58); // Length
        b[(LDR_OFF + 0x04) as usize] = 1; // Initialized
        for i in 0..3u64 {
            link_ring(b, PE_PARAMS_ADDR, &nodes_for(i));
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

        put(b, MOD2_OFF + 0x30, PE_KERNEL32_ADDR); // DllBase
        put(b, MOD2_OFF + 0x38, 0); // EntryPoint (none)
        put(b, MOD2_OFF + 0x40, 0x1000); // SizeOfImage
        put16(b, MOD2_OFF + 0x48, k32_full_len); // FullDllName
        put16(b, MOD2_OFF + 0x4A, k32_full_len + 2);
        put(b, MOD2_OFF + 0x50, k32_full_buf);
        put16(b, MOD2_OFF + 0x58, k32_base_len); // BaseDllName
        put16(b, MOD2_OFF + 0x5A, k32_base_len + 2);
        put(b, MOD2_OFF + 0x60, k32_base_buf);

        put(b, MOD3_OFF + 0x30, PE_NTDLL_ADDR); // DllBase
        put(b, MOD3_OFF + 0x38, 0); // EntryPoint (none)
        put(b, MOD3_OFF + 0x40, 0x1000); // SizeOfImage
        put16(b, MOD3_OFF + 0x48, ntd_full_len); // FullDllName
        put16(b, MOD3_OFF + 0x4A, ntd_full_len + 2);
        put(b, MOD3_OFF + 0x50, ntd_full_buf);
        put16(b, MOD3_OFF + 0x58, ntd_base_len); // BaseDllName
        put16(b, MOD3_OFF + 0x5A, ntd_base_len + 2);
        put(b, MOD3_OFF + 0x60, ntd_base_buf);

        // --- ANSI command line (GetCommandLineA) + empty environment ---
        b[ANSI_CMDLINE_OFF as usize..ANSI_CMDLINE_OFF as usize + ANSI_CMDLINE.len()]
            .copy_from_slice(ANSI_CMDLINE);
        // ENV_OFF: leave the two NUL words already zeroed
    })?;

    // --- PE_LDRDATA page: one LDR_DATA_TABLE_ENTRY per on-disk DLL, plus its
    //     UTF-16 names, plus this page's share of the three ring links. ---
    if n_file != 0 {
        w(PE_LDRDATA_ADDR, &|d| {
            let mut scur = LDRDATA_STR_OFF;
            let mut wput = |d: &mut [u8], s: &str| -> (u64, u16) {
                let va = PE_LDRDATA_ADDR + scur as u64;
                let mut n = 0usize;
                for u in s.encode_utf16() {
                    d[scur + n..scur + n + 2].copy_from_slice(&u.to_le_bytes());
                    n += 2;
                }
                d[scur + n..scur + n + 2].copy_from_slice(&[0, 0]);
                scur += n + 2;
                scur = (scur + 1) & !1;
                (va, n as u16)
            };
            for (k, m) in file_mods.iter().take(n_file).enumerate() {
                let e = k * LDR_ENTRY_STRIDE as usize;
                let (full_buf, full_len) =
                    wput(d, &alloc::format!("C:\\Windows\\System32\\{}", m.name));
                let (base_buf, base_len) = wput(d, &m.name);
                d[e + 0x30..e + 0x38].copy_from_slice(&m.base.to_le_bytes()); // DllBase
                d[e + 0x38..e + 0x40].copy_from_slice(&m.entry.to_le_bytes()); // EntryPoint
                d[e + 0x40..e + 0x48].copy_from_slice(&(m.size & 0xFFFF_FFFF).to_le_bytes());
                d[e + 0x48..e + 0x4A].copy_from_slice(&full_len.to_le_bytes()); // FullDllName
                d[e + 0x4A..e + 0x4C].copy_from_slice(&(full_len + 2).to_le_bytes());
                d[e + 0x50..e + 0x58].copy_from_slice(&full_buf.to_le_bytes());
                d[e + 0x58..e + 0x5A].copy_from_slice(&base_len.to_le_bytes()); // BaseDllName
                d[e + 0x5A..e + 0x5C].copy_from_slice(&(base_len + 2).to_le_bytes());
                d[e + 0x60..e + 0x68].copy_from_slice(&base_buf.to_le_bytes());
            }
            for i in 0..3u64 {
                link_ring(d, PE_LDRDATA_ADDR, &nodes_for(i));
            }
        })?;
    }
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

/// The two SEH pages: a writable one-`PVOID` slot for the vectored handler,
/// and an r-x `KiUserExceptionDispatcher` — it runs the handler with a
/// `&EXCEPTION_POINTERS`, then `NtContinue`s (on `EXCEPTION_CONTINUE_EXECUTION`)
/// or `NtTerminateProcess`es.
fn map_seh_pages(proc: &Process) -> Result<(), &'static str> {
    // handler slot page (rw, zeroed)
    let f = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (seh slot)")?;
    unsafe { core::ptr::write_bytes(phys_to_virt(f.start_address()).as_mut_ptr::<u8>(), 0, 4096) };
    proc.map(crate::seh::PE_EXC_ADDR, f.start_address().as_u64(), true, false);

    // dispatcher code page (r-x)
    let f = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (KiUser)")?;
    let page = unsafe {
        let p = phys_to_virt(f.start_address()).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        core::slice::from_raw_parts_mut(p, 4096)
    };
    let cont = (crate::nt::NT_BASE | crate::nt::NT_NTDLL_FLAG as u64 | crate::nt::NT_NTCONTINUE as u64) as u32;
    let term = (crate::nt::NT_BASE
        | crate::nt::NT_NTDLL_FLAG as u64
        | crate::nt::NT_NTTERMINATEPROCESS as u64) as u32;
    let mut c: Vec<u8> = Vec::new();
    // entry: rcx = &EXCEPTION_RECORD, rdx = &CONTEXT
    c.extend_from_slice(&[0x53]); // push rbx
    c.extend_from_slice(&[0x48, 0x89, 0xD3]); // mov rbx, rdx  (save CONTEXT ptr)
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // sub rsp, 0x38
    c.extend_from_slice(&[0x48, 0x89, 0x4C, 0x24, 0x20]); // mov [rsp+0x20], rcx  (EP.ExceptionRecord)
    c.extend_from_slice(&[0x48, 0x89, 0x54, 0x24, 0x28]); // mov [rsp+0x28], rdx  (EP.ContextRecord)
    c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    c.extend_from_slice(&crate::seh::PE_EXC_ADDR.to_le_bytes());
    c.extend_from_slice(&[0x48, 0x8B, 0x00]); // mov rax, [rax]  (handler VA)
    c.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
    c.extend_from_slice(&[0x74, 0x1D]); // jz .term
    c.extend_from_slice(&[0x48, 0x8D, 0x4C, 0x24, 0x20]); // lea rcx, [rsp+0x20]  (&EXCEPTION_POINTERS)
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x83, 0xF8, 0xFF]); // cmp eax, -1  (EXCEPTION_CONTINUE_EXECUTION)
    c.extend_from_slice(&[0x75, 0x11]); // jne .term
    c.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx  (CONTEXT)
    c.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx
    c.extend_from_slice(&[0xB8]);
    c.extend_from_slice(&cont.to_le_bytes()); // mov eax, <NtContinue sel>
    c.extend_from_slice(&[0x49, 0x89, 0xCA, 0x0F, 0x05, 0x0F, 0x0B]); // mov r10,rcx; syscall; ud2
    // .term:
    c.extend_from_slice(&[0x48, 0x8B, 0x44, 0x24, 0x20]); // mov rax, [rsp+0x20]  (&EXCEPTION_RECORD)
    c.extend_from_slice(&[0x8B, 0x10]); // mov edx, [rax]  (ExceptionCode)
    c.extend_from_slice(&[0x48, 0xC7, 0xC1, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rcx, -1
    c.extend_from_slice(&[0xB8]);
    c.extend_from_slice(&term.to_le_bytes()); // mov eax, <NtTerminateProcess sel>
    c.extend_from_slice(&[0x49, 0x89, 0xCA, 0x0F, 0x05, 0x0F, 0x0B]); // mov r10,rcx; syscall; ud2
    page[..c.len()].copy_from_slice(&c);
    proc.map(crate::seh::PE_KIUSER_ADDR, f.start_address().as_u64(), false, true);
    Ok(())
}

/// The `KiUserApcDispatcher` page (r-x): on entry `rsp` -> a `CONTEXT` whose
/// home area holds the APC parameters ([`crate::apc::stage`] lays it out). Load
/// them, call the routine, then `NtContinue(&ctx, TestAlert=TRUE)` — which
/// drains any further queued APC before resuming the interrupted code.
fn map_apc_page(proc: &Process) -> Result<(), &'static str> {
    let f = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (KiUserApc)")?;
    let page = unsafe {
        let p = phys_to_virt(f.start_address()).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        core::slice::from_raw_parts_mut(p, 4096)
    };
    let cont = (crate::nt::NT_BASE
        | crate::nt::NT_NTDLL_FLAG as u64
        | crate::nt::NT_NTCONTINUE as u64) as u32;
    let mut c: Vec<u8> = Vec::new();
    c.extend_from_slice(&[0x48, 0x8B, 0x44, 0x24, 0x18]); // mov rax, [rsp+0x18]  NormalRoutine
    c.extend_from_slice(&[0x48, 0x8B, 0x0C, 0x24]); // mov rcx, [rsp]       NormalContext
    c.extend_from_slice(&[0x48, 0x8B, 0x54, 0x24, 0x08]); // mov rdx, [rsp+0x08]  SystemArgument1
    c.extend_from_slice(&[0x4C, 0x8B, 0x44, 0x24, 0x10]); // mov r8,  [rsp+0x10]  SystemArgument2
    c.extend_from_slice(&[0x48, 0x89, 0xE3]); // mov rbx, rsp  (save &CONTEXT; callee-saved)
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx  (&CONTEXT)
    c.extend_from_slice(&[0xBA, 0x01, 0x00, 0x00, 0x00]); // mov edx, 1  (TestAlert = TRUE)
    c.extend_from_slice(&[0xB8]);
    c.extend_from_slice(&cont.to_le_bytes()); // mov eax, <NtContinue sel>
    c.extend_from_slice(&[0x49, 0x89, 0xCA, 0x0F, 0x05, 0x0F, 0x0B]); // mov r10,rcx; syscall; ud2
    page[..c.len()].copy_from_slice(&c);
    proc.map(crate::apc::PE_KIUSERAPC_ADDR, f.start_address().as_u64(), false, true);
    Ok(())
}

/// The worker-thread entry stub (r-x). On entry `rsp` → `[StartRoutine][Argument]`
/// (16-aligned): pop them, call the routine, then `NtTerminateThread(-2, retval)`.
fn map_thread_start_page(proc: &Process) -> Result<(), &'static str> {
    let f = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (threadstart)")?;
    let page = unsafe {
        let p = phys_to_virt(f.start_address()).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        core::slice::from_raw_parts_mut(p, 4096)
    };
    let term = (crate::nt::NT_BASE
        | crate::nt::NT_NTDLL_FLAG as u64
        | crate::nt::NT_NTTERMINATETHREAD as u64) as u32;
    let mut c: Vec<u8> = Vec::new();
    c.extend_from_slice(&[0x58]); // pop rax   ; StartRoutine
    c.extend_from_slice(&[0x59]); // pop rcx   ; Argument -> Win64 arg0
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 0x20
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 0x20
    c.extend_from_slice(&[0x89, 0xC2]); // mov edx, eax   ; ExitStatus
    c.extend_from_slice(&[0x48, 0xC7, 0xC1, 0xFE, 0xFF, 0xFF, 0xFF]); // mov rcx, -2 (NtCurrentThread)
    c.extend_from_slice(&[0xB8]);
    c.extend_from_slice(&term.to_le_bytes()); // mov eax, <NtTerminateThread sel>
    c.extend_from_slice(&[0x49, 0x89, 0xCA, 0x0F, 0x05, 0x0F, 0x0B]); // mov r10,rcx; syscall; ud2
    page[..c.len()].copy_from_slice(&c);
    proc.map(PE_THREADSTART_ADDR, f.start_address().as_u64(), false, true);
    Ok(())
}

/// Spawn one ring-3 worker thread in the current PE process: it shares the
/// address space + `Task`, gets its own TEB (`PE_TEB2_ADDR`), 32 KiB stack and
/// kernel stack, and enters at [`PE_THREADSTART_ADDR`] with
/// `[start][arg]` on top of its user stack. Returns `(tid, exit_event)` — the
/// event is signalled (manual-reset) when the thread terminates.
///
/// One worker per process for now (the TEB / stack regions are fixed).
pub fn spawn_thread(
    start: u64,
    arg: u64,
) -> Result<(u64, alloc::sync::Arc<crate::wait::Event>), &'static str> {
    let task = crate::sched::current().task().ok_or("no current task")?;
    let proc = task.space();

    // user stack: fresh frames, contiguous VA at PE_THREAD_STACK_ADDR.
    let pages = (PE_THREAD_STACK_BYTES / 0x1000) as usize;
    for i in 0..pages {
        let fr = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (thread stack)")?;
        unsafe { core::ptr::write_bytes(phys_to_virt(fr.start_address()).as_mut_ptr::<u8>(), 0, 4096) };
        if i == pages - 1 {
            // top frame: lay [start][arg] at the very top (16-aligned).
            let top = phys_to_virt(fr.start_address()).as_u64() + 0x1000;
            unsafe {
                *((top - 16) as *mut u64) = start;
                *((top - 8) as *mut u64) = arg;
            }
        }
        proc.map(PE_THREAD_STACK_ADDR + (i as u64) * 0x1000, fr.start_address().as_u64(), true, false);
    }
    let user_rsp = PE_THREAD_STACK_ADDR + PE_THREAD_STACK_BYTES - 16;

    // worker TEB.
    let tf = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (thread TEB)")?;
    unsafe {
        let p = phys_to_virt(tf.start_address()).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        let put = |off: usize, v: u64| *((p.add(off)) as *mut u64) = v;
        put(0x08, PE_THREAD_STACK_ADDR + PE_THREAD_STACK_BYTES); // StackBase
        put(0x10, PE_THREAD_STACK_ADDR); // StackLimit
        put(0x30, PE_TEB2_ADDR); // NT_TIB.Self
        put(0x60, PE_PEB_ADDR); // ProcessEnvironmentBlock
    }
    proc.map(PE_TEB2_ADDR, tf.start_address().as_u64(), true, false);

    let ev = alloc::sync::Arc::new(crate::wait::Event::new());
    let tid = crate::sched::spawn_user_pe("pe-thread", task, PE_THREADSTART_ADDR, user_rsp, PE_TEB2_ADDR);
    crate::process::register_thread_exit(tid, ev.clone());
    Ok((tid, ev))
}

fn map_kernel32_page(proc: &Process) -> Result<(), &'static str> {
    map_synth_dll(proc, PE_KERNEL32_ADDR, "KERNEL32.DLL", &crate::nt::NT_EXPORTS, 0, &[])
}
fn map_ntdll_page(proc: &Process) -> Result<(), &'static str> {
    map_synth_dll(
        proc,
        PE_NTDLL_ADDR,
        "NTDLL.DLL",
        &crate::nt::NTDLL_EXPORTS,
        crate::nt::NT_NTDLL_FLAG,
        &[],
    )
}
fn map_msvcrt_page(proc: &Process) -> Result<(), &'static str> {
    map_synth_dll(
        proc,
        PE_MSVCRT_ADDR,
        "msvcrt.dll",
        &crate::nt::MSVCRT_EXPORTS,
        crate::nt::NT_MSVCRT_FLAG,
        &crate::nt::MSVCRT_DATA_EXPORTS,
    )
}

/// Build the r-x `_initterm` stub page (see [`PE_INITTERM_ADDR`]).
fn map_initterm_page(proc: &Process) -> Result<(), &'static str> {
    let f = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (initterm)")?;
    let page = unsafe {
        let p = phys_to_virt(f.start_address()).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        core::slice::from_raw_parts_mut(p, 4096)
    };
    // void _initterm(void (**pfbegin)(void) /*rcx*/, void (**pfend)(void) /*rdx*/)
    #[rustfmt::skip]
    let code: [u8; 44] = [
        0x53,                   // push rbx
        0x56,                   // push rsi
        0x57,                   // push rdi
        0x48, 0x89, 0xCB,       // mov rbx, rcx        ; pfbegin
        0x48, 0x89, 0xD7,       // mov rdi, rdx        ; pfend
        0x48, 0x83, 0xEC, 0x20, // sub rsp, 0x20       ; shadow space
        0x48, 0x39, 0xFB,       // .loop: cmp rbx, rdi
        0x73, 0x10,             // jae .done
        0x48, 0x8B, 0x03,       // mov rax, [rbx]
        0x48, 0x83, 0xC3, 0x08, // add rbx, 8
        0x48, 0x85, 0xC0,       // test rax, rax
        0x74, 0xEF,             // jz .loop
        0xFF, 0xD0,             // call rax
        0xEB, 0xEB,             // jmp .loop
        0x48, 0x83, 0xC4, 0x20, // .done: add rsp, 0x20
        0x5F,                   // pop rdi
        0x5E,                   // pop rsi
        0x5B,                   // pop rbx
        0x31, 0xC0,             // xor eax, eax
        0xC3,                   // ret
    ];
    page[..code.len()].copy_from_slice(&code);
    proc.map(PE_INITTERM_ADDR, f.start_address().as_u64(), false, true);
    Ok(())
}

/// The CRT scratch page (rw): `msvcrt`'s `__iob_func` `FILE[3]` (fd at
/// `_file`/+28), `errno`, a near-empty `struct lconv`, and the `__getmainargs`
/// `argv` array + strings.
fn map_crt_page(proc: &Process) -> Result<(), &'static str> {
    let f = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (crt)")?;
    unsafe {
        let p = phys_to_virt(f.start_address()).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        // FILE[3] with _file (offset 28) = 0/1/2.
        for i in 0..3usize {
            *((p.add(i * 48 + 28)) as *mut i32) = i as i32;
        }
        // lconv @ 0x108: every `char *` field -> "" (the NUL at 0x181);
        // `decimal_point` (first field) -> "." at 0x180.
        *(p.add(0x180)) = b'.';
        *(p.add(0x181)) = 0;
        *((p.add(0x108)) as *mut u64) = PE_CRT_ADDR + 0x180; // decimal_point
        for k in 1..8u64 {
            *((p.add(0x108 + (k * 8) as usize)) as *mut u64) = PE_CRT_ADDR + 0x181;
        }
    }
    proc.map(PE_CRT_ADDR, f.start_address().as_u64(), true, false);
    Ok(())
}

/// Build a synthetic system DLL image at `image_base` (one page, exec +
/// read-only): a minimal PE32+ header, one `mov eax, NT_BASE|flag|i ; mov r10,
/// rcx ; syscall ; ret` trampoline per export at [`SYNTH_STUBS_OFF`], and an
/// `IMAGE_EXPORT_DIRECTORY` at [`SYNTH_EXPDIR_OFF`] whose
/// `AddressOfFunctions[i]` points at trampoline `i` and `AddressOfNames[i]` is
/// `exports[i]`. `nt::ExportDir::parse` walks exactly this layout.
fn map_synth_dll(
    proc: &Process,
    image_base: u64,
    dll_name: &str,
    exports: &[&str],
    sel_flag: u16,
    data_exports: &[u16],
) -> Result<(), &'static str> {
    let frame = FRAME_ALLOC.lock().alloc().ok_or("PE: out of frames (synth dll)")?;
    let phys = frame.start_address();
    let page = unsafe {
        let p = phys_to_virt(phys).as_mut_ptr::<u8>();
        core::ptr::write_bytes(p, 0, 4096);
        core::slice::from_raw_parts_mut(p, 4096)
    };
    let p16 = |b: &mut [u8], o: usize, v: u16| b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    let p32 = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let p64 = |b: &mut [u8], o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_le_bytes());

    // DOS stub + PE signature.
    page[0..2].copy_from_slice(b"MZ");
    p32(page, 0x3C, 0x40);
    page[0x40..0x44].copy_from_slice(b"PE\0\0");

    // COFF header @ 0x44.
    let coff = 0x44usize;
    p16(page, coff, 0x8664); // Machine
    p16(page, coff + 2, 0); // NumberOfSections (headers-only module)
    p16(page, coff + 16, 0xF0); // SizeOfOptionalHeader
    p16(page, coff + 18, 0x2022); // EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE | DLL

    // PE32+ optional header @ 0x58.
    let opt = coff + 20;
    p16(page, opt, 0x20B); // Magic = PE32+
    p64(page, opt + 24, image_base); // ImageBase
    p32(page, opt + 32, 0x1000); // SectionAlignment
    p32(page, opt + 36, 0x200); // FileAlignment
    p32(page, opt + 56, 0x1000); // SizeOfImage (one page)
    p32(page, opt + 60, 0x200); // SizeOfHeaders
    p16(page, opt + 68, 3); // Subsystem (unused)
    p32(page, opt + 108, 16); // NumberOfRvaAndSizes

    let n = exports.len();

    // Trampolines — except for *data* exports, whose EAT slot stays writable
    // zero bytes (the CRT reads/writes `*_fmode` etc., all default 0).
    for idx in 0..n {
        if data_exports.contains(&(idx as u16)) {
            continue;
        }
        let mut stub = [0xB8u8, 0, 0, 0, 0, 0x49, 0x89, 0xCA, 0x0F, 0x05, 0xC3];
        let sel = (crate::nt::NT_BASE | sel_flag as u64 | idx as u64) as u32;
        stub[1..5].copy_from_slice(&sel.to_le_bytes());
        let at = SYNTH_STUBS_OFF as usize + idx * NT_STUB_STRIDE as usize;
        page[at..at + stub.len()].copy_from_slice(&stub);
    }

    // Export tables, laid out after the fixed-size directory.
    let expdir = SYNTH_EXPDIR_OFF as usize;
    let eat = expdir + 0x28; // AddressOfFunctions
    let enpt = eat + n * 4; // AddressOfNames
    let ords = enpt + n * 4; // AddressOfNameOrdinals
    let mut cur = ords + n * 2;

    let name_rva = cur;
    page[cur..cur + dll_name.len()].copy_from_slice(dll_name.as_bytes());
    cur += dll_name.len() + 1;

    for (i, fname) in exports.iter().enumerate() {
        p32(page, eat + i * 4, SYNTH_STUBS_OFF as u32 + (i * NT_STUB_STRIDE as usize) as u32);
        p32(page, enpt + i * 4, cur as u32);
        p16(page, ords + i * 2, i as u16);
        let fb = fname.as_bytes();
        page[cur..cur + fb.len()].copy_from_slice(fb);
        cur += fb.len() + 1;
    }
    if cur > 4096 {
        return Err("PE: synthetic DLL overflows its page");
    }

    // IMAGE_EXPORT_DIRECTORY @ SYNTH_EXPDIR_OFF.
    p32(page, expdir + 0x0C, name_rva as u32); // Name
    p32(page, expdir + 0x10, 1); // Base (ordinal base)
    p32(page, expdir + 0x14, n as u32); // NumberOfFunctions
    p32(page, expdir + 0x18, n as u32); // NumberOfNames
    p32(page, expdir + 0x1C, eat as u32); // AddressOfFunctions
    p32(page, expdir + 0x20, enpt as u32); // AddressOfNames
    p32(page, expdir + 0x24, ords as u32); // AddressOfNameOrdinals

    // Data directory 0 (export).
    p32(page, opt + 112, expdir as u32);
    p32(page, opt + 116, (cur - expdir) as u32);

    // rwx when the module has data exports the CRT writes through; else r-x.
    proc.map(image_base, phys.as_u64(), !data_exports.is_empty(), true);
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
