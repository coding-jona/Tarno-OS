// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: /dev/null, /dev/zero, /dev/urandom, /dev/tty. Raw syscalls, no libc.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         out("rcx") _, out("r11") _);
    r
}
unsafe fn sys5(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         in("r10") d, in("r8") e, out("rcx") _, out("r11") _);
    r
}
fn open(path: &str, flags: u64) -> i64 {
    let mut p = path.as_bytes().to_vec();
    p.push(0);
    unsafe { sys(2, p.as_ptr() as u64, flags, 0) }
}
fn read(fd: i64, b: &mut [u8]) -> i64 { unsafe { sys(0, fd as u64, b.as_mut_ptr() as u64, b.len() as u64) } }
fn write(fd: i64, b: &[u8]) -> i64 { unsafe { sys(1, fd as u64, b.as_ptr() as u64, b.len() as u64) } }

fn main() {
    let mut bad: Vec<String> = Vec::new();

    let n = open("/dev/null", 2);
    if n < 0 { bad.push(format!("open /dev/null = {n}")); } else {
        if write(n, b"discard me") != 10 { bad.push("write /dev/null".into()); }
        let mut b = [7u8; 8];
        if read(n, &mut b) != 0 { bad.push("read /dev/null not EOF".into()); }
    }

    let z = open("/dev/zero", 0);
    if z < 0 { bad.push(format!("open /dev/zero = {z}")); } else {
        let mut b = [0xFFu8; 4096];
        if read(z, &mut b) != 4096 || b.iter().any(|&x| x != 0) { bad.push("read /dev/zero".into()); }
    }

    let r = open("/dev/urandom", 0);
    if r < 0 { bad.push(format!("open /dev/urandom = {r}")); } else {
        let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
        if read(r, &mut a) != 64 || read(r, &mut b) != 64 { bad.push("read /dev/urandom".into()); }
        if a == b || a.iter().all(|&x| x == 0) { bad.push("/dev/urandom is not random".into()); }
    }

    let t = open("/dev/tty", 1);
    if t < 0 { bad.push(format!("open /dev/tty = {t}")); } else if write(t, b"") != 0 { bad.push("write /dev/tty".into()); }

    // select(): /dev/zero is always readable; an empty pipe times out with 0.
    if z >= 0 {
        let mut rd = [0u64; 1];
        rd[0] = 1 << z;
        let mut tv = [1i64, 0];
        let n = unsafe { sys5(23, (z + 1) as u64, rd.as_mut_ptr() as u64, 0, 0, tv.as_mut_ptr() as u64) };
        if n != 1 || rd[0] & (1 << z) == 0 { bad.push(format!("select(/dev/zero) = {n}")); }
    }
    let mut fds = [0i32; 2];
    if unsafe { sys(22, fds.as_mut_ptr() as u64, 0, 0) } == 0 {
        let mut rd = [0u64; 1];
        rd[0] = 1 << fds[0];
        let mut tv = [0i64, 100_000]; // 100 ms
        let n = unsafe { sys5(23, (fds[0] + 1) as u64, rd.as_mut_ptr() as u64, 0, 0, tv.as_mut_ptr() as u64) };
        if n != 0 || rd[0] != 0 { bad.push(format!("select(empty pipe, 100ms) = {n} (want 0)")); }
    }

    if open("/dev/nonexistent", 0) != -2 { bad.push("/dev/nonexistent is not ENOENT".into()); }

    if bad.is_empty() { println!("dev ok: null zero urandom tty select"); } else { for b in &bad { println!("dev FAIL: {b}"); } }
}
