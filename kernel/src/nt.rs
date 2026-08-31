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

use alloc::sync::Arc;

use crate::syscall::UserFrame;
use crate::wait::Event;
use crate::{object, process, sched};

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

/// Selector bit OR-ed into a stub's index when it belongs to the **native NT**
/// (`ntdll`) layer rather than the **Win32** (`kernel32`) layer. `dispatch`
/// routes on it. `kernel32` calls are Win32-shaped (BOOL / `LastError` / a
/// HANDLE that is just an fd); `Nt*` calls are the real primitive shape
/// (NTSTATUS / `IO_STATUS_BLOCK` / in-out pointers) and share the same cores.
pub const NT_NTDLL_FLAG: u16 = 0x8000;

// ntdll `Nt*` / `Ldr*` indices — position **is** the index into `NTDLL_EXPORTS`.
pub const NT_NTCLOSE: u16 = 0;
pub const NT_NTWRITEFILE: u16 = 1;
pub const NT_NTREADFILE: u16 = 2;
pub const NT_NTALLOCATEVIRTUALMEMORY: u16 = 3;
pub const NT_NTFREEVIRTUALMEMORY: u16 = 4;
pub const NT_NTPROTECTVIRTUALMEMORY: u16 = 5;
pub const NT_NTTERMINATEPROCESS: u16 = 6;
pub const NT_LDRGETPROCEDUREADDRESS: u16 = 7;
pub const NT_LDRLOADDLL: u16 = 8;
pub const NT_NTQUERYINFORMATIONPROCESS: u16 = 9;
pub const NT_NTQUERYVIRTUALMEMORY: u16 = 10;
pub const NT_NTSETINFORMATIONTHREAD: u16 = 11;
pub const NT_NTSETINFORMATIONPROCESS: u16 = 12;
pub const NT_NTCREATEEVENT: u16 = 13;
pub const NT_NTWAITFORSINGLEOBJECT: u16 = 14;
pub const NT_NTSETEVENT: u16 = 15;
pub const NT_NTRESETEVENT: u16 = 16;
pub const NTDLL_STUB_COUNT: u16 = 17;

/// The `ntdll` service table — this **is** THOS's SSDT: the stub index is the
/// service number, and `dispatch_ntdll` is a table-driven switch on it. The
/// synthetic `ntdll.dll` module exports exactly these names at exactly these
/// ordinals, and [`crate::pe::resolve_import`] binds `ntdll.dll` imports here.
/// (See [`NT_EXPORTS`] for the Win32 `kernel32` shim layer that sits on top.)
pub const NTDLL_EXPORTS: [&str; NTDLL_STUB_COUNT as usize] = [
    "NtClose",
    "NtWriteFile",
    "NtReadFile",
    "NtAllocateVirtualMemory",
    "NtFreeVirtualMemory",
    "NtProtectVirtualMemory",
    "NtTerminateProcess",
    "LdrGetProcedureAddress",
    "LdrLoadDll",
    "NtQueryInformationProcess",
    "NtQueryVirtualMemory",
    "NtSetInformationThread",
    "NtSetInformationProcess",
    "NtCreateEvent",
    "NtWaitForSingleObject",
    "NtSetEvent",
    "NtResetEvent",
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

// NTSTATUS values the `Nt*` layer returns (low 32 bits; the high bit marks an
// error, which a caller tests with `NT_SUCCESS`).
const STATUS_SUCCESS: u32 = 0x0000_0000;
const STATUS_END_OF_FILE: u32 = 0xC000_0011;
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_INVALID_INFO_CLASS: u32 = 0xC000_0003;
const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xC000_0004;
const STATUS_TIMEOUT: u32 = 0x0000_0102;

/// NT `HANDLE`s to executive objects (events, …) carry this bit, so `NtClose`
/// and the wait/signal calls know to route to [`object`] rather than the fd
/// table. A real per-process unified HANDLE table replaces this later.
const NT_OBJ_TAG: u64 = 0x4000_0000;
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const STATUS_PROCEDURE_NOT_FOUND: u32 = 0xC000_007A;
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;

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

/// Handle a `syscall` from an NT stub. `sel` is the stub's index; the
/// [`NT_NTDLL_FLAG`] bit selects the native `Nt*` layer over Win32. Returns the
/// value for `rax` (Win64 return register). The terminate calls do not return.
pub fn dispatch(sel: u16, frame: &mut UserFrame) -> i64 {
    if sel & NT_NTDLL_FLAG != 0 {
        dispatch_ntdll(sel & !NT_NTDLL_FLAG, frame)
    } else {
        dispatch_kernel32(sel, frame)
    }
}

// --- shared cores: one implementation, reached from either personality ---

/// Set the exit status and leave ring 3 for good.
fn proc_terminate(code: i32) -> ! {
    process::set_exit_status(code);
    crate::syscall::note_user_exit();
    sched::exit();
}

/// `write(2)` against the current process's fd table. Bytes written, or -1.
fn file_write_core(fd: i32, buf: u64, len: usize) -> i64 {
    let Some(f) = process::current_fd(fd) else { return -1 };
    f.write(unsafe { core::slice::from_raw_parts(buf as *const u8, len) })
}
/// `read(2)` against the current process's fd table. Bytes read (0 = EOF), or -1.
fn file_read_core(fd: i32, buf: u64, len: usize) -> i64 {
    let Some(f) = process::current_fd(fd) else { return -1 };
    f.read(unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) })
}
fn handle_close_core(h: i32) -> bool {
    matches!(sched::current().task(), Some(t) if t.fd_close(h))
}
/// One anonymous zeroed mapping of `size` bytes (page-rounded by `mmap_anon`).
/// 0 on failure.
fn mem_alloc_core(size: u64) -> u64 {
    sched::current_proc()
        .map(|p| p.mmap_anon(size.max(1)))
        .unwrap_or(0)
}

