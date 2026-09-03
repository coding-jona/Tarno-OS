// SPDX-License-Identifier: GPL-2.0-or-later
//! NT structured-exception delivery.
//!
//! A ring-3 CPU fault in a PE process becomes a call to that process's
//! `KiUserExceptionDispatcher` with an `EXCEPTION_RECORD` + `CONTEXT` on the
//! user stack, exactly as on Windows. The dispatcher (a stub page built by
//! [`crate::pe`]) invokes the process's vectored handler and either resumes
//! via `NtContinue` or terminates. Frame-based (`.pdata` / `.xdata`) SEH
//! layers on top later.
//!
//! `#UD`, `#DE`, `#GP` and `#PF` all funnel through `thos_fault_common` →
//! [`thos_fault_dispatch`].

use crate::{kprintln, sched};

const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;
const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;

const NT_STUB_BASE: u64 = 0x0000_7FF0_0000_0000;
/// Per-process vectored-handler slot (one `PVOID`); [`crate::pe`] maps it rw.
pub const PE_EXC_ADDR: u64 = NT_STUB_BASE + 0x9000;
/// `KiUserExceptionDispatcher` code page; [`crate::pe`] maps it r-x.
pub const PE_KIUSER_ADDR: u64 = NT_STUB_BASE + 0xA000;

const CTX_SIZE: u64 = 0x4D0; // x64 CONTEXT, through the XMM save area
const CONTEXT_FULL: u32 = 0x0010_0000 | 0x1 | 0x2 | 0x4; // AMD64 | CONTROL | INTEGER | SEGMENTS
const EXR_SIZE: u64 = 0x98; // EXCEPTION_RECORD with all 15 parameter slots

/// Full ring-3 register state at a fault — like `syscall::UserFrame` but with
/// `rcx` (which SYSCALL clobbers and that frame therefore omits). Field order
/// matches the push order in `thos_fault_common` and the load order in
/// `thos_exc_resume`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ExcFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub cs: u64,
    pub ss: u64,
}

core::arch::global_asm!(
    r#"
.text

// --- no-error-code faults: fake a 0 error code, then converge ---
.globl thos_ud_entry
thos_ud_entry:
    test byte ptr [rsp + 8], 3
    jz   1f
    swapgs
1:  push 0
    push rax
    mov  rax, 6
    jmp  thos_fault_common

.globl thos_de_entry
thos_de_entry:
    test byte ptr [rsp + 8], 3
    jz   1f
    swapgs
1:  push 0
    push rax
    mov  rax, 0
    jmp  thos_fault_common

// --- error-code faults: the CPU already pushed one ---
.globl thos_gp_entry
thos_gp_entry:
    test byte ptr [rsp + 16], 3
    jz   1f
    swapgs
1:  push rax
    mov  rax, 13
    jmp  thos_fault_common

.globl thos_pf_entry
thos_pf_entry:
    test byte ptr [rsp + 16], 3
    jz   1f
    swapgs
1:  push rax
    mov  rax, 14
    jmp  thos_fault_common

// On arrival: rax = vector; stack = [saved_rax][ec][rip][cs][rflags][rsp][ss].
thos_fault_common:
    push rbx                       // scratch; frame base
    mov  rbx, rsp
    push qword ptr [rbx + 56]      // ss
    push qword ptr [rbx + 32]      // cs
    push qword ptr [rbx + 48]      // rsp_u
    push qword ptr [rbx + 40]      // rflags
    push qword ptr [rbx + 24]      // rip
    push rdi
    push rsi
    push rdx
    push rcx
    push qword ptr [rbx + 8]       // saved rax
    push r8
    push r9
    push r10
    push r11
    push qword ptr [rbx]           // saved (real user) rbx
    push rbp
    push r12
    push r13
    push r14
    push r15                       // &ExcFrame == rsp
    mov  rdi, rsp                  // &ExcFrame
    mov  rsi, rax                  // vector
    mov  rdx, [rbx + 16]           // error code
    mov  rcx, cr2                  // faulting address (only meaningful for #PF)
    sub  rsp, 8                    // 16-align
    call thos_fault_dispatch       // returns only to resume
    add  rsp, 8
    mov  rdi, rsp
    jmp  thos_exc_resume

// thos_exc_resume(frame: *const ExcFrame in rdi) -> !
.globl thos_exc_resume
thos_exc_resume:
    mov  r15, rdi
    push qword ptr [r15 + 19*8]    // ss
    push qword ptr [r15 + 17*8]    // rsp
    push qword ptr [r15 + 16*8]    // rflags
    push qword ptr [r15 + 18*8]    // cs
    push qword ptr [r15 + 15*8]    // rip
    mov  rax, [r15 + 10*8]
    mov  rcx, [r15 + 11*8]
    mov  rdx, [r15 + 12*8]
    mov  rsi, [r15 + 13*8]
    mov  rdi, [r15 + 14*8]
    mov  r8,  [r15 + 9*8]
    mov  r9,  [r15 + 8*8]
    mov  r10, [r15 + 7*8]
    mov  r11, [r15 + 6*8]
    mov  rbx, [r15 + 5*8]
    mov  rbp, [r15 + 4*8]
    mov  r12, [r15 + 3*8]
    mov  r13, [r15 + 2*8]
    mov  r14, [r15 + 1*8]
    mov  r15, [r15 + 0*8]
    swapgs
    iretq
"#
);

