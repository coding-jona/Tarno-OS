// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — the NT-personality syscall surface.
//!
//! A PE process's imports resolve to trampolines in [`crate::pe`]'s shared stub
//! page; each does `mov eax, NT_BASE|idx; mov r10, rcx; syscall`. The Linux
//! dispatcher routes `rax` in the `NT_BASE` range here. `dispatch` reads the
//! **Win64** argument registers off the [`UserFrame`] (arg0 was moved `rcx`→
//! `r10` by the stub before `syscall` clobbered `rcx`; args 1-3 are `rdx`,
//! `r8`, `r9`; the 5th+ live on the user stack at `rsp+0x28`) and marshals the
//! call onto THOS's own objects.
//!
//! These are the `kernel32` boundary for now (`WriteFile` short-circuits
//! straight to a THOS write); a real `ntdll` with `Nt*` primitives layers on
//! later.

use crate::syscall::UserFrame;
use crate::{process, sched};

/// `rax` values `NT_BASE ..= NT_BASE|0xFFFF` are NT-personality calls.
pub const NT_BASE: u64 = 0x4E54_0000; // 'N' 'T'

// Stub indices — must match `pe::resolve_import` and the stub page layout.
pub const NT_EXITPROCESS: u16 = 0;
pub const NT_GETSTDHANDLE: u16 = 1;
pub const NT_WRITEFILE: u16 = 2;
pub const NT_GETLASTERROR: u16 = 3;
pub const NT_SETLASTERROR: u16 = 4;
pub const NT_CREATEFILEA: u16 = 5;
pub const NT_READFILE: u16 = 6;
pub const NT_CLOSEHANDLE: u16 = 7;
pub const NT_GETCOMMANDLINEA: u16 = 8;
pub const NT_GETMODULEHANDLEA: u16 = 9;
pub const NT_VIRTUALALLOC: u16 = 10;
pub const NT_VIRTUALFREE: u16 = 11;
pub const NT_VIRTUALPROTECT: u16 = 12;
pub const NT_GETPROCESSHEAP: u16 = 13;
pub const NT_HEAPALLOC: u16 = 14;
pub const NT_HEAPFREE: u16 = 15;
pub const NT_GETPROCADDRESS: u16 = 16;
pub const NT_LOADLIBRARYA: u16 = 17;
pub const NT_STUB_COUNT: u16 = 18;

/// The `kernel32` export table, in stub-index order. Drives both
/// [`crate::pe::resolve_import`] (import → stub index) and the synthetic
/// `kernel32.dll` module's `IMAGE_EXPORT_DIRECTORY` (name → stub RVA), so
/// `GetProcAddress` / `LoadLibraryA` resolve against the same list an import
/// does. Index **must** equal the matching `NT_*` constant.
pub const NT_EXPORTS: [&str; NT_STUB_COUNT as usize] = [
    "ExitProcess",
    "GetStdHandle",
    "WriteFile",
    "GetLastError",
    "SetLastError",
    "CreateFileA",
    "ReadFile",
    "CloseHandle",
    "GetCommandLineA",
    "GetModuleHandleA",
    "VirtualAlloc",
    "VirtualFree",
    "VirtualProtect",
    "GetProcessHeap",
    "HeapAlloc",
    "HeapFree",
    "GetProcAddress",
    "LoadLibraryA",
];

/// The sentinel `GetProcessHeap()` returns (and `PEB->ProcessHeap`). Handles are
/// opaque tokens to a Win32 program; ours is just a fixed non-zero value.
pub const PE_PROCESS_HEAP: u64 = 0x0000_7FF0_0000_0100;

const STD_INPUT_HANDLE: i32 = -10;
const STD_OUTPUT_HANDLE: i32 = -11;
const STD_ERROR_HANDLE: i32 = -12;
const INVALID_HANDLE_VALUE: i64 = -1;

// A few Win32 error codes.
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;

/// `TEB.LastErrorValue` lives at `gs:[0x68]`. The kernel runs with `gs` swapped
/// to the per-CPU block, so reach the TEB through the thread's saved `%gs` base.
fn teb() -> Option<*mut u8> {
    let base = sched::current().gsbase();
    (base != 0).then_some(base as *mut u8)
}
fn set_last_error(err: u32) {
    if let Some(t) = teb() {
        unsafe { *(t.add(0x68) as *mut u32) = err };
    }
}
fn get_last_error() -> u32 {
    teb().map_or(0, |t| unsafe { *(t.add(0x68) as *const u32) })
}