/// The native NT (`ntdll`) syscall layer: NTSTATUS returns, `IO_STATUS_BLOCK`
/// out-params, in-out pointers. `kernel32` is a thin Win32-shaped shim over
/// these same cores.
fn dispatch_ntdll(idx: u16, frame: &mut UserFrame) -> i64 {
    let a0 = frame.r10; // was rcx
    let a1 = frame.rdx;
    let a2 = frame.r8;
    let a3 = frame.r9;
    // 5th Win64 arg at [rsp+0x28] (0x20 shadow + the call's return address);
    // `stack(i)` walks further stack args.
    let stack = |i: u64| unsafe { *((frame.rsp + 0x28 + i * 8) as *const u64) };

    match idx {
        // NtTerminateProcess(ProcessHandle, ExitStatus)
        NT_NTTERMINATEPROCESS => proc_terminate(a1 as i32),

        // NtClose(Handle) — an executive-object HANDLE or an fd.
        NT_NTCLOSE => {
            let ok = if a0 & NT_OBJ_TAG != 0 {
                object::close(object::Handle((a0 & !NT_OBJ_TAG) as u32))
            } else {
                handle_close_core(a0 as i32)
            };
            status(ok, STATUS_INVALID_HANDLE)
        }

        // NtWriteFile(FileHandle, Event, ApcRoutine, ApcContext, IoStatusBlock,
        //             Buffer, Length, ByteOffset, Key)
        NT_NTWRITEFILE => {
            let (iosb, buf, len) = (stack(0), stack(1), stack(2) as usize);
            let n = file_write_core(a0 as i32, buf, len);
            if n < 0 {
                return STATUS_INVALID_HANDLE as i64;
            }
            unsafe { write_iosb(iosb, STATUS_SUCCESS, n as u64) };
            STATUS_SUCCESS as i64
        }

        // NtReadFile(same shape as NtWriteFile)
        NT_NTREADFILE => {
            let (iosb, buf, len) = (stack(0), stack(1), stack(2) as usize);
            let n = file_read_core(a0 as i32, buf, len);
            if n < 0 {
                return STATUS_INVALID_HANDLE as i64;
            }
            let st = if n == 0 { STATUS_END_OF_FILE } else { STATUS_SUCCESS };
            unsafe { write_iosb(iosb, st, n as u64) };
            st as i64
        }

        // NtAllocateVirtualMemory(ProcessHandle, *BaseAddress, ZeroBits,
        //                         *RegionSize, AllocationType, Protect)
        NT_NTALLOCATEVIRTUALMEMORY => {
            let (base_pp, size_pp) = (a1, a3);
            if base_pp == 0 || size_pp == 0 {
                return STATUS_INVALID_PARAMETER as i64;
            }
            let size = unsafe { *(size_pp as *const u64) };
            let base = mem_alloc_core(size);
            if base == 0 {
                return STATUS_NO_MEMORY as i64;
            }
            unsafe {
                *(base_pp as *mut u64) = base;
                *(size_pp as *mut u64) = (size + 0xFFF) & !0xFFF;
            }
            STATUS_SUCCESS as i64
        }
        // No teardown / per-page protection yet.
        NT_NTFREEVIRTUALMEMORY | NT_NTPROTECTVIRTUALMEMORY => STATUS_SUCCESS as i64,

        // NtQueryInformationProcess(ProcessHandle, InfoClass, Buffer, Length,
        //                           *ReturnLength). Only ProcessBasicInformation
        // (class 0) — the call a real ntdll uses first, to find the PEB.
        NT_NTQUERYINFORMATIONPROCESS => {
            let (class, buf, len) = (a1 as u32, a2, a3 as usize);
            let ret_len = stack(0);
            if class != 0 {
                return STATUS_INVALID_INFO_CLASS as i64;
            }
            if len < 0x30 {
                return STATUS_INFO_LENGTH_MISMATCH as i64;
            }
            let peb = teb().map_or(0, |t| unsafe { *(t.add(0x60) as *const u64) });
            unsafe {
                let b = buf as *mut u64;
                *b.add(0) = 0; // ExitStatus
                *b.add(1) = peb; // PebBaseAddress
                *b.add(2) = 1; // AffinityMask
                *b.add(3) = 8; // BasePriority
                *b.add(4) = process::current_pid(); // UniqueProcessId
                *b.add(5) = 0; // InheritedFromUniqueProcessId
            }
            if ret_len != 0 {
                unsafe { *(ret_len as *mut u32) = 0x30 }; // ReturnLength is ULONG
            }
            STATUS_SUCCESS as i64
        }

        // NtQueryVirtualMemory(ProcessHandle, BaseAddress, InfoClass, Buffer,
        //                      Length, *ReturnLength). Only MemoryBasicInformation
        // (class 0); reports one committed RWX private region per page (W^X and
        // real region tracking arrive later).
        NT_NTQUERYVIRTUALMEMORY => {
            let (addr, class, buf, len) = (a1, a2 as u32, a3, stack(0) as usize);
            let ret_len = stack(1);
            if class != 0 {
                return STATUS_INVALID_INFO_CLASS as i64;
            }
            if len < 0x30 {
                return STATUS_INFO_LENGTH_MISMATCH as i64;
            }
            let page = addr & !0xFFF;
            unsafe {
                let b = buf as *mut u64;
                *b.add(0) = page; // BaseAddress
                *b.add(1) = page; // AllocationBase
                *(b.add(2) as *mut u32) = 0x40; // AllocationProtect = PAGE_EXECUTE_READWRITE
                *b.add(3) = 0x1000; // RegionSize
                *(b.add(4) as *mut u32) = 0x1000; // State = MEM_COMMIT
                *(b.add(4) as *mut u32).add(1) = 0x40; // Protect
                *(b.add(5) as *mut u32) = 0x2_0000; // Type = MEM_PRIVATE
            }
            if ret_len != 0 {
                unsafe { *(ret_len as *mut u64) = 0x30 }; // ReturnLength is SIZE_T
            }
            STATUS_SUCCESS as i64
        }

        // NtSetInformationThread / NtSetInformationProcess — early ntdll calls
        // these with classes THOS can safely ignore (debugger flags, priority,
        // …). Accept everything until a class actually needs backing.
        NT_NTSETINFORMATIONTHREAD | NT_NTSETINFORMATIONPROCESS => STATUS_SUCCESS as i64,

        // NtCreateEvent(*EventHandle, DesiredAccess, *ObjectAttributes,
        //               EventType, InitialState). Unnamed only; the executive
        // `Event` is manual-reset, so `EventType` (0 notification /
        // 1 synchronization) is not honoured yet.
        NT_NTCREATEEVENT => {
            let ev = Event::new();
            if stack(0) & 0xFF != 0 {
                ev.signal(); // InitialState = TRUE
            }
            let h = object::insert(Arc::new(ev));
            unsafe { *(a0 as *mut u64) = NT_OBJ_TAG | h.0 as u64 };
            STATUS_SUCCESS as i64
        }

        // NtWaitForSingleObject(Handle, Alertable, *Timeout). NULL timeout =
        // block until signalled; `*Timeout == 0` = poll; any other value is
        // treated as an immediate poll (a real timed wait comes with the
        // executive timer path). Events only for now.
        NT_NTWAITFORSINGLEOBJECT => {
            if a0 & NT_OBJ_TAG == 0 {
                return STATUS_INVALID_HANDLE as i64;
            }
            let Some(ev) = object::get::<Event>(object::Handle((a0 & !NT_OBJ_TAG) as u32)) else {
                return STATUS_INVALID_HANDLE as i64;
            };
            let poll_only = a2 != 0 && unsafe { *(a2 as *const i64) } != 0;
            if a2 == 0 {
                ev.wait(); // block until signalled
                STATUS_SUCCESS as i64
            } else if ev.is_signaled() {
                STATUS_SUCCESS as i64
            } else {
                let _ = poll_only;
                STATUS_TIMEOUT as i64
            }
        }

        // NtSetEvent / NtResetEvent(Handle, *PreviousState)
        NT_NTSETEVENT | NT_NTRESETEVENT => {
            if a0 & NT_OBJ_TAG == 0 {
                return STATUS_INVALID_HANDLE as i64;
            }
            let Some(ev) = object::get::<Event>(object::Handle((a0 & !NT_OBJ_TAG) as u32)) else {
                return STATUS_INVALID_HANDLE as i64;
            };
            let prev = ev.is_signaled() as u32;
            if idx == NT_NTSETEVENT {
                ev.signal();
            } else {
                ev.reset();
            }
            if a1 != 0 {
                unsafe { *(a1 as *mut u32) = prev };
            }
            STATUS_SUCCESS as i64
        }

        // LdrGetProcedureAddress(DllHandle, *AnsiName(STRING), Ordinal, *Address)
        NT_LDRGETPROCEDUREADDRESS => {
            let addr = if a1 != 0 {
                let namebuf = unsafe { *((a1 + 8) as *const u64) }; // STRING.Buffer
                export_by_name(a0, &user_cstr(namebuf), 0)
            } else {
                export_by_ordinal(a0, a2 as u16, 0)
            };
            if addr == 0 {
                return STATUS_PROCEDURE_NOT_FOUND as i64;
            }
            if a3 != 0 {
                unsafe { *(a3 as *mut u64) = addr as u64 };
            }
            STATUS_SUCCESS as i64
        }

        // LdrLoadDll(PathToFile, Flags, *ModuleFileName(UNICODE_STRING), *Handle)
        NT_LDRLOADDLL => {
            let name = unsafe { unicode_string_ascii(a2) };
            let base = ldr_find(&normalize_mod(&name));
            if base == 0 {
                return STATUS_DLL_NOT_FOUND as i64;
            }
            if a3 != 0 {
                unsafe { *(a3 as *mut u64) = base };
            }
            STATUS_SUCCESS as i64
        }

        _ => {
            crate::kprintln!("THOS: ntdll unhandled call {}", idx);
            STATUS_INVALID_PARAMETER as i64
        }
    }
}

