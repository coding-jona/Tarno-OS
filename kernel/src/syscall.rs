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

use x86_64::registers::model_specific::{Efer, EferFlags, FsBase, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

use crate::{gdt, kprintln, sched, serial, smp};

static USER_EXITS: AtomicU64 = AtomicU64::new(0);

/// How many user threads have called `exit` / `exit_group` so far.
pub fn user_exits() -> u64 {
    USER_EXITS.load(Ordering::Acquire)
}

// Linux x86-64 syscall numbers (the POSIX personality speaks the Linux ABI so
// that unmodified static ELF binaries run).
const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_CLOSE: u64 = 3;
const SYS_MMAP: u64 = 9;
const SYS_BRK: u64 = 12;
const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_IOCTL: u64 = 16;
const SYS_WRITEV: u64 = 20;
const SYS_GETPID: u64 = 39;
const SYS_EXIT: u64 = 60;
const SYS_ARCH_PRCTL: u64 = 158;
const SYS_SET_TID_ADDRESS: u64 = 218;
const SYS_EXIT_GROUP: u64 = 231;
const SYS_SET_ROBUST_LIST: u64 = 273;
const SYS_PRLIMIT64: u64 = 302;
const SYS_GETRANDOM: u64 = 318;
const SYS_RSEQ: u64 = 334;

const ENOSYS: isize = -38;
const EBADF: isize = -9;
const EINVAL: isize = -22;
const ENOTTY: isize = -25;

const ARCH_SET_FS: u64 = 0x1002;
const ARCH_GET_FS: u64 = 0x1003;

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

/// Write a user buffer to the console (stdout/stderr only for now). We run
/// under the caller's CR3, so user pointers are directly readable — a
/// validating copy_from_user comes later.
fn sys_write(fd: u64, ptr: u64, len: u64) -> isize {
    if fd != 1 && fd != 2 {
        return EBADF;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serial::write_bytes(bytes);
    len as isize
}

#[no_mangle]
extern "C" fn thos_syscall_dispatch(args: &SyscallArgs) -> isize {
    match args.nr {
        SYS_WRITE => sys_write(args.a1, args.a2, args.a3),

        SYS_WRITEV => {
            if args.a1 != 1 && args.a1 != 2 {
                return EBADF;
            }
            let iov = unsafe {
                core::slice::from_raw_parts(args.a2 as *const [u64; 2], args.a3 as usize)
            };
            let mut total = 0isize;
            for &[base, len] in iov {
                total += sys_write(args.a1, base, len).max(0);
            }
            total
        }

        SYS_READ => 0, // EOF for now

        SYS_ARCH_PRCTL => match args.a1 {
            ARCH_SET_FS => {
                FsBase::write(VirtAddr::new(args.a2));
                0
            }
            ARCH_GET_FS => {
                unsafe { *(args.a2 as *mut u64) = FsBase::read().as_u64() };
                0
            }
            _ => EINVAL,
        },

        SYS_BRK => match sched::current_proc() {
            Some(p) => p.brk(args.a1) as isize,
            None => EINVAL,
        },

        SYS_MMAP => match sched::current_proc() {
            Some(p) => p.mmap_anon(args.a2) as isize,
            None => EINVAL,
        },

        SYS_GETRANDOM => {
            let buf = unsafe {
                core::slice::from_raw_parts_mut(args.a1 as *mut u8, args.a2 as usize)
            };
            let mut x = RNG.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
            for b in buf.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = x as u8;
            }
            args.a2 as isize
        }

        SYS_GETPID => 1,
        SYS_SET_TID_ADDRESS => 1,
        SYS_IOCTL => ENOTTY,
        SYS_CLOSE | SYS_RT_SIGACTION | SYS_RT_SIGPROCMASK | SYS_SET_ROBUST_LIST | SYS_PRLIMIT64 => 0,
        SYS_RSEQ => ENOSYS,

        SYS_EXIT | SYS_EXIT_GROUP => {
            USER_EXITS.fetch_add(1, Ordering::Release);
            sched::exit()
        }

        n => {
            kprintln!("THOS: unhandled syscall {}", n);
            ENOSYS
        }
    }
}

static RNG: AtomicU64 = AtomicU64::new(0x1234_5678_9abc_def0);
