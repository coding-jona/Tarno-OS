// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: the target of a real elevate() call. Prints its own uid — only
// meaningful evidence that `elevate` actually granted uid 0, not just that
// the syscall returned success.
fn main() {
    let uid = unsafe { libc_getuid() };
    println!("elevated-check uid={uid}");
}

/// Linux x86-64 `getuid` (syscall 102) — no libc linked in this tiny
/// binary, so call it directly the same way `do-elevate` calls the
/// THOS-native `elevate` syscall.
unsafe fn libc_getuid() -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") 102u64 => ret,
        out("rcx") _,
        out("r11") _,
    );
    ret
}
