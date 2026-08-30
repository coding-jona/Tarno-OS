// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — the `syscall` / `sysretq` fast path + the Linux-ABI dispatcher.
//!
//! `syscall` switches neither stack, CR3, nor GS, so the entry stub does the
//! stack + GS itself: `swapgs`, stash the user `rsp` in the per-CPU block, load
//! this thread's kernel stack (`gs:[kernel_rsp]`), build a full [`UserFrame`],
//! call the Rust dispatcher, then restore and `sysretq`. CR3 stays on the
//! caller's address space (the kernel half is mapped there).
//!
//! The full register frame is what makes `fork` / `execve` possible: `fork`
//! copies the frame into the child, `execve` builds a fresh one.
//!
//! Convention (Linux x86-64): `rax` = number; args `rdi, rsi, rdx, r10, r8, r9`;
//! return value written back to `frame.rax`.

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::registers::model_specific::{Efer, EferFlags, FsBase, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

use crate::{ext2, gdt, kprintln, process, sched, serial, smp};

static USER_EXITS: AtomicU64 = AtomicU64::new(0);

/// How many user threads have called `exit` / `exit_group` so far.
pub fn user_exits() -> u64 {
    USER_EXITS.load(Ordering::Acquire)
}

// Linux x86-64 syscall numbers.
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
const SYS_FORK: u64 = 57;
const SYS_EXECVE: u64 = 59;
const SYS_EXIT: u64 = 60;
const SYS_WAIT4: u64 = 61;
const SYS_ARCH_PRCTL: u64 = 158;
const SYS_SET_TID_ADDRESS: u64 = 218;
const SYS_EXIT_GROUP: u64 = 231;
const SYS_SET_ROBUST_LIST: u64 = 273;
const SYS_PRLIMIT64: u64 = 302;
const SYS_GETRANDOM: u64 = 318;
const SYS_RSEQ: u64 = 334;

const ENOSYS: i64 = -38;
const EBADF: i64 = -9;
const ECHILD: i64 = -10;
#[allow(dead_code)]
const _USE_ECHILD: i64 = ECHILD;
const EINVAL: i64 = -22;
const ENOTTY: i64 = -25;
const ENOENT: i64 = -2;

const ARCH_SET_FS: u64 = 0x1002;
const ARCH_GET_FS: u64 = 0x1003;

/// Full user register state at a ring transition. Field order matches the push
/// order in the entry stub and the load order in `thos_user_resume`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserFrame {
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
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub cs: u64,
    pub ss: u64,
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
    mov gs:[{user_scratch}], rsp
    mov rsp, gs:[{kernel_rsp}]          // this thread's kernel stack (16-aligned)

    sub rsp, 16                         // UserFrame.cs / .ss (unused on this path)
    push gs:[{user_scratch}]            // .rsp
    push r11                            // .rflags
    push rcx                            // .rip
    push rdi
    push rsi
    push rdx
    push rax
    push r8
    push r9
    push r10
    push r11                            // .r11 slot (user r11 is lost to SYSCALL)
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15                            // 17 pushes; rsp now %16 == 8
    sub rsp, 8                          // align to 16 for the call
    lea rdi, [rsp + 8]                  // &UserFrame
    call thos_syscall_dispatch          // writes frame.rax
    add rsp, 8                          // drop alignment pad

    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    add rsp, 8                          // skip .r11 slot
    pop r10
    pop r9
    pop r8
    pop rax                             // dispatcher's return value
    pop rdx
    pop rsi
    pop rdi
    pop rcx                             // .rip
    pop r11                            // .rflags
    pop rsp                            // user rsp
    swapgs
    sysretq

// thos_user_resume(frame: *const UserFrame in rdi) -> !
// Enter ring 3 with a full register frame (fork child / execve).
.globl thos_user_resume
thos_user_resume:
    mov r15, rdi
    push qword ptr [r15 + 18*8]         // ss
    push qword ptr [r15 + 16*8]         // rsp
    push qword ptr [r15 + 15*8]         // rflags
    push qword ptr [r15 + 17*8]         // cs
    push qword ptr [r15 + 14*8]         // rip
    mov rax, [r15 + 10*8]
    mov rdx, [r15 + 11*8]
    mov rsi, [r15 + 12*8]
    mov rdi, [r15 + 13*8]
    mov r8,  [r15 + 9*8]
    mov r9,  [r15 + 8*8]
    mov r10, [r15 + 7*8]
    mov r11, [r15 + 6*8]
    mov rbx, [r15 + 5*8]
    mov rbp, [r15 + 4*8]
    mov r12, [r15 + 3*8]
    mov r13, [r15 + 2*8]
    mov r14, [r15 + 1*8]
    mov r15, [r15 + 0*8]
    swapgs
    iretq

// Kernel-thread trampoline for a fork child: r12 = &UserFrame.
.globl thos_user_thread_resume
thos_user_thread_resume:
    mov rdi, r12
    jmp thos_user_resume
"#,
    kernel_rsp = const PERCPU_KERNEL_RSP,
    user_scratch = const PERCPU_USER_SCRATCH,
);