/// Handle a `syscall` from an NT stub. Returns the value to place in `rax`
/// (the Win64 return register). `ExitProcess` does not return.
pub fn dispatch(idx: u16, frame: &mut UserFrame) -> i64 {
    let a0 = frame.r10; // was rcx
    let a1 = frame.rdx;
    let a2 = frame.r8;
    let a3 = frame.r9;

    match idx {
        NT_EXITPROCESS => {
            // ExitProcess(UINT uExitCode)
            process::set_exit_status(a0 as i32);
            crate::syscall::note_user_exit();
            sched::exit();
        }

        NT_GETSTDHANDLE => match a0 as i32 {
            // A THOS "HANDLE" for the std streams is just the fd number.
            STD_INPUT_HANDLE => 0,
            STD_OUTPUT_HANDLE => 1,
            STD_ERROR_HANDLE => 2,
            _ => INVALID_HANDLE_VALUE,
        },

        NT_WRITEFILE => {
            // WriteFile(HANDLE hFile, LPCVOID buf, DWORD len, LPDWORD written, LPOVERLAPPED)
            let fd = a0;
            let buf = a1;
            let len = a2 as usize;
            let written_ptr = a3;

            let Some(f) = process::current_fd(fd as i32) else {
                return 0; // FALSE — bad handle
            };
            let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
            let n = f.write(bytes);
            if n < 0 {
                return 0; // FALSE
            }
            if written_ptr != 0 {
                unsafe { *(written_ptr as *mut u32) = n as u32 };
            }
            1 // TRUE
        }

        NT_READFILE => {
            // ReadFile(HANDLE, LPVOID buf, DWORD len, LPDWORD read, LPOVERLAPPED)
            let Some(f) = process::current_fd(a0 as i32) else {
                set_last_error(ERROR_INVALID_HANDLE);
                return 0;
            };
            let out = unsafe { core::slice::from_raw_parts_mut(a1 as *mut u8, a2 as usize) };
            let n = f.read(out);
            if n < 0 {
                return 0; // FALSE (EOF is n==0 -> still TRUE, *read = 0)
            }
            if a3 != 0 {
                unsafe { *(a3 as *mut u32) = n as u32 };
            }
            1
        }

        NT_CREATEFILEA => {
            // CreateFileA(name, access, share, sec, disposition, flags, template)
            // First cut: read-only opens of an existing file. The 5th arg sits
            // at [rsp+0x28] from the stub's frame (0x20 shadow + the `call`'s
            // 8-byte return address).
            let name = user_cstr(a0);
            let disposition = unsafe { *((frame.rsp + 0x28) as *const u32) };
            const OPEN_EXISTING: u32 = 3;
            if disposition != OPEN_EXISTING {
                set_last_error(ERROR_FILE_NOT_FOUND);
                return INVALID_HANDLE_VALUE;
            }
            let path = win_path_to_thos(&name);
            let fd = crate::syscall::open_resolved(&path);
            if fd < 0 {
                set_last_error(ERROR_FILE_NOT_FOUND);
                INVALID_HANDLE_VALUE
            } else {
                fd // the fd is the HANDLE
            }
        }

        NT_CLOSEHANDLE => {
            match sched::current().task() {
                Some(t) if t.fd_close(a0 as i32) => 1,
                _ => {
                    set_last_error(ERROR_INVALID_HANDLE);
                    0
                }
            }
        }

        NT_GETLASTERROR => get_last_error() as i64,
        NT_SETLASTERROR => {
            set_last_error(a0 as u32);
            0
        }

        // GetCommandLineA() -> LPSTR : the ANSI command line we placed in the
        // process's parameter page.
        NT_GETCOMMANDLINEA => crate::pe::PE_ANSI_CMDLINE_ADDR as i64,

        // GetModuleHandleA(lpModuleName) -> HMODULE. NULL -> the exe's base
        // (PEB->ImageBaseAddress); a name -> the matching PEB->Ldr entry's
        // DllBase (case-insensitive, `.dll` implied), else NULL + MOD_NOT_FOUND.
        NT_GETMODULEHANDLEA => {
            if a0 == 0 {
                teb().map_or(0, |t| unsafe {
                    let peb = *(t.add(0x60) as *const u64);
                    *((peb + 0x10) as *const u64) as i64
                })
            } else {
                match ldr_find(&normalize_mod(&user_cstr(a0))) {
                    b if b != 0 => b as i64,
                    _ => {
                        set_last_error(126); // ERROR_MOD_NOT_FOUND
                        0
                    }
                }
            }
        }

        // LoadLibraryA(lpLibFileName) -> HMODULE. No on-disk DLLs yet, so this
        // only hands back an already-present module (the synthetic kernel32).
        NT_LOADLIBRARYA => match ldr_find(&normalize_mod(&user_cstr(a0))) {
            b if b != 0 => b as i64,
            _ => {
                set_last_error(126); // ERROR_MOD_NOT_FOUND
                0
            }
        },

        // GetProcAddress(hModule, lpProcName) -> FARPROC. Parses the module's
        // IMAGE_EXPORT_DIRECTORY. `lpProcName` is a string, or an ordinal when
        // the upper bits are zero (`MAKEINTRESOURCE`-style).
        NT_GETPROCADDRESS => {
            let r = if a1 >> 16 == 0 {
                export_by_ordinal(a0, a1 as u16)
            } else {
                export_by_name(a0, &user_cstr(a1))
            };
            if r == 0 {
                set_last_error(127); // ERROR_PROC_NOT_FOUND
            }
            r
        }

        // VirtualAlloc(addr, size, type, protect): we always pick the address.
        // Backed by an anonymous zeroed mapping; RWX regardless of `protect`
        // until W^X lands.
        NT_VIRTUALALLOC => {
            let size = a1;
            match sched::current_proc().map(|p| p.mmap_anon(size)) {
                Some(base) if base != 0 => base as i64,
                _ => {
                    set_last_error(8); // ERROR_NOT_ENOUGH_MEMORY
                    0
                }
            }
        }
        // VirtualFree / VirtualProtect: no teardown / per-page protection yet.
        NT_VIRTUALFREE | NT_VIRTUALPROTECT => 1,

        NT_GETPROCESSHEAP => PE_PROCESS_HEAP as i64,

        // HeapAlloc(hHeap, flags, bytes): one anon mapping per call. Wasteful
        // for tiny allocations but correct; a real heap allocator comes later.
        NT_HEAPALLOC => {
            let bytes = a2.max(1);
            match sched::current_proc().map(|p| p.mmap_anon(bytes)) {
                Some(base) if base != 0 => base as i64, // mmap_anon already zeroes
                _ => 0,
            }
        }
        NT_HEAPFREE => 1,

        _ => {
            crate::kprintln!("THOS: nt unhandled call {}", idx);
            0
        }
    }
}

