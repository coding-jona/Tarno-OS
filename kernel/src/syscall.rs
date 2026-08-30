// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 (prep) — the `syscall` / `sysretq` fast path.
//!
//! `syscall` does not switch stacks or GS, so the entry stub does it by hand:
//! `swapgs`, stash the user `rsp` in the per-CPU block, load the per-CPU kernel
//! stack, save the argument registers into a [`SyscallArgs`] frame, call the
//! Rust dispatcher, then `swapgs` + `sysretq` back.
//!
//! Calling convention (Linux-style): `rax` = number, args in
//! `rdi, rsi, rdx, r10, r8, r9`, return value in `rax`.
//!
//! The self-test drops to ring 3 into a 14-byte user stub, which issues
//! `SYS_WRITE` then `SYS_EXIT`; `SYS_EXIT` long-jumps back into the kernel. A
//! real process model (Phase 2/3) replaces the self-test.

use core::sync::atomic::{AtomicBool, Ordering};

use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

use crate::{gdt, kprintln, smp, vmm};

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
    mov rsp, gs:[{kernel_rsp}]          // per-CPU kernel stack

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

.globl thos_enter_ring3
// (rdi=rip, rsi=rsp, rdx=msg, rcx=len, r8=user_cs, r9=user_ss)
thos_enter_ring3:
    lea rax, [rip + 2f]
    mov [rip + THOS_RING3_RESUME + 0], rax
    mov [rip + THOS_RING3_RESUME + 8], rsp
    mov [rip + THOS_RING3_RESUME + 16], rbx
    mov [rip + THOS_RING3_RESUME + 24], rbp
    mov [rip + THOS_RING3_RESUME + 32], r12
    mov [rip + THOS_RING3_RESUME + 40], r13
    mov [rip + THOS_RING3_RESUME + 48], r14
    mov [rip + THOS_RING3_RESUME + 56], r15

    mov r10, rdi                        // user rip
    mov r11, rsi                        // user rsp
    mov rdi, rdx                        // user rdi = msg ptr
    mov rsi, rcx                        // user rsi = len

    push r9                             // SS
    push r11                           // RSP
    push 0x2                           // RFLAGS: reserved bit only (IF=0)
    push r8                            // CS
    push r10                           // RIP
    swapgs
    iretq
2:  ret

.globl thos_ring3_return
thos_ring3_return:
    mov rsp, [rip + THOS_RING3_RESUME + 8]
    mov rbx, [rip + THOS_RING3_RESUME + 16]
    mov rbp, [rip + THOS_RING3_RESUME + 24]
    mov r12, [rip + THOS_RING3_RESUME + 32]
    mov r13, [rip + THOS_RING3_RESUME + 40]
    mov r14, [rip + THOS_RING3_RESUME + 48]
    mov r15, [rip + THOS_RING3_RESUME + 56]
    jmp [rip + THOS_RING3_RESUME + 0]

.bss
.align 8
.globl THOS_RING3_RESUME
THOS_RING3_RESUME:
    .zero 64
"#,
    kernel_rsp = const PERCPU_KERNEL_RSP,
    user_scratch = const PERCPU_USER_SCRATCH,
);

extern "C" {
    fn thos_syscall_entry();
    fn thos_enter_ring3(rip: u64, rsp: u64, msg: u64, len: u64, user_cs: u64, user_ss: u64) -> ();
    fn thos_ring3_return() -> !;
}

/// Set up the `syscall` MSRs for this CPU and its kernel entry stack.
pub fn init_cpu(cpu: usize) {
    let s = gdt::selectors();
    unsafe { Efer::update(|e| e.insert(EferFlags::SYSTEM_CALL_EXTENSIONS)) };
    Star::write(s.user_code, s.user_data, s.kernel_code, s.kernel_data).expect("STAR selectors");
    LStar::write(VirtAddr::new(thos_syscall_entry as *const () as u64));
    SFMask::write(
        RFlags::INTERRUPT_FLAG | RFlags::DIRECTION_FLAG | RFlags::TRAP_FLAG | RFlags::ALIGNMENT_CHECK,
    );

    let top = smp::syscall_stack_top(cpu);
    smp::set_kernel_rsp(cpu, top);
    gdt::set_kernel_stack(cpu, VirtAddr::new(top));
}

#[no_mangle]
extern "C" fn thos_syscall_dispatch(args: &SyscallArgs) -> isize {
    match args.nr {
        SYS_WRITE => {
            let bytes = unsafe { core::slice::from_raw_parts(args.a1 as *const u8, args.a2 as usize) };
            if let Ok(s) = core::str::from_utf8(bytes) {
                crate::serial::print(s);
            }
            args.a2 as isize
        }
        SYS_GETPID => 1,
        SYS_EXIT => {
            SELFTEST_EXITED.store(true, Ordering::Release);
            unsafe { thos_ring3_return() }
        }
        _ => -1,
    }
}

static SELFTEST_EXITED: AtomicBool = AtomicBool::new(false);

const USER_CODE: u64 = 0x5555_0000_0000;
const USER_DATA: u64 = 0x5555_0000_1000;
const USER_STACK: u64 = 0x5555_0001_0000;

/// Ring-3 round-trip: map a tiny user program, drop to CPL 3, let it call
/// `SYS_WRITE` + `SYS_EXIT`.
pub fn selftest() {
    // 14-byte user stub:  mov eax,1 ; syscall ; mov eax,2 ; syscall
    let stub: [u8; 14] = [
        0xB8, 0x01, 0x00, 0x00, 0x00, 0x0F, 0x05, 0xB8, 0x02, 0x00, 0x00, 0x00, 0x0F, 0x05,
    ];
    let msg = b"hello from ring 3 via syscall\n";

    let code_frame = crate::mm::FRAME_ALLOC.lock().alloc().expect("user code frame");
    let data_frame = crate::mm::FRAME_ALLOC.lock().alloc().expect("user data frame");
    let stack_frame = crate::mm::FRAME_ALLOC.lock().alloc().expect("user stack frame");

    unsafe {
        let cptr = crate::mm::phys_to_virt(code_frame.start_address()).as_mut_ptr::<u8>();
        core::ptr::copy_nonoverlapping(stub.as_ptr(), cptr, stub.len());
        let dptr = crate::mm::phys_to_virt(data_frame.start_address()).as_mut_ptr::<u8>();
        core::ptr::copy_nonoverlapping(msg.as_ptr(), dptr, msg.len());
    }

    vmm::map_page(USER_CODE, code_frame.start_address().as_u64(), false, true, true);
    vmm::map_page(USER_DATA, data_frame.start_address().as_u64(), true, true, false);
    vmm::map_page(USER_STACK, stack_frame.start_address().as_u64(), true, true, false);

    let s = gdt::selectors();
    let user_cs = (s.user_code.0 | 3) as u64;
    let user_ss = (s.user_data.0 | 3) as u64;

    unsafe {
        thos_enter_ring3(
            USER_CODE,
            USER_STACK + 0x1000,
            USER_DATA,
            msg.len() as u64,
            user_cs,
            user_ss,
        );
    }

    let ok = SELFTEST_EXITED.load(Ordering::Acquire);
    kprintln!("THOS: syscall selftest {}", if ok { "ok (ring 3 -> SYS_WRITE -> SYS_EXIT)" } else { "FAILED" });
}
