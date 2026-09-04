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
use crate::wait::{Event, EventMode};
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
pub const NT_NTCONTINUE: u16 = 17;
pub const NT_RTLADDVECTOREDEXCEPTIONHANDLER: u16 = 18;
pub const NT_RTLREMOVEVECTOREDEXCEPTIONHANDLER: u16 = 19;
pub const NT_NTQUEUEAPCTHREAD: u16 = 20;
pub const NT_NTTESTALERT: u16 = 21;
pub const NT_NTCREATEKEY: u16 = 22;
pub const NT_NTOPENKEY: u16 = 23;
pub const NT_NTSETVALUEKEY: u16 = 24;
pub const NT_NTQUERYVALUEKEY: u16 = 25;
pub const NT_NTDELETEKEY: u16 = 26;
pub const NTDLL_STUB_COUNT: u16 = 27;

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
    "NtContinue",
    "RtlAddVectoredExceptionHandler",
    "RtlRemoveVectoredExceptionHandler",
    "NtQueueApcThread",
    "NtTestAlert",
    "NtCreateKey",
    "NtOpenKey",
    "NtSetValueKey",
    "NtQueryValueKey",
    "NtDeleteKey",
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
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const STATUS_PROCEDURE_NOT_FOUND: u32 = 0xC000_007A;
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;

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

        // NtClose(Handle) — one per-process table; files and objects alike.
        NT_NTCLOSE => status(handle_close_core(a0 as i32), STATUS_INVALID_HANDLE),

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
        // NtCreateEvent(*Handle, DesiredAccess, *ObjectAttributes, EventType,
        //               InitialState). EventType 0 = NotificationEvent
        // (manual-reset), 1 = SynchronizationEvent (auto-reset). Unnamed only.
        NT_NTCREATEEVENT => {
            let mode = if a3 == 1 { EventMode::Auto } else { EventMode::Manual };
            let ev = Arc::new(Event::with_mode(mode));
            if stack(0) & 0xFF != 0 {
                ev.signal(); // InitialState = TRUE
            }
            let h = process::current_alloc_event(ev);
            if h < 0 {
                return STATUS_NO_MEMORY as i64;
            }
            unsafe { *(a0 as *mut u64) = h as u64 };
            STATUS_SUCCESS as i64
        }

        // NtWaitForSingleObject(Handle, Alertable, *Timeout). NULL = block
        // forever; `*Timeout == 0` = poll; a negative `*Timeout` is a relative
        // wait in 100 ns units — spun against the 100 Hz APIC tick until the
        // event fires or the deadline passes (a real timed block on the
        // executive timer wheel comes later). Positive (absolute) = poll.
        NT_NTWAITFORSINGLEOBJECT => {
            let Some(ev) = process::current_event(a0 as i32) else {
                return STATUS_INVALID_HANDLE as i64;
            };
            if a2 == 0 {
                ev.wait();
                return STATUS_SUCCESS as i64;
            }
            let timeout = unsafe { *(a2 as *const i64) };
            if timeout >= 0 {
                // 0 = poll; positive (absolute deadline) is not tracked, so
                // also just check the current state.
                return if ev.try_take() {
                    STATUS_SUCCESS as i64
                } else {
                    STATUS_TIMEOUT as i64
                };
            }
            // Relative timeout. A PE thread's syscall runs with IF=0 (they are
            // cooperatively scheduled), so we can't rely on the timer tick or
            // block — spin a bounded number of cooperative yields scaled to the
            // requested interval and return `STATUS_TIMEOUT`. A true timed
            // block lands with the executive timer wheel.
            let spins = (((-timeout) as u64) / 50).clamp(20_000, 4_000_000);
            for _ in 0..spins {
                if ev.try_take() {
                    return STATUS_SUCCESS as i64;
                }
                sched::yield_now();
            }
            STATUS_TIMEOUT as i64
        }

        // NtContinue(*Context, TestAlert) — resume ring 3 from the CONTEXT the
        // exception / APC dispatcher (maybe) fixed up. When `TestAlert` is set
        // (the `KiUserApcDispatcher` tail passes it), drain the next queued user
        // APC before resuming, so a run of APCs unwinds one dispatcher call at a
        // time. Does not return.
        NT_NTCONTINUE => {
            let c = a0;
            let (cs, ss) = process::user_selectors();
            let rd = |off: u64| unsafe { *((c + off) as *const u64) };
            let mut f = crate::seh::ExcFrame {
                rax: rd(0x78),
                rcx: rd(0x80),
                rdx: rd(0x88),
                rbx: rd(0x90),
                rsp: rd(0x98),
                rbp: rd(0xA0),
                rsi: rd(0xA8),
                rdi: rd(0xB0),
                r8: rd(0xB8),
                r9: rd(0xC0),
                r10: rd(0xC8),
                r11: rd(0xD0),
                r12: rd(0xD8),
                r13: rd(0xE0),
                r14: rd(0xE8),
                r15: rd(0xF0),
                rip: rd(0xF8),
                // keep the saved IF (0 for a cooperative PE thread); bit 1 is
                // the reserved always-set flag.
                rflags: unsafe { *((c + 0x44) as *const u32) } as u64 | 0x2,
                cs,
                ss,
            };
            if a1 != 0 {
                if let Some((rsp, rip)) = crate::apc::take_and_stage(&exc_frame_regs(&f)) {
                    f.rsp = rsp;
                    f.rip = rip;
                }
            }
            unsafe { crate::seh::thos_exc_resume(&f) }
        }

        // NtQueueApcThread(ThreadHandle, ApcRoutine, ApcArgument1, ApcArgument2,
        //                  ApcArgument3). Only the current thread is a valid
        // target while a PE process is single-threaded: NtCurrentThread (-2) or
        // NtCurrentProcess (-1).
        NT_NTQUEUEAPCTHREAD => {
            if a0 as i64 != -2 && a0 as i64 != -1 {
                return STATUS_INVALID_HANDLE as i64;
            }
            if a1 == 0 {
                return STATUS_INVALID_PARAMETER as i64;
            }
            let e = process::ApcEntry { routine: a1, arg1: a2, arg2: a3, arg3: stack(0) };
            status(process::current_queue_apc(e), STATUS_INVALID_HANDLE)
        }

        // NtTestAlert() — if a user APC is queued, deliver it now by redirecting
        // this thread's return through `KiUserApcDispatcher`; the staged CONTEXT
        // carries STATUS_SUCCESS in `Rax` so the eventual resume returns it.
        NT_NTTESTALERT => {
            let (cs, ss) = process::user_selectors();
            let r = crate::apc::Regs {
                rax: STATUS_SUCCESS as u64,
                rcx: 0,
                rdx: frame.rdx,
                rbx: frame.rbx,
                rsp: frame.rsp,
                rbp: frame.rbp,
                rsi: frame.rsi,
                rdi: frame.rdi,
                r8: frame.r8,
                r9: frame.r9,
                r10: frame.r10,
                r11: frame.r11,
                r12: frame.r12,
                r13: frame.r13,
                r14: frame.r14,
                r15: frame.r15,
                rip: frame.rip,
                rflags: frame.rflags,
                cs,
                ss,
            };
            if let Some((rsp, rip)) = crate::apc::take_and_stage(&r) {
                frame.rsp = rsp;
                frame.rip = rip;
            }
            STATUS_SUCCESS as i64
        }

        // RtlAddVectoredExceptionHandler(First, Handler) — one slot for now
        // (a real ntdll keeps the list in userspace). Returns a non-NULL cookie.
        NT_RTLADDVECTOREDEXCEPTIONHANDLER => {
            unsafe { *(crate::seh::PE_EXC_ADDR as *mut u64) = a1 };
            crate::seh::PE_EXC_ADDR as i64
        }
        NT_RTLREMOVEVECTOREDEXCEPTIONHANDLER => {
            unsafe { *(crate::seh::PE_EXC_ADDR as *mut u64) = 0 };
            1
        }

        // NtSetEvent / NtResetEvent(Handle, *PreviousState)
        NT_NTSETEVENT | NT_NTRESETEVENT => {
            let Some(ev) = process::current_event(a0 as i32) else {
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

        // NtCreateKey(*KeyHandle, DesiredAccess, *ObjectAttributes, TitleIndex,
        //             *Class, CreateOptions, *Disposition). Creates missing
        // ancestors; class / security / options ignored.
        NT_NTCREATEKEY => {
            let Some(path) = (unsafe { oa_path(a2) }) else {
                return STATUS_INVALID_PARAMETER as i64;
            };
            let existed = crate::registry::open(&path);
            if !crate::registry::create(&path) {
                return STATUS_INVALID_PARAMETER as i64;
            }
            let h = process::current_alloc_regkey(crate::registry::canon(&path));
            if h < 0 {
                return STATUS_NO_MEMORY as i64;
            }
            unsafe { *(a0 as *mut u64) = h as u64 };
            let disp = stack(2);
            if disp != 0 {
                unsafe { *(disp as *mut u32) = if existed { 2 } else { 1 } }; // OPENED_EXISTING / CREATED_NEW
            }
            STATUS_SUCCESS as i64
        }

        // NtOpenKey(*KeyHandle, DesiredAccess, *ObjectAttributes)
        NT_NTOPENKEY => {
            let Some(path) = (unsafe { oa_path(a2) }) else {
                return STATUS_INVALID_PARAMETER as i64;
            };
            if !crate::registry::open(&path) {
                return STATUS_OBJECT_NAME_NOT_FOUND as i64;
            }
            let h = process::current_alloc_regkey(crate::registry::canon(&path));
            if h < 0 {
                return STATUS_NO_MEMORY as i64;
            }
            unsafe { *(a0 as *mut u64) = h as u64 };
            STATUS_SUCCESS as i64
        }

        // NtSetValueKey(KeyHandle, *ValueName(UNICODE_STRING), TitleIndex, Type,
        //               *Data, DataSize)
        NT_NTSETVALUEKEY => {
            let Some(path) = process::current_regkey(a0 as i32) else {
                return STATUS_INVALID_HANDLE as i64;
            };
            let name = unsafe { unicode_string_ascii(a1) };
            let (data_ptr, size) = (stack(0), stack(1) as usize);
            let data = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, size) };
            status(
                crate::registry::set_value(&path, &name, a3 as u32, data),
                STATUS_OBJECT_NAME_NOT_FOUND,
            )
        }

        // NtQueryValueKey(KeyHandle, *ValueName, InfoClass, *Info, Length,
        //                 *ResultLength). Only KeyValuePartialInformation
        // (class 2): { u32 TitleIndex; u32 Type; u32 DataLength; u8 Data[]; }.
        NT_NTQUERYVALUEKEY => {
            let Some(path) = process::current_regkey(a0 as i32) else {
                return STATUS_INVALID_HANDLE as i64;
            };
            let name = unsafe { unicode_string_ascii(a1) };
            if a2 != 2 {
                return STATUS_INVALID_INFO_CLASS as i64;
            }
            let Some((ty, data)) = crate::registry::query_value(&path, &name) else {
                return STATUS_OBJECT_NAME_NOT_FOUND as i64;
            };
            let need = 12 + data.len();
            let ret_len = stack(1);
            if ret_len != 0 {
                unsafe { *(ret_len as *mut u32) = need as u32 };
            }
            if (stack(0) as usize) < need {
                return STATUS_INFO_LENGTH_MISMATCH as i64;
            }
            unsafe {
                let b = a3 as *mut u8;
                *(b as *mut u32) = 0; // TitleIndex
                *(b.add(4) as *mut u32) = ty; // Type
                *(b.add(8) as *mut u32) = data.len() as u32; // DataLength
                core::ptr::copy_nonoverlapping(data.as_ptr(), b.add(12), data.len());
            }
            STATUS_SUCCESS as i64
        }

        // NtDeleteKey(KeyHandle) — "mini": drop the key from its parent now
        // (a real one defers until the last handle closes).
        NT_NTDELETEKEY => {
            let Some(path) = process::current_regkey(a0 as i32) else {
                return STATUS_INVALID_HANDLE as i64;
            };
            status(crate::registry::delete_key(&path), STATUS_OBJECT_NAME_NOT_FOUND)
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

/// Snapshot a rebuilt `seh::ExcFrame` as [`crate::apc::Regs`] so a pending APC
/// can be staged on top of the state `NtContinue` is about to resume.
fn exc_frame_regs(f: &crate::seh::ExcFrame) -> crate::apc::Regs {
    crate::apc::Regs {
        rax: f.rax,
        rcx: f.rcx,
        rdx: f.rdx,
        rbx: f.rbx,
        rsp: f.rsp,
        rbp: f.rbp,
        rsi: f.rsi,
        rdi: f.rdi,
        r8: f.r8,
        r9: f.r9,
        r10: f.r10,
        r11: f.r11,
        r12: f.r12,
        r13: f.r13,
        r14: f.r14,
        r15: f.r15,
        rip: f.rip,
        rflags: f.rflags,
        cs: f.cs,
        ss: f.ss,
    }
}

/// Fill an `IO_STATUS_BLOCK` (`{ NTSTATUS Status; ULONG_PTR Information; }`).
unsafe fn write_iosb(iosb: u64, status: u32, information: u64) {
    if iosb != 0 {
        *(iosb as *mut u32) = status;
        *((iosb + 8) as *mut u64) = information;
    }
}

/// Resolve an `OBJECT_ATTRIBUTES` to a registry path string: its `ObjectName`
/// (`+0x10`, a `PUNICODE_STRING`), prefixed with the `RootDirectory` (`+0x08`)
/// key's path when that handle is set.
unsafe fn oa_path(oa: u64) -> Option<alloc::string::String> {
    if oa == 0 {
        return None;
    }
    let root = *((oa + 0x08) as *const u64);
    let name = unicode_string_ascii(*((oa + 0x10) as *const u64));
    if name.is_empty() && root == 0 {
        return None;
    }
    if root != 0 {
        let base = process::current_regkey(root as i32)?;
        Some(alloc::format!("{base}\\{name}"))
    } else {
        Some(name)
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
