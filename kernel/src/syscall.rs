// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — the `syscall` / `sysretq` fast path.
//!
//! `syscall` does not switch stacks, CR3, or GS, so the entry stub does the
//! stack + GS by hand: `swapgs`, stash the user `rsp` in the per-CPU block,
//! load this thread's kernel stack (`gs:[kernel_rsp]`), build a [`SyscallArgs`]
//! frame, call the Rust dispatcher, restore, `swapgs`, `sysretq`. CR3 stays on
//! the calling process's address space — the kernel half is mapped there.
//!
//! Convention (Linux-style): `rax` = number; args `rdi, rsi, rdx, r10, r8, r9`;
//! return value in `rax`.

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

use crate::{gdt, sched, serial, smp};

static USER_EXITS: AtomicU64 = AtomicU64::new(0);

/// How many user threads have called `SYS_EXIT` so far.
pub fn user_exits() -> u64 {
    USER_EXITS.load(Ordering::Acquire)
}

pub const SYS_WRITE: u64 = 1;
pub const SYS_EXIT: u64 = 2;
pub const SYS_GETPID: u64 = 3;

/// Argument frame the entry stub builds on the kernel stack.
#[repr(C)]
pub struct SyscallArgs {
    pub nr: u64,
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
    pub a6: u64,
}

const PERCPU_KERNEL_RSP: usize = 16;
const PERCPU_USER_SCRATCH: usize = 24;
const _: () = assert!(core::mem::offset_of!(smp::PerCpu, kernel_rsp) == PERCPU_KERNEL_RSP);
const _: () = assert!(core::mem::offset_of!(smp::PerCpu, user_scratch) == PERCPU_USER_SCRATCH);

core::arch::global_asm!(
    r#"
.text
.globl thos_syscall_entry
thos_syscall_entry:
    swapgs
    mov gs:[{user_scratch}], rsp        // save user rsp
    mov rsp, gs:[{kernel_rsp}]          // this thread's kernel stack

    push rcx                            // user rip  (clobbered by the call)
    push r11                            // user rflags
    sub rsp, 8                          // align to 16 before the call

    push r9                             // SyscallArgs.a6
    push r8                             // a5
    push r10                           // a4
    push rdx                           // a3
    push rsi                           // a2
    push rdi                           // a1
    push rax                           // nr  (rsp -> &SyscallArgs)

    mov rdi, rsp
    call thos_syscall_dispatch         // -> isize in rax

    add rsp, 7*8 + 8                   // drop args + alignment pad
    pop r11                            // user rflags
    pop rcx                            // user rip
    mov rsp, gs:[{user_scratch}]       // user rsp
    swapgs
    sysretq
"#,
    kernel_rsp = const PERCPU_KERNEL_RSP,
    user_scratch = const PERCPU_USER_SCRATCH,
);

extern "C" {
    fn thos_syscall_entry();
}

/// Set up the `syscall` MSRs for this CPU.
pub fn init_cpu(_cpu: usize) {
    let s = gdt::selectors();
    unsafe { Efer::update(|e| e.insert(EferFlags::SYSTEM_CALL_EXTENSIONS)) };
    Star::write(s.user_code, s.user_data, s.kernel_code, s.kernel_data).expect("STAR selectors");
    LStar::write(VirtAddr::new(thos_syscall_entry as *const () as u64));
    SFMask::write(
        RFlags::INTERRUPT_FLAG | RFlags::DIRECTION_FLAG | RFlags::TRAP_FLAG | RFlags::ALIGNMENT_CHECK,
    );
}

#[no_mangle]
extern "C" fn thos_syscall_dispatch(args: &SyscallArgs) -> isize {
    match args.nr {
        SYS_WRITE => {
            // `args.a1` is a user pointer; we run under the caller's CR3, so a
            // direct read is valid. A validating copy_from_user comes later.
            let bytes =
                unsafe { core::slice::from_raw_parts(args.a1 as *const u8, args.a2 as usize) };
            if let Ok(s) = core::str::from_utf8(bytes) {
                serial::print(s);
            }
            args.a2 as isize
        }
        SYS_GETPID => 1,
        SYS_EXIT => {
            USER_EXITS.fetch_add(1, Ordering::Release);
            sched::exit()
        }
        _ => -1,
    }
}