extern "C" {
    pub fn thos_ud_entry();
    pub fn thos_de_entry();
    pub fn thos_gp_entry();
    pub fn thos_pf_entry();
    pub fn thos_exc_resume(frame: *const ExcFrame) -> !;
}

/// Rust side of every routed fault. A PE process with an armed vectored
/// handler gets the exception delivered to ring 3; anything else is
/// fatal / killed.
#[no_mangle]
extern "C" fn thos_fault_dispatch(frame: &mut ExcFrame, vector: u64, error_code: u64, cr2: u64) {
    let from_user = frame.cs & 3 == 3;
    let handler = if from_user {
        unsafe { core::ptr::read_volatile(PE_EXC_ADDR as *const u64) }
    } else {
        0
    };
    let (name, code, sig) = match vector {
        0 => ("#DE divide error", STATUS_INTEGER_DIVIDE_BY_ZERO, 136),
        6 => ("#UD invalid opcode", STATUS_ILLEGAL_INSTRUCTION, 132),
        13 => ("#GP general protection fault", STATUS_ACCESS_VIOLATION, 139),
        _ => ("#PF page fault", STATUS_ACCESS_VIOLATION, 139),
    };

    if from_user && handler != 0 {
        let pf = (vector == 14).then_some((error_code, cr2));
        deliver(frame, code, pf);
        return;
    }

    if vector == 14 {
        kprintln!(
            "THOS trap: {}{} rip={:#x} cr2={:#x} err={:#x}",
            name,
            if from_user { " [user]" } else { "" },
            frame.rip,
            cr2,
            error_code
        );
    } else {
        kprintln!(
            "THOS trap: {}{} rip={:#x}",
            name,
            if from_user { " [user]" } else { "" },
            frame.rip
        );
    }
    if from_user {
        crate::process::set_exit_status(sig);
        crate::syscall::note_user_exit();
        sched::exit();
    }
    crate::exit_qemu(crate::ExitCode::Failed);
    crate::hcf();
}

/// Push an `EXCEPTION_RECORD` + `CONTEXT` onto the user stack and re-point
/// `frame` at `KiUserExceptionDispatcher` (`rcx` = record, `rdx` = context).
fn deliver(frame: &mut ExcFrame, code: u32, pf: Option<(u64, u64)>) {
    let mut sp = frame.rsp - 128; // skip the red zone
    sp = (sp - CTX_SIZE) & !0xF;
    let ctx = sp;
    sp = (sp - EXR_SIZE) & !0xF;
    let exr = sp;
    let new_rsp = (sp - 8) & !0xF;

    unsafe {
        core::ptr::write_bytes(exr as *mut u8, 0, EXR_SIZE as usize);
        *(exr as *mut u32) = code; // ExceptionCode
        *((exr + 0x10) as *mut u64) = frame.rip; // ExceptionAddress
        if let Some((ec, addr)) = pf {
            *((exr + 0x18) as *mut u32) = 2; // NumberParameters
            let acc = if ec & 0x10 != 0 {
                8 // execute
            } else if ec & 0x2 != 0 {
                1 // write
            } else {
                0 // read
            };
            *((exr + 0x20) as *mut u64) = acc; // ExceptionInformation[0]
            *((exr + 0x28) as *mut u64) = addr; // ExceptionInformation[1] = faulting VA
        }

        core::ptr::write_bytes(ctx as *mut u8, 0, CTX_SIZE as usize);
        *((ctx + 0x30) as *mut u32) = CONTEXT_FULL; // ContextFlags
        *((ctx + 0x38) as *mut u16) = frame.cs as u16; // SegCs
        *((ctx + 0x42) as *mut u16) = frame.ss as u16; // SegSs
        *((ctx + 0x44) as *mut u32) = frame.rflags as u32; // EFlags
        for (off, v) in [
            (0x78u64, frame.rax),
            (0x80, frame.rcx),
            (0x88, frame.rdx),
            (0x90, frame.rbx),
            (0x98, frame.rsp),
            (0xA0, frame.rbp),
            (0xA8, frame.rsi),
            (0xB0, frame.rdi),
            (0xB8, frame.r8),
            (0xC0, frame.r9),
            (0xC8, frame.r10),
            (0xD0, frame.r11),
            (0xD8, frame.r12),
            (0xE0, frame.r13),
            (0xE8, frame.r14),
            (0xF0, frame.r15),
        ] {
            *((ctx + off) as *mut u64) = v;
        }
        *((ctx + 0xF8) as *mut u64) = frame.rip; // Rip
    }

    frame.rcx = exr;
    frame.rdx = ctx;
    frame.rsp = new_rsp;
    frame.rip = PE_KIUSER_ADDR;
}
