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
pub const NT_STUB_COUNT: u16 = 3;

const STD_INPUT_HANDLE: i32 = -10;
const STD_OUTPUT_HANDLE: i32 = -11;
const STD_ERROR_HANDLE: i32 = -12;
const INVALID_HANDLE_VALUE: i64 = -1;

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

        _ => {
            crate::kprintln!("THOS: nt unhandled call {}", idx);
            0
        }
    }
}