fn status(ok: bool, err: u32) -> i64 {
    if ok { STATUS_SUCCESS as i64 } else { err as i64 }
}

/// Fill an `IO_STATUS_BLOCK` (`{ NTSTATUS Status; ULONG_PTR Information; }`).
unsafe fn write_iosb(iosb: u64, status: u32, information: u64) {
    if iosb != 0 {
        *(iosb as *mut u32) = status;
        *((iosb + 8) as *mut u64) = information;
    }
}

/// Decode a `UNICODE_STRING` (`{ u16 Length; u16 Max; u64 Buffer; }`, `Length`
/// in bytes) to an ASCII `String`, non-ASCII code units becoming `?`.
unsafe fn unicode_string_ascii(us: u64) -> alloc::string::String {
    if us == 0 {
        return alloc::string::String::new();
    }
    let n = (*(us as *const u16) as usize / 2).min(260);
    let buf = *((us + 8) as *const u64);
    let mut s = alloc::string::String::with_capacity(n);
    for i in 0..n {
        let c = *((buf + (i * 2) as u64) as *const u16);
        s.push(if c < 0x80 { c as u8 as char } else { '?' });
    }
    s
}

/// The Win32 (`kernel32`) layer: BOOL / `LastError` / fd-as-HANDLE, shimmed
/// onto the shared cores above.
fn dispatch_kernel32(idx: u16, frame: &mut UserFrame) -> i64 {
    let a0 = frame.r10; // was rcx
    let a1 = frame.rdx;
    let a2 = frame.r8;
    let a3 = frame.r9;

    match idx {
        // ExitProcess(UINT uExitCode)
        NT_EXITPROCESS => proc_terminate(a0 as i32),

        NT_GETSTDHANDLE => match a0 as i32 {
            // A THOS "HANDLE" for the std streams is just the fd number.
            STD_INPUT_HANDLE => 0,
            STD_OUTPUT_HANDLE => 1,
            STD_ERROR_HANDLE => 2,
            _ => INVALID_HANDLE_VALUE,
        },

        // WriteFile(HANDLE, buf, len, *written, overlapped) — shim over NtWriteFile's core.
        NT_WRITEFILE => {
            let n = file_write_core(a0 as i32, a1, a2 as usize);
            if n < 0 {
                return 0; // FALSE
            }
            if a3 != 0 {
                unsafe { *(a3 as *mut u32) = n as u32 };
            }
            1 // TRUE
        }

        // ReadFile(HANDLE, buf, len, *read, overlapped). EOF is n==0 -> still TRUE.
        NT_READFILE => {
            let n = file_read_core(a0 as i32, a1, a2 as usize);
            if n < 0 {
                set_last_error(ERROR_INVALID_HANDLE);
                return 0;
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
            if handle_close_core(a0 as i32) {
                1
            } else {
                set_last_error(ERROR_INVALID_HANDLE);
                0
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
                export_by_ordinal(a0, a1 as u16, 0)
            } else {
                export_by_name(a0, &user_cstr(a1), 0)
            };
            if r == 0 {
                set_last_error(127); // ERROR_PROC_NOT_FOUND
            }
            r
        }

        // VirtualAlloc(addr, size, type, protect): we always pick the address.
        // Anonymous zeroed mapping; RWX regardless of `protect` until W^X lands.
        NT_VIRTUALALLOC => match mem_alloc_core(a1) {
            0 => {
                set_last_error(8); // ERROR_NOT_ENOUGH_MEMORY
                0
            }
            base => base as i64,
        },
        // VirtualFree / VirtualProtect: no teardown / per-page protection yet.
        NT_VIRTUALFREE | NT_VIRTUALPROTECT => 1,

        NT_GETPROCESSHEAP => PE_PROCESS_HEAP as i64,

        // HeapAlloc(hHeap, flags, bytes): one anon mapping per call. Wasteful
        // for tiny allocations but correct; a real heap allocator comes later.
        NT_HEAPALLOC => mem_alloc_core(a2.max(1)) as i64,
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

    /// `base + AddressOfFunctions[idx]`. A forwarder RVA (one pointing back
    /// inside the export directory) is followed: read the `"Dll.Func"` string,
    /// find the target module in the `Ldr` list, resolve there. 0 on any problem.
    unsafe fn resolve_slot(&self, idx: u64, depth: u32) -> i64 {
        if idx >= self.n_funcs {
            return 0;
        }
        let frva = *((self.eat + idx * 4) as *const u32) as u64;
        if frva == 0 {
            return 0;
        }
        if frva < self.dir_rva || frva >= self.dir_rva + self.dir_size {
            return (self.base + frva) as i64; // ordinary address
        }
        // Forwarder.
        if depth > 8 {
            return 0;
        }
        let s = user_cstr(self.base + frva);
        let Some((dll, func)) = s.rsplit_once('.') else { return 0 };
        let tbase = ldr_find(&normalize_mod(dll));
        if tbase == 0 {
            return 0;
        }
        match func.strip_prefix('#') {
            Some(n) => match n.parse::<u16>() {
                Ok(o) => export_by_ordinal(tbase, o, depth + 1),
                _ => 0,
            },
            None => export_by_name(tbase, func, depth + 1),
        }
    }
}

fn export_by_name(base: u64, name: &str, depth: u32) -> i64 {
    unsafe {
        let Some(ed) = ExportDir::parse(base) else { return 0 };
        for i in 0..ed.n_names {
            let nrva = *((ed.enpt + (i * 4) as u64) as *const u32) as u64;
            if user_cstr(base + nrva) == name {
                let ord = *((ed.ords + (i * 2) as u64) as *const u16) as u64;
                return ed.resolve_slot(ord, depth);
            }
        }
    }
    0
}

fn export_by_ordinal(base: u64, ordinal: u16, depth: u32) -> i64 {
    unsafe {
        let Some(ed) = ExportDir::parse(base) else { return 0 };
        ed.resolve_slot((ordinal as u64).wrapping_sub(ed.ord_base), depth)
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
