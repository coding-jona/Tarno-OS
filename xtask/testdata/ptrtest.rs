// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: the kernel must never trust a pointer a process hands it. Every
// call below passes either a *kernel* address (HHDM / kernel image — a
// read/write primitive on kernel memory if unchecked) or an unmapped user
// address, and must come back -EFAULT (-14). Raw syscalls, no libc. One valid
// call at the end proves the checks did not break the normal path.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         in("r10") d, in("r8") e, out("rcx") _, out("r11") _);
    r
}

const EFAULT: i64 = -14;
const KPTR: u64 = 0xFFFF_8000_0000_1000; // inside the kernel's direct map
const KIMG: u64 = 0xFFFF_FFFF_8000_0000; // the kernel image
const UNMAPPED: u64 = 0x0000_7123_0000_0000; // user half, nothing mapped there

fn main() {
    let mut total = 0;
    let mut bad: Vec<String> = Vec::new();
    let mut check = |name: &str, got: i64| {
        total += 1;
        if got != EFAULT {
            bad.push(format!("{name}={got}"));
        }
    };
    unsafe {
        // write(1, ptr, 16)
        check("write(kernel)", sys(1, 1, KPTR, 16, 0, 0));
        check("write(kimage)", sys(1, 1, KIMG, 16, 0, 0));
        check("write(unmapped)", sys(1, 1, UNMAPPED, 16, 0, 0));
        // read from a pipe INTO kernel memory
        let mut fds = [0i32; 2];
        sys(22, fds.as_mut_ptr() as u64, 0, 0, 0, 0); // pipe
        sys(1, fds[1] as u64, b"x".as_ptr() as u64, 1, 0, 0);
        check("read(kernel)", sys(0, fds[0] as u64, KPTR, 1, 0, 0));
        check("read(unmapped)", sys(0, fds[0] as u64, UNMAPPED, 1, 0, 0));
        // stat family / ioctl / misc out-parameters
        check("fstat(kernel)", sys(5, 1, KPTR, 0, 0, 0));
        check("fstat(unmapped)", sys(5, 1, UNMAPPED, 0, 0, 0));
        check("ioctl TCGETS(kernel)", sys(16, 1, 0x5401, KPTR, 0, 0));
        check("uname(kernel)", sys(63, KPTR, 0, 0, 0, 0));
        check("getcwd(kernel)", sys(79, KPTR, 64, 0, 0, 0));
        check("pipe(kernel)", sys(22, KPTR, 0, 0, 0, 0));
        check("getrandom(kernel)", sys(318, KPTR, 16, 0, 0, 0));
        check("clock_gettime(kernel)", sys(228, 0, KPTR, 0, 0, 0));
        // getdents64 into kernel memory
        let root = b"/\0";
        let dfd = sys(2, root.as_ptr() as u64, 0, 0, 0, 0); // open("/")
        check("getdents64(kernel)", sys(217, dfd as u64, KPTR, 64, 0, 0));
        // writev with the iovec array in kernel memory
        check("writev(kernel iov)", sys(20, 1, KPTR, 1, 0, 0));
        // execve with argv in kernel memory
        let bb = b"/busybox\0";
        check("execve(kernel argv)", sys(59, bb.as_ptr() as u64, KPTR, 0, 0, 0));
        // path string in kernel memory must not open anything
        let r = sys(2, KPTR, 0, 0, 0, 0);
        total += 1;
        if r >= 0 {
            bad.push(format!("open(kernel path)={r}"));
        }
        // the normal path still works
        let msg = b"ptrtest: valid write\n";
        let w = sys(1, 1, msg.as_ptr() as u64, msg.len() as u64, 0, 0);
        total += 1;
        if w != msg.len() as i64 {
            bad.push(format!("valid write={w}"));
        }
    }
    if bad.is_empty() {
        println!("ptr ok: {total}/{total} bad pointers refused with EFAULT, valid calls unaffected");
    } else {
        println!("ptr FAIL: {}/{total} wrong: {}", bad.len(), bad.join(", "));
    }
}
