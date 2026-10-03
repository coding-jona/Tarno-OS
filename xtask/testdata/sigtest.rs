// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: POSIX signals. A handler must run (on the right stack, with registers and
// SSE state intact afterwards), masks must block, SIG_IGN must ignore, SIGKILL / SIGTERM
// must kill a busy loop / a process blocked in a pipe read (wait4 sees WIFSIGNALED),
// a handled signal must interrupt nanosleep (EINTR + remaining time), SA_RESTART must
// restart a blocking read, and writing to a pipe without readers must raise SIGPIPE.
// Raw syscalls, no libc.
use std::arch::{asm, global_asm};

unsafe fn sys(n: u64, a: u64, b: u64, c: u64, d: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         in("r10") d, out("rcx") _, out("r11") _);
    r
}

// The handler lives in assembly so the test can see exactly which registers it destroys.
global_asm!(
    ".data",
    ".globl sig_hits",
    ".globl sig_last",
    "sig_hits: .quad 0",
    "sig_last: .quad 0",
    ".text",
    ".globl sig_handler",
    "sig_handler:",
    "  mov [rip + sig_last], rdi",
    "  lock inc qword ptr [rip + sig_hits]",
    "  mov r12, 0",
    "  mov r13, 0",
    "  pxor xmm0, xmm0",
    "  ret",
    ".globl sig_restorer",
    "sig_restorer:",
    "  mov rax, 15",
    "  syscall",
);
extern "C" {
    static sig_hits: u64;
    static sig_last: u64;
    fn sig_handler();
    fn sig_restorer();
}

fn hits() -> u64 {
    unsafe { std::ptr::read_volatile(&sig_hits) }
}

#[repr(C)]
struct Act {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}
const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: u64 = 0x1000_0000;

fn handle(sig: u64, flags: u64) {
    let a = Act { handler: sig_handler as u64, flags: flags | SA_RESTORER, restorer: sig_restorer as u64, mask: 0 };
    unsafe { sys(13, sig, &a as *const Act as u64, 0, 8) };
}
fn ignore(sig: u64) {
    let a = Act { handler: 1, flags: SA_RESTORER, restorer: sig_restorer as u64, mask: 0 };
    unsafe { sys(13, sig, &a as *const Act as u64, 0, 8) };
}
fn block(sig: u64, how: u64) {
    let m: u64 = 1 << (sig - 1);
    unsafe { sys(14, how, &m as *const u64 as u64, 0, 8) };
}
fn fork() -> i64 { unsafe { sys(57, 0, 0, 0, 0) } }
fn exit(c: u64) -> ! { unsafe { sys(60, c, 0, 0, 0) }; loop {} }
fn getpid() -> u64 { unsafe { sys(39, 0, 0, 0, 0) as u64 } }
fn kill(pid: u64, sig: u64) -> i64 { unsafe { sys(62, pid, sig, 0, 0) } }
fn pipe() -> [i32; 2] {
    let mut p = [0i32; 2];
    unsafe { sys(22, p.as_mut_ptr() as u64, 0, 0, 0) };
    p
}
fn close(fd: i32) { unsafe { sys(3, fd as u64, 0, 0, 0) }; }
fn nap_ms(ms: i64) {
    let req = [ms / 1000, (ms % 1000) * 1_000_000];
    unsafe { sys(35, req.as_ptr() as u64, 0, 0, 0) };
}
fn wait(pid: i64) -> u32 {
    let mut st = 0u32;
    unsafe { sys(61, pid as u64, &mut st as *mut u32 as u64, 0, 0) };
    st
}