/// A crude Windows→THOS path map: strip a leading `X:\`, turn `\` into `/`,
/// force absolute. Good enough for `C:\...` / bare names until the
/// `\Device\` + drive-letter VFS view lands.
fn win_path_to_thos(win: &str) -> alloc::string::String {
    let b = win.as_bytes();
    let s = if b.len() >= 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        &win[2..] // drop the "X:" drive prefix, keep the separator
    } else {
        win
    };
    let mut out = alloc::string::String::from("/");
    for part in s.split(|c| c == '\\' || c == '/').filter(|p| !p.is_empty()) {
        if out.len() > 1 {
            out.push('/');
        }
        out.push_str(part);
    }
    out
}

/// Normalise a module name for an `Ldr` lookup: drop any directory, lowercase,
/// and append `.dll` when there is no extension (matching `GetModuleHandleA`).
fn normalize_mod(name: &str) -> alloc::string::String {
    let base = name.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(name);
    let mut s = base.to_ascii_lowercase();
    if !s.contains('.') {
        s.push_str(".dll");
    }
    s
}

/// `true` if the `n`-code-unit UTF-16LE string at `buf` equals the ASCII
/// `want` (which the caller has already lowercased), case-insensitively.
unsafe fn utf16_eq_ci(buf: u64, n: usize, want: &str) -> bool {
    let wb = want.as_bytes();
    if wb.len() != n {
        return false;
    }
    for (i, &w) in wb.iter().enumerate() {
        let c = *((buf + (i * 2) as u64) as *const u16);
        if c > 0x7F || (c as u8).to_ascii_lowercase() != w {
            return false;
        }
    }
    true
}

