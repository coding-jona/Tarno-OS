// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: the PS/2 mouse via /dev/input/mice. Prints "mouse ready", then collects raw
// 3-byte packets for a few seconds while the host moves the pointer and presses the left
// button through the QEMU monitor. Raw syscalls, no libc.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         out("rcx") _, out("r11") _);
    r
}

fn main() {
    let path = b"/dev/input/mice\0";
    let fd = unsafe { sys(2, path.as_ptr() as u64, 0, 0) };
    if fd < 0 {
        println!("mouse FAIL: open /dev/input/mice = {fd}");
        return;
    }
    println!("mouse ready");
    let (mut dx, mut dy, mut left, mut n) = (0i32, 0i32, false, 0);
    // pollfd { fd, POLLIN }
    let mut pfd = [0u8; 8];
    pfd[0..4].copy_from_slice(&(fd as i32).to_le_bytes());
    pfd[4..6].copy_from_slice(&1i16.to_le_bytes());
    let mut quiet = 0;
    for _ in 0..400 {
        // wait up to 100 ms for a packet; stop after 2 s of silence once we have data
        let r = unsafe { sys(7, pfd.as_mut_ptr() as u64, 1, 100) };
        if r <= 0 {
            quiet += 1;
            if n > 0 && quiet >= 20 { break; } else { continue; }
        }
        quiet = 0;
        let mut p = [0u8; 3];
        let got = unsafe { sys(0, fd as u64, p.as_mut_ptr() as u64, 3) };
        if got != 3 { println!("mouse FAIL: read = {got}"); return; }
        if p[0] & 8 == 0 { println!("mouse FAIL: bad packet header {:#x}", p[0]); return; }
        dx += p[1] as i32 - if p[0] & 0x10 != 0 { 256 } else { 0 };
        dy += p[2] as i32 - if p[0] & 0x20 != 0 { 256 } else { 0 };
        left |= p[0] & 1 != 0;
        n += 1;
    }
    if n == 0 {
        println!("mouse FAIL: no packets");
    } else if dx == 0 && dy == 0 {
        println!("mouse FAIL: {n} packets but no movement");
    } else if !left {
        println!("mouse FAIL: left button never seen (dx={dx} dy={dy})");
    } else {
        println!("mouse ok: {n} packets dx={dx} dy={dy} left-button");
    }
}