extern "C" {
    fn thos_syscall_entry();
    pub fn thos_user_resume(frame: *const UserFrame) -> !;
    pub fn thos_user_thread_resume() -> !;
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

fn sys_write(fd: u64, ptr: u64, len: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return EBADF;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serial::write_bytes(bytes);
    len as i64
}

/// Read a NUL-terminated string from user memory (we're under the caller's CR3).
fn user_cstr(ptr: u64) -> alloc::string::String {
    let mut s = alloc::vec::Vec::new();
    let mut p = ptr as *const u8;
    for _ in 0..4096 {
        let b = unsafe { *p };
        if b == 0 {
            break;
        }
        s.push(b);
        p = unsafe { p.add(1) };
    }
    alloc::string::String::from_utf8_lossy(&s).into_owned()
}

/// Read a NULL-terminated array of user string pointers.
fn user_cstr_array(ptr: u64) -> alloc::vec::Vec<alloc::string::String> {
    let mut out = alloc::vec::Vec::new();
    if ptr == 0 {
        return out;
    }
    let mut p = ptr as *const u64;
    for _ in 0..256 {
        let sp = unsafe { *p };
        if sp == 0 {
            break;
        }
        out.push(user_cstr(sp));
        p = unsafe { p.add(1) };
    }
    out
}

#[no_mangle]
extern "C" fn thos_syscall_dispatch(frame: &mut UserFrame) {
    let (nr, a1, a2, a3, a4, a5) =
        (frame.rax, frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8);
    let _ = (a4, a5);

    let ret: i64 = match nr {
        SYS_WRITE => sys_write(a1, a2, a3),

        SYS_WRITEV => {
            if a1 != 1 && a1 != 2 {
                EBADF
            } else {
                let iov = unsafe { core::slice::from_raw_parts(a2 as *const [u64; 2], a3 as usize) };
                let mut total = 0i64;
                for &[base, len] in iov {
                    total += sys_write(a1, base, len).max(0);
                }
                total
            }
        }

        SYS_READ => 0,

        SYS_ARCH_PRCTL => match a1 {
            ARCH_SET_FS => {
                FsBase::write(VirtAddr::new(a2));
                0
            }
            ARCH_GET_FS => {
                unsafe { *(a2 as *mut u64) = FsBase::read().as_u64() };
                0
            }
            _ => EINVAL,
        },

        SYS_BRK => sched::current_proc().map(|p| p.brk(a1) as i64).unwrap_or(EINVAL),
        SYS_MMAP => sched::current_proc().map(|p| p.mmap_anon(a2) as i64).unwrap_or(EINVAL),

        SYS_GETRANDOM => {
            let buf = unsafe { core::slice::from_raw_parts_mut(a1 as *mut u8, a2 as usize) };
            let mut x = RNG.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
            for b in buf.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = x as u8;
            }
            a2 as i64
        }

        SYS_GETPID => process::current_pid() as i64,
        SYS_SET_TID_ADDRESS => process::current_pid() as i64,
        SYS_IOCTL => ENOTTY,
        SYS_CLOSE | SYS_RT_SIGACTION | SYS_RT_SIGPROCMASK | SYS_SET_ROBUST_LIST | SYS_PRLIMIT64 => 0,
        SYS_RSEQ => ENOSYS,

        SYS_FORK => process::fork(frame),

        SYS_EXECVE => {
            let path = user_cstr(a1);
            let argv = user_cstr_array(a2);
            let envp = user_cstr_array(a3);
            match ext2::open().ok().and_then(|fs| fs.read_path(&path)) {
                Some(bytes) => process::execve(&bytes, &argv, &envp), // -> ! on success
                None => ENOENT,
            }
        }

        SYS_WAIT4 => process::wait4(a1 as i64, a2),

        SYS_EXIT | SYS_EXIT_GROUP => {
            process::set_exit_status(a1 as i32);
            USER_EXITS.fetch_add(1, Ordering::Release);
            sched::exit()
        }

        n => {
            kprintln!("THOS: unhandled syscall {}", n);
            ENOSYS
        }
    };

    frame.rax = ret as u64;
}

static RNG: AtomicU64 = AtomicU64::new(0x1234_5678_9abc_def0);