/// Walk `PEB->Ldr->InLoadOrderModuleList` for a module whose `BaseDllName`
/// matches `want` (already normalised/lowercased). Returns its `DllBase`, or 0.
fn ldr_find(want: &str) -> u64 {
    let Some(t) = teb() else { return 0 };
    unsafe {
        let peb = *(t.add(0x60) as *const u64);
        if peb == 0 {
            return 0;
        }
        let ldr = *((peb + 0x18) as *const u64);
        if ldr == 0 {
            return 0;
        }
        let head = ldr + 0x10; // InLoadOrderModuleList
        let mut cur = *(head as *const u64); // first Flink
        // InLoadOrderLinks sits at offset 0 of LDR_DATA_TABLE_ENTRY.
        for _ in 0..64 {
            if cur == head || cur == 0 {
                break;
            }
            let len = *((cur + 0x58) as *const u16) as usize; // BaseDllName.Length
            let bufp = *((cur + 0x60) as *const u64); // BaseDllName.Buffer
            if bufp != 0 && len >= 2 && utf16_eq_ci(bufp, len / 2, want) {
                return *((cur + 0x30) as *const u64); // DllBase
            }
            cur = *(cur as *const u64); // next Flink
        }
    }
    0
}

/// Parsed `IMAGE_EXPORT_DIRECTORY` — absolute pointers into the mapped module.
struct ExportDir {
    base: u64,
    eat: u64,   // AddressOfFunctions   (u32 RVAs)
    enpt: u64,  // AddressOfNames       (u32 RVAs)
    ords: u64,  // AddressOfNameOrdinals(u16 indices)
    n_names: usize,
    n_funcs: u64,
    ord_base: u64,
    dir_rva: u64,
    dir_size: u64,
}

impl ExportDir {
    /// Locate and sanity-check the export directory of the PE mapped at `base`.
    unsafe fn parse(base: u64) -> Option<ExportDir> {
        if base == 0 {
            return None;
        }
        let pe = base + *((base + 0x3C) as *const u32) as u64;
        if *(pe as *const u32) != 0x0000_4550 {
            return None; // "PE\0\0"
        }
        let opt = pe + 4 + 20;
        if *(opt as *const u16) != 0x20B || *((opt + 108) as *const u32) < 1 {
            return None; // not PE32+, or no export data dir slot
        }
        let dir_rva = *((opt + 112) as *const u32) as u64;
        let dir_size = *((opt + 116) as *const u32) as u64;
        if dir_rva == 0 {
            return None;
        }
        let ed = base + dir_rva;
        Some(ExportDir {
            base,
            eat: base + *((ed + 0x1C) as *const u32) as u64,
            enpt: base + *((ed + 0x20) as *const u32) as u64,
            ords: base + *((ed + 0x24) as *const u32) as u64,
            n_names: *((ed + 0x18) as *const u32) as usize,
            n_funcs: *((ed + 0x14) as *const u32) as u64,
            ord_base: *((ed + 0x10) as *const u32) as u64,
            dir_rva,
            dir_size,
        })
    }

    /// `base + AddressOfFunctions[idx]`, rejecting an empty slot or a forwarder
    /// RVA (one pointing back inside the export directory). 0 on any problem.
    unsafe fn func_at(&self, idx: u64) -> i64 {
        if idx >= self.n_funcs {
            return 0;
        }
        let frva = *((self.eat + idx * 4) as *const u32) as u64;
        if frva == 0 || (frva >= self.dir_rva && frva < self.dir_rva + self.dir_size) {
            return 0;
        }
        (self.base + frva) as i64
    }
}

fn export_by_name(base: u64, name: &str) -> i64 {
    unsafe {
        let Some(ed) = ExportDir::parse(base) else { return 0 };
        for i in 0..ed.n_names {
            let nrva = *((ed.enpt + (i * 4) as u64) as *const u32) as u64;
            if user_cstr(base + nrva) == name {
                let ord = *((ed.ords + (i * 2) as u64) as *const u16) as u64;
                return ed.func_at(ord);
            }
        }
    }
    0
}

fn export_by_ordinal(base: u64, ordinal: u16) -> i64 {
    unsafe {
        let Some(ed) = ExportDir::parse(base) else { return 0 };
        ed.func_at((ordinal as u64).wrapping_sub(ed.ord_base))
    }
}

fn user_cstr(ptr: u64) -> alloc::string::String {
    let mut s = alloc::string::String::new();
    let mut p = ptr;
    for _ in 0..4096 {
        let b = unsafe { *(p as *const u8) };
        if b == 0 {
            break;
        }
        s.push(b as char);
        p += 1;
    }
    s
}
