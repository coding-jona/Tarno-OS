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
pub const NT_STUB_COUNT: u16 = 8;

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
