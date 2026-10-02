// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: the scheduler must keep every process's x87/SSE registers private.
// Forks more children than there are CPUs; each loads a unique pattern into
// xmm0..xmm7 and spins in ONE asm block (so the compiler cannot reuse xmm
// registers between checks), re-reading two of them every iteration. If a
// timer-driven context switch ever leaks another process's xmm state, a child
// sees a foreign pattern and exits 1. Raw syscalls, no libc.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64, d: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d,
         out("rcx") _, out("r11") _);
    r
}

fn spin_check(pat: u64) -> i64 {
    let mut n: u64 = 40_000_000;
    let bad: u64;
    unsafe {
        asm!(
            "movq xmm0, {p}", "movq xmm1, {p}", "movq xmm2, {p}", "movq xmm3, {p}",
            "movq xmm4, {p}", "movq xmm5, {p}", "movq xmm6, {p}", "movq xmm7, {p}",
            "2:",
            "movq {t}, xmm0", "cmp {t}, {p}", "jne 3f",
            "movq {t}, xmm7", "cmp {t}, {p}", "jne 3f",
            "dec {n}", "jnz 2b",
            "xor {bad:e}, {bad:e}", "jmp 4f",
            "3: mov {bad:e}, 1",
            "4:",
            p = in(reg) pat, t = out(reg) _, n = inout(reg) n, bad = out(reg) bad,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
        );
    }
    let _ = n;
    bad as i64
}

const KIDS: u64 = 12;

fn main() {
    let mut started = 0;
    for i in 0..KIDS {
        let pid = unsafe { sys(57, 0, 0, 0, 0) }; // fork
        if pid == 0 {
            let bad = spin_check(0x0101_0101_0101_0101u64.wrapping_mul(i + 1));
            unsafe { sys(60, bad as u64, 0, 0, 0) }; // exit(bad)
        }
        if pid > 0 {
            started += 1;
        }
    }
    let mut corrupt = 0;
    for _ in 0..started {
        let mut status: i32 = 0;
        unsafe { sys(61, u64::MAX, &mut status as *mut i32 as u64, 0, 0) }; // wait4(-1)
        if (status >> 8) & 0xFF != 0 {
            corrupt += 1;
        }
    }
    if started == KIDS && corrupt == 0 {
        println!("fpu ok: {started} processes kept private xmm state across preemption");
    } else {
        println!("fpu CORRUPT: {corrupt}/{started} processes saw foreign xmm state");
    }
}
