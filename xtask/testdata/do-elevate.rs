// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: calls the THOS-native `elevate(path, argv, password)` syscall
// directly (rax = THOS_BASE, no libc — the same raw-syscall shape every
// other THOS-native/NT call from user mode uses) to spawn /elevated-check
// as uid 0, re-authenticating with the real admin password ("pass", the
// one every xtask test's `drive_login` sets up).
use std::ffi::CString;

const THOS_BASE: u64 = 0x5448_0000; // 'T' 'H' — see kernel/src/syscall.rs

fn main() {
    let path = CString::new("/elevated-check").unwrap();
    let password = CString::new("pass").unwrap();
    let argv: [*const i8; 2] = [path.as_ptr(), std::ptr::null()];

    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") THOS_BASE => ret,
            in("rdi") path.as_ptr(),
            in("rsi") argv.as_ptr(),
            in("rdx") password.as_ptr(),
            out("rcx") _,
            out("r11") _,
        );
    }
    println!("elevate returned {ret}");
}