fn main() {
    let mut bad: Vec<String> = Vec::new();
    let me = getpid();

    // 1. A handler runs before kill() returns; callee-saved registers and xmm0 survive it.
    handle(10, 0);
    let (mut r12, mut r13, mut x): (u64, u64, u64) = (0x1111_2222_3333_4444, 0x5555_6666_7777_8888, 0);
    unsafe {
        asm!(
            "movq xmm0, {pat}",
            "mov rax, 62",
            "syscall",
            "movq {x}, xmm0",
            pat = in(reg) 0xA5A5_5A5A_DEAD_BEEFu64,
            x = out(reg) x,
            in("rdi") me, in("rsi") 10u64,
            inout("r12") r12, inout("r13") r13,
            out("rax") _, out("rcx") _, out("r11") _, out("xmm0") _,
        );
    }
    if hits() != 1 || unsafe { sig_last } != 10 { bad.push(format!("handler: hits={} sig={}", hits(), unsafe { sig_last })); }
    if r12 != 0x1111_2222_3333_4444 || r13 != 0x5555_6666_7777_8888 { bad.push(format!("gpr clobbered: {r12:x} {r13:x}")); }
    if x != 0xA5A5_5A5A_DEAD_BEEF { bad.push(format!("xmm0 clobbered: {x:x}")); }

    // 2. A blocked signal waits for the unblock.
    block(10, 0); // SIG_BLOCK
    kill(me, 10);
    if hits() != 1 { bad.push(format!("blocked signal ran the handler (hits={})", hits())); }
    block(10, 1); // SIG_UNBLOCK -> delivered at the exit of this very call
    if hits() != 2 { bad.push(format!("unblocked signal not delivered (hits={})", hits())); }

    // 3. SIG_IGN: the process survives.
    ignore(12);
    if kill(me, 12) != 0 { bad.push("kill with SIG_IGN failed".into()); }

    // 4. SIGKILL ends a busy loop that never makes a system call (needs the timer IRQ path).
    let c = fork();
    if c == 0 { loop { std::hint::spin_loop(); } }
    nap_ms(100);
    kill(c as u64, 9);
    let st = wait(c);
    if st != 9 { bad.push(format!("SIGKILL of a busy loop: status {st:#x} (want 9)")); }

    // 5. SIGTERM ends a process blocked in a pipe read.
    let p = pipe();
    let c = fork();
    if c == 0 { let mut b = [0u8; 1]; unsafe { sys(0, p[0] as u64, b.as_mut_ptr() as u64, 1, 0) }; exit(0); }
    nap_ms(150);
    kill(c as u64, 15);
    let st = wait(c);
    if st != 15 { bad.push(format!("SIGTERM of a blocked read: status {st:#x} (want 15)")); }
    close(p[0]); close(p[1]);

    // 6. A handled signal interrupts nanosleep: EINTR and the time left.
    let c = fork();
    if c == 0 {
        handle(10, 0);
        let base = hits();
        let req = [5i64, 0];
        let mut rem = [0i64; 2];
        let r = unsafe { sys(35, req.as_ptr() as u64, rem.as_mut_ptr() as u64, 0, 0) };
        exit(if r == -4 && hits() == base + 1 && rem[0] >= 3 && rem[0] <= 5 { 7 } else { 1 });
    }
    nap_ms(200);
    kill(c as u64, 10);
    let st = wait(c);
    if st != 7 << 8 { bad.push(format!("nanosleep EINTR: status {st:#x} (want 0x700)")); }

    // 7. SA_RESTART: the blocked read resumes after the handler and gets the data.
    let p = pipe();
    let c = fork();
    if c == 0 {
        handle(10, SA_RESTART);
        let base = hits();
        let mut b = [0u8; 1];
        let n = unsafe { sys(0, p[0] as u64, b.as_mut_ptr() as u64, 1, 0) };
        exit(if n == 1 && b[0] == b'x' && hits() == base + 1 { 3 } else { 1 });
    }
    nap_ms(150);
    kill(c as u64, 10);
    nap_ms(100);
    unsafe { sys(1, p[1] as u64, b"x".as_ptr() as u64, 1, 0) };
    let st = wait(c);
    if st != 3 << 8 { bad.push(format!("SA_RESTART read: status {st:#x} (want 0x300)")); }
    close(p[0]); close(p[1]);

    // 8. SIGPIPE: writing with no reader kills the writer.
    let p = pipe();
    close(p[0]);
    let c = fork();
    if c == 0 {
        let mut old = Act { handler: 99, flags: 0, restorer: 0, mask: 0 };
        let mut om = 0u64;
        unsafe {
            sys(13, 13, 0, &mut old as *mut Act as u64, 8);
            sys(14, 0, 0, &mut om as *mut u64 as u64, 8);
        }
        // The shell leaves SIGPIPE ignored (SIG_IGN survives execve); restore the default.
        println!("sigpipe inherited: handler={} blocked={:#x}", old.handler, om);
        let dfl = Act { handler: 0, flags: SA_RESTORER, restorer: sig_restorer as u64, mask: 0 };
        unsafe { sys(13, 13, &dfl as *const Act as u64, 0, 8) };
        let r = unsafe { sys(1, p[1] as u64, b"x".as_ptr() as u64, 1, 0) };
        exit(if r == -32 { 5 } else { 6 }); // 5: EPIPE came back but no signal; 6: no EPIPE
    }
    let st = wait(c);
    if st != 13 { bad.push(format!("SIGPIPE: status {st:#x} (want 13; 0x500 = EPIPE without signal, 0x600 = no EPIPE)")); }

    // 10. alarm(1): SIGALRM interrupts pause() about a second later
    handle(14, 0);
    let before = hits();
    unsafe { sys(37, 1, 0, 0, 0) };
    let r = unsafe { sys(34, 0, 0, 0, 0) };
    if r != -4 || hits() != before + 1 { bad.push(format!("alarm: pause = {r}, handler runs = {}", hits() - before)); }

    // 9. kill(0 or pid) of a process that does not exist: ESRCH.
    if kill(99999, 0) != -3 { bad.push("kill(nonexistent) is not ESRCH".into()); }

    if bad.is_empty() {
        println!("sig ok: handler+mask+ign+kill+term+eintr+restart+sigpipe");
    } else {
        for b in &bad { println!("sig FAIL: {b}"); }
    }
}
