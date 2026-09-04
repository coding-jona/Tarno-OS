// SPDX-License-Identifier: GPL-2.0-or-later
//! NT user-mode APC delivery.
//!
//! A queued user APC is handed to the target thread the next time it becomes
//! *alertable*: `NtTestAlert`, or the `TestAlert` tail of `NtContinue`. (An
//! alertable `NtWaitForSingleObject` will hook in here too once the wait path
//! grows a real block.) Delivery mirrors Windows/x64: a `CONTEXT` capturing the
//! interrupted register state is pushed to the user stack with the APC
//! parameters in its home area, and control is redirected to
//! `KiUserApcDispatcher` — a stub page built by [`crate::pe`] — which calls the
//! routine and then `NtContinue(&ctx, TestAlert=TRUE)` to resume, draining any
//! further queued APCs on the way out.

use crate::process::{self, ApcEntry};

const NT_STUB_BASE: u64 = 0x0000_7FF0_0000_0000;
/// `KiUserApcDispatcher` code page; [`crate::pe`] maps it r-x.
pub const PE_KIUSERAPC_ADDR: u64 = NT_STUB_BASE + 0xB000;

const CTX_SIZE: u64 = 0x4D0; // x64 CONTEXT, through the XMM save area
const CONTEXT_FULL: u32 = 0x0010_0000 | 0x1 | 0x2 | 0x4; // AMD64 | CONTROL | INTEGER | SEGMENTS

/// The register state to resume once a delivered APC (and anything it chains
/// to) has run. Populated from a `syscall::UserFrame` or a `seh::ExcFrame`.
#[derive(Clone, Copy, Default)]
pub struct Regs {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
    pub cs: u64,
    pub ss: u64,
}

/// Write a full x64 `CONTEXT` for `r` at `dst` (caller guarantees `dst` points
/// at `CTX_SIZE` writable user bytes).
unsafe fn write_context(dst: u64, r: &Regs) {
    core::ptr::write_bytes(dst as *mut u8, 0, CTX_SIZE as usize);
    *((dst + 0x30) as *mut u32) = CONTEXT_FULL; // ContextFlags
    *((dst + 0x38) as *mut u16) = r.cs as u16; // SegCs
    *((dst + 0x42) as *mut u16) = r.ss as u16; // SegSs
    *((dst + 0x44) as *mut u32) = r.rflags as u32; // EFlags
    for (off, v) in [
        (0x78u64, r.rax),
        (0x80, r.rcx),
        (0x88, r.rdx),
        (0x90, r.rbx),
        (0x98, r.rsp),
        (0xA0, r.rbp),
        (0xA8, r.rsi),
        (0xB0, r.rdi),
        (0xB8, r.r8),
        (0xC0, r.r9),
        (0xC8, r.r10),
        (0xD0, r.r11),
        (0xD8, r.r12),
        (0xE0, r.r13),
        (0xE8, r.r14),
        (0xF0, r.r15),
    ] {
        *((dst + off) as *mut u64) = v;
    }
    *((dst + 0xF8) as *mut u64) = r.rip; // Rip
}

/// Stage one APC on top of `r`'s state: push a `CONTEXT` (with the APC
/// parameters in its `P1Home..P4Home` area, the layout the `KiUserApcDispatcher`
/// stub reads) to the user stack and return `(new_rsp, new_rip)` — `new_rip`
/// being `KiUserApcDispatcher`.
pub fn stage(r: &Regs, e: &ApcEntry) -> (u64, u64) {
    let mut sp = r.rsp - 128; // skip the red zone
    sp = (sp - CTX_SIZE) & !0xF; // 16-aligned CONTEXT == dispatcher entry rsp
    let ctx = sp;
    unsafe {
        write_context(ctx, r);
        *((ctx + 0x00) as *mut u64) = e.arg1; // P1Home = NormalContext (ApcArgument1)
        *((ctx + 0x08) as *mut u64) = e.arg2; // P2Home = SystemArgument1 (ApcArgument2)
        *((ctx + 0x10) as *mut u64) = e.arg3; // P3Home = SystemArgument2 (ApcArgument3)
        *((ctx + 0x18) as *mut u64) = e.routine; // P4Home = NormalRoutine (ApcRoutine)
    }
    (ctx, PE_KIUSERAPC_ADDR)
}

/// If the current thread has a pending user APC, dequeue it and stage it over
/// `r`, returning the redirected `(rsp, rip)`. `None` if the queue is empty.
pub fn take_and_stage(r: &Regs) -> Option<(u64, u64)> {
    let e = process::current_take_apc()?;
    Some(stage(r, &e))
}
