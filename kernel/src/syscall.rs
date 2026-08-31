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

use crate::{ext2, gdt, kprintln, process, sched, smp};

static USER_EXITS: AtomicU64 = AtomicU64::new(0);

/// How many user threads have called `exit` / `exit_group` so far.
pub fn user_exits() -> u64 {
    USER_EXITS.load(Ordering::Acquire)
}

/// Count a user thread that ended without `exit` (e.g. killed by a fault).
pub fn note_user_exit() {
    USER_EXITS.fetch_add(1, Ordering::Release);
}

// Linux x86-64 syscall numbers.
const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_FSTAT: u64 = 5;
const SYS_LSEEK: u64 = 8;
const SYS_MMAP: u64 = 9;
const SYS_BRK: u64 = 12;
const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_IOCTL: u64 = 16;
const SYS_WRITEV: u64 = 20;
const SYS_GETPID: u64 = 39;
const SYS_GETUID: u64 = 102;
const SYS_GETGID: u64 = 104;
const SYS_GETEUID: u64 = 107;
const SYS_GETEGID: u64 = 108;
const SYS_FORK: u64 = 57;
const SYS_EXECVE: u64 = 59;
const SYS_EXIT: u64 = 60;
const SYS_WAIT4: u64 = 61;
const SYS_ARCH_PRCTL: u64 = 158;
const SYS_SET_TID_ADDRESS: u64 = 218;
const SYS_EXIT_GROUP: u64 = 231;
const SYS_SET_ROBUST_LIST: u64 = 273;
const SYS_PRCTL: u64 = 157;
const SYS_PRLIMIT64: u64 = 302;
const SYS_GETRANDOM: u64 = 318;
const SYS_RSEQ: u64 = 334;
const SYS_OPENAT: u64 = 257;
const SYS_UNLINK: u64 = 87;
const SYS_RMDIR: u64 = 84;
const SYS_UNLINKAT: u64 = 263;
const SYS_POLL: u64 = 7;
const SYS_DUP: u64 = 32;
const SYS_DUP2: u64 = 33;
const SYS_DUP3: u64 = 292;
const SYS_PIPE: u64 = 22;
const SYS_PIPE2: u64 = 293;
const SYS_FCNTL: u64 = 72;
const SYS_NANOSLEEP: u64 = 35;
const SYS_CLOCK_NANOSLEEP: u64 = 230;
const SYS_CHDIR: u64 = 80;
const SYS_FCHDIR: u64 = 81;
const SYS_GETPPID: u64 = 110;
const SYS_GETPGRP: u64 = 111;
const SYS_GETPGID: u64 = 121;
const SYS_SETPGID: u64 = 109;
const SYS_SETSID: u64 = 112;
const SYS_SYSINFO: u64 = 99;
const SYS_WAITID: u64 = 247;
const SYS_CLONE: u64 = 56;
const SYS_NEWFSTATAT: u64 = 262;
const SYS_GETDENTS64: u64 = 217;
const SYS_TIME: u64 = 201;
const SYS_SENDFILE: u64 = 40;
const SYS_SETUID: u64 = 105;
const SYS_SETGID: u64 = 106;
const SYS_MPROTECT: u64 = 10;
const SYS_MADVISE: u64 = 28;
const SYS_MUNMAP: u64 = 11;
const SYS_KILL: u64 = 62;
const SYS_UNAME: u64 = 63;
const SYS_GETCWD: u64 = 79;
const SYS_READLINK: u64 = 89;
const SYS_RT_SIGRETURN: u64 = 15;
const SYS_SIGALTSTACK: u64 = 131;
const SYS_GETTID: u64 = 186;
const SYS_FUTEX: u64 = 202;
const SYS_SCHED_GETAFFINITY: u64 = 204;
const SYS_TKILL: u64 = 200;
const SYS_TGKILL: u64 = 234;
const SYS_CLOCK_GETTIME: u64 = 228;
const SYS_PPOLL: u64 = 271;
const SYS_READLINKAT: u64 = 267;

const ENOSYS: i64 = -38;
const EBADF: i64 = -9;
const ECHILD: i64 = -10;
#[allow(dead_code)]
const _USE_ECHILD: i64 = ECHILD;
const EINVAL: i64 = -22;
const ENOTTY: i64 = -25;
const ENOENT: i64 = -2;
const EIO: i64 = -5;
const EISDIR: i64 = -21;
const ENOTDIR: i64 = -20;
const ENOTEMPTY: i64 = -39;

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
    call thos_finish_switch            // release the thread that yielded to us
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

fn cur_fd(fd: u64) -> Option<alloc::sync::Arc<dyn crate::file::FileOps>> {
    sched::current().task()?.fd_get(fd as i32)
}

fn sys_write(fd: u64, ptr: u64, len: u64) -> i64 {
    match cur_fd(fd) {
        Some(f) => {
            let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
            f.write(bytes)
        }
        None => EBADF,
    }
}

fn sys_read(fd: u64, ptr: u64, len: u64) -> i64 {
    match cur_fd(fd) {
        Some(f) => {
            let buf = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) };
            f.read(buf)
        }
        None => EBADF,
    }
}

/// `pipe(fds)` / `pipe2(fds, flags)` — bounded in-memory pipe, two fresh fds
/// written to `fds[0]` (read) and `fds[1]` (write). `O_CLOEXEC` (0o2000000) in
/// `flags` marks both fds close-on-exec.
fn sys_pipe(fds_ptr: u64, flags: u64) -> i64 {
    let Some(task) = sched::current().task() else { return EBADF };
    let cloexec = flags & 0o2000000 != 0;
    let (r, w) = crate::file::pipe();
    let rf: alloc::sync::Arc<dyn crate::file::FileOps> = r;
    let wf: alloc::sync::Arc<dyn crate::file::FileOps> = w;
    let rfd = task.fd_alloc_flags(rf, cloexec);
    let wfd = task.fd_alloc_flags(wf, cloexec);
    unsafe {
        *(fds_ptr as *mut i32) = rfd;
        *((fds_ptr + 4) as *mut i32) = wfd;
    }
    0
}

fn sys_unlink(path_ptr: u64, dir: bool) -> i64 {
    let path = process::resolve_path(&user_cstr(path_ptr));
    let Some(fs) = ext2::open().ok() else { return EIO };
    let r = if dir { fs.rmdir_path(&path) } else { fs.unlink_path(&path) };
    match r {
        Ok(()) => 0,
        Err("no such file") | Err("no such directory") | Err("parent dir missing") => ENOENT,
        Err("is a directory") => EISDIR,
        Err("not a directory") => ENOTDIR,
        Err("directory not empty") => ENOTEMPTY,
        Err(_) => EINVAL,
    }
}

fn sys_open(path_ptr: u64) -> i64 {
    let path = process::resolve_path(&user_cstr(path_ptr));
    let Some(task) = sched::current().task() else {
        return EBADF;
    };
    let Some(fs) = ext2::open().ok() else { return EIO };
    let Some(ino) = fs.path_lookup(&path) else { return ENOENT };
    let node = fs.read_inode(ino);
    if node.mode & 0xF000 == 0x4000 {
        // A directory: hand back a `getdents64`-able stream.
        let entries: alloc::vec::Vec<(u64, u8, alloc::string::String)> =
            fs.read_dir(ino).into_iter().map(|(i, t, n)| (i as u64, t, n)).collect();
        task.fd_alloc(crate::file::DirFile::new(&entries)) as i64
    } else {
        task.fd_alloc(crate::file::MemFile::new(fs.read_file(&node))) as i64
    }
}

/// Minimal `struct stat` (x86-64 layout): mode @24, nlink @16, size @48,
/// blksize @56, blocks @64. Everything else zero.
fn sys_fstat(fd: u64, buf: u64) -> i64 {
    let Some(f) = cur_fd(fd) else { return EBADF };
    let (mode, size) = f.stat();
    unsafe {
        core::ptr::write_bytes(buf as *mut u8, 0, 144);
        *((buf + 16) as *mut u64) = 1; // st_nlink
        *((buf + 24) as *mut u32) = mode;
        *((buf + 48) as *mut i64) = size as i64;
        *((buf + 56) as *mut i64) = 4096; // st_blksize
        *((buf + 64) as *mut i64) = ((size + 511) / 512) as i64;
    }
    0
}

/// Minimal terminal `ioctl`: report a canonical-mode line discipline with the
/// terminal's own echo *off* (our line-disciplined console already echoes and
/// edits), so BusyBox `sh` goes interactive — prompt on — but leaves line
/// editing to us.
fn sys_ioctl(fd: u64, cmd: u64, arg: u64) -> i64 {
    if cur_fd(fd).is_none() {
        return EBADF;
    }
    match cmd {
        0x5401 => unsafe {
            // TCGETS — glibc passes a 36-byte `struct __kernel_termios`
            // (__KERNEL_NCCS = 19), so never touch more than that.
            core::ptr::write_bytes(arg as *mut u8, 0, 36);
            *((arg) as *mut u32) = 0x0100; // c_iflag = ICRNL
            *((arg + 4) as *mut u32) = 0x0005; // c_oflag = OPOST | ONLCR
            *((arg + 8) as *mut u32) = 0x00bf; // c_cflag = B38400|CS8|CREAD|HUPCL
            *((arg + 12) as *mut u32) = 0x0003; // c_lflag = ISIG | ICANON  (no ECHO)
            let cc = (arg + 17) as *mut u8; // c_cc[19]
            *cc.add(0) = 3; // VINTR
            *cc.add(1) = 28; // VQUIT
            *cc.add(2) = 0x7f; // VERASE
            *cc.add(3) = 21; // VKILL
            *cc.add(4) = 4; // VEOF
            *cc.add(6) = 1; // VMIN
            0
        },
        0x5402 | 0x5403 | 0x5404 => 0, // TCSETS / TCSETSW / TCSETSF
        0x5413 => unsafe {
            // TIOCGWINSZ
            *(arg as *mut [u16; 4]) = [25, 80, 0, 0];
            0
        },
        0x540f => unsafe {
            // TIOCGPGRP
            *(arg as *mut u32) = process::current_pid() as u32;
            0
        },
        0x5410 => 0, // TIOCSPGRP
        _ => ENOTTY,
    }
}

/// `sendfile(out_fd, in_fd, off_ptr, count)` — copy `count` bytes from `in_fd`
/// to `out_fd` through a small bounce buffer. If `off_ptr` is non-NULL it names
/// the start offset in `in_fd` and receives the new offset; `in_fd`'s own file
/// position is otherwise used.
fn sys_sendfile(out_fd: u64, in_fd: u64, off_ptr: u64, count: u64) -> i64 {
    let (Some(src), Some(dst)) = (cur_fd(in_fd), cur_fd(out_fd)) else {
        return EBADF;
    };
    if off_ptr != 0 {
        let start = unsafe { *(off_ptr as *const i64) };
        if src.seek(start, crate::file::SEEK_SET) < 0 {
            return EINVAL;
        }
    }
    let mut buf = [0u8; 4096];
    let mut left = count as usize;
    let mut total: i64 = 0;
    while left > 0 {
        let want = left.min(buf.len());
        let n = src.read(&mut buf[..want]);
        if n <= 0 {
            if n < 0 && total == 0 {
                return n;
            }
            break;
        }
        let n = n as usize;
        let w = dst.write(&buf[..n]);
        if w < 0 {
            if total == 0 {
                return w;
            }
            break;
        }
        total += w;
        left -= w as usize;
        if (w as usize) < n {
            break;
        }
    }
    if off_ptr != 0 {
        let cur = src.seek(0, crate::file::SEEK_CUR);
        if cur >= 0 {
            unsafe { *(off_ptr as *mut i64) = cur };
        }
    }
    total
}

/// `newfstatat(dirfd, path, statbuf, flags)` — path resolved against the task
/// cwd (a real `dirfd` other than `AT_FDCWD` is not honoured), plus
/// `AT_EMPTY_PATH` fstat.
fn sys_newfstatat(dirfd: u64, path_ptr: u64, buf: u64, flags: u64) -> i64 {
    let raw = user_cstr(path_ptr);
    if raw.is_empty() && flags & 0x1000 != 0 {
        return sys_fstat(dirfd, buf);
    }
    let path = process::resolve_path(&raw);
    let Some(fs) = ext2::open().ok() else { return -5 /* EIO */ };
    let Some(ino) = fs.path_lookup(&path) else { return ENOENT };
    let node = fs.read_inode(ino);
    unsafe {
        core::ptr::write_bytes(buf as *mut u8, 0, 144);
        *((buf + 16) as *mut u64) = 1;
        *((buf + 24) as *mut u32) = node.mode as u32;
        *((buf + 48) as *mut i64) = node.size as i64;
        *((buf + 56) as *mut i64) = 4096;
        *((buf + 64) as *mut i64) = ((node.size + 511) / 512) as i64;
    }
    0
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
        SYS_READ => sys_read(a1, a2, a3),

        SYS_WRITEV => {
            let iov = unsafe { core::slice::from_raw_parts(a2 as *const [u64; 2], a3 as usize) };
            let mut total = 0i64;
            for &[base, len] in iov {
                let r = sys_write(a1, base, len);
                if r < 0 {
                    total = if total == 0 { r } else { total };
                    break;
                }
                total += r;
            }
            total
        }

        SYS_OPEN => sys_open(a1),
        SYS_OPENAT => sys_open(a2), // dirfd ignored; paths are absolute

        SYS_UNLINK => sys_unlink(a1, false),
        SYS_RMDIR => sys_unlink(a1, true),
        SYS_UNLINKAT => sys_unlink(a2, a3 & 0x200 != 0), // flags=a3; AT_REMOVEDIR=0x200
        SYS_CLOSE => {
            if sched::current().task().map(|t| t.fd_close(a1 as i32)).unwrap_or(false) {
                0
            } else {
                EBADF
            }
        }
        SYS_LSEEK => cur_fd(a1).map(|f| f.seek(a2 as i64, a3 as u32)).unwrap_or(EBADF),

        // time(2): seconds since the epoch. No RTC yet — a fixed plausible value
        // keeps `ls` and friends from tripping the unhandled-syscall path.
        SYS_TIME => {
            let t: i64 = 1_735_689_600; // 2025-01-01
            if a1 != 0 {
                unsafe { *(a1 as *mut i64) = t };
            }
            t
        }

        // sendfile(out, in, *offset, count): plain copy loop through a bounce
        // buffer. Lets `cat` / `cp` use their fast path instead of falling back.
        SYS_SENDFILE => sys_sendfile(a1, a2, a3, a4),

        SYS_GETDENTS64 => match cur_fd(a1) {
            Some(f) => {
                let buf = unsafe { core::slice::from_raw_parts_mut(a2 as *mut u8, a3 as usize) };
                f.getdents64(buf)
            }
            None => EBADF,
        },
        SYS_FSTAT => sys_fstat(a1, a2),

        SYS_ARCH_PRCTL => match a1 {
            ARCH_SET_FS => {
                FsBase::write(VirtAddr::new(a2));
                sched::current().set_fsbase(a2); // survive context switches
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

        SYS_GETPID | SYS_GETTID => process::current_pid() as i64,
        SYS_GETPPID => process::current_ppid() as i64,
        SYS_GETUID | SYS_GETEUID | SYS_GETGID | SYS_GETEGID => process::current_uid() as i64,
        SYS_SET_TID_ADDRESS => process::current_pid() as i64,
        SYS_IOCTL => sys_ioctl(a1, a2, a3),
        SYS_RT_SIGACTION | SYS_RT_SIGPROCMASK | SYS_RT_SIGRETURN | SYS_SET_ROBUST_LIST
        | SYS_PRLIMIT64 | SYS_SIGALTSTACK | SYS_MPROTECT | SYS_MADVISE | SYS_MUNMAP | SYS_FUTEX
        | SYS_PRCTL | SYS_FCHDIR | SYS_SETPGID | SYS_SETSID => 0,
        SYS_RSEQ => ENOSYS,

        // chdir: normalise against the cwd, verify it names a directory in ext2.
        SYS_CHDIR => {
            let path = process::resolve_path(&user_cstr(a1));
            match ext2::open().ok().and_then(|fs| {
                fs.path_lookup(&path).map(|ino| fs.read_inode(ino).mode)
            }) {
                Some(mode) if mode & 0xF000 == 0x4000 => {
                    process::set_current_cwd(path);
                    0
                }
                Some(_) => ENOTDIR,
                None => ENOENT,
            }
        }

        // No process groups; job-control shells just want a plausible answer.
        SYS_GETPGRP | SYS_GETPGID => process::current_pid() as i64,

        SYS_PIPE => sys_pipe(a1, 0),
        SYS_PIPE2 => sys_pipe(a1, a2),

        SYS_DUP => process::current_fd_dup(a1 as i32, 0) as i64,
        SYS_DUP2 => process::current_fd_dup2(a1 as i32, a2 as i32) as i64,
        // dup3(old, new, flags): flags bit O_CLOEXEC (0o2000000); old==new is EINVAL.
        SYS_DUP3 => {
            if a1 == a2 {
                EINVAL
            } else {
                process::current_fd_dup3(a1 as i32, a2 as i32, a3 & 0o2000000 != 0) as i64
            }
        }

        // fcntl: F_DUPFD(0) / F_DUPFD_CLOEXEC(1030), F_GETFD(1) / F_SETFD(2),
        // F_GETFL(3) / F_SETFL(4). FD_CLOEXEC is bit 0 of the F_*FD arg.
        SYS_FCNTL => match a2 {
            0 => process::current_fd_dup(a1 as i32, a3 as i32) as i64,
            1030 => {
                let fd = process::current_fd_dup(a1 as i32, a3 as i32);
                if fd >= 0 {
                    process::current_fd_set_cloexec(fd, true);
                }
                fd as i64
            }
            1 => process::current_fd_get_cloexec(a1 as i32) as i64,
            2 => process::current_fd_set_cloexec(a1 as i32, a3 & 1 != 0) as i64,
            3 => 0o2, // F_GETFL -> O_RDWR
            4 => 0,   // F_SETFL
            _ => 0,
        },

        SYS_NANOSLEEP | SYS_CLOCK_NANOSLEEP => {
            for _ in 0..1000 {
                sched::yield_now();
            }
            0
        }

        SYS_SYSINFO => {
            unsafe { core::ptr::write_bytes(a1 as *mut u8, 0, 112) };
            0
        }

        SYS_WAITID => ECHILD,

        // poll: mark valid fds as "no events", invalid as POLLNVAL.
        SYS_POLL | SYS_PPOLL => {
            let n = a2 as usize;
            let fds = unsafe { core::slice::from_raw_parts_mut(a1 as *mut [u8; 8], n) };
            let mut ready = 0i64;
            for pfd in fds.iter_mut() {
                let fd = i32::from_le_bytes([pfd[0], pfd[1], pfd[2], pfd[3]]);
                let revents: u16 = if cur_fd(fd as u64).is_some() { 0 } else { 0x20 /* POLLNVAL */ };
                pfd[6] = revents as u8;
                pfd[7] = (revents >> 8) as u8;
                if revents != 0 {
                    ready += 1;
                }
            }
            ready
        }

        SYS_CLOCK_GETTIME => {
            unsafe { core::ptr::write_bytes(a2 as *mut u8, 0, 16) };
            0
        }

        SYS_SCHED_GETAFFINITY => {
            // report the online CPUs as a bitmask
            let len = (a2 as usize).min(8);
            let mask = ((1u64 << smp::cpu_count().min(64)) - 1).to_le_bytes();
            unsafe { core::ptr::copy_nonoverlapping(mask.as_ptr(), a3 as *mut u8, len) };
            len as i64
        }

        SYS_KILL | SYS_TKILL | SYS_TGKILL => {
            // deliver only fatal signals; everything else is a no-op for now
            let sig = if nr == SYS_TGKILL { a3 } else { a2 };
            if sig == 6 || sig == 9 || sig == 15 {
                process::set_exit_status(128 + sig as i32);
                USER_EXITS.fetch_add(1, Ordering::Release);
                sched::exit()
            }
            0
        }

        // getcwd(buf, size): write the path + NUL, return its length incl. NUL.
        SYS_GETCWD => {
            let cwd = process::current_cwd();
            let need = cwd.len() + 1;
            if a1 == 0 || (a2 as usize) < need {
                -34 // ERANGE
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(cwd.as_ptr(), a1 as *mut u8, cwd.len());
                    *((a1 + cwd.len() as u64) as *mut u8) = 0;
                }
                need as i64
            }
        }
        SYS_READLINK | SYS_READLINKAT => EINVAL,
        SYS_UNAME => {
            unsafe { core::ptr::write_bytes(a1 as *mut u8, 0, 6 * 65) };
            0
        }

        SYS_FORK => process::fork(frame),

        // glibc's fork() is clone(SIGCHLD | CHILD_{SET,CLEAR}TID, stack=0).
        // A shared-VM clone (threads) isn't supported yet.
        SYS_CLONE => {
            const CLONE_VM: u64 = 0x100;
            if a1 & CLONE_VM != 0 {
                ENOSYS
            } else {
                process::fork(frame)
            }
        }

        SYS_NEWFSTATAT => sys_newfstatat(a1, a2, a3, a4),
        SYS_SETUID | SYS_SETGID => 0,

        SYS_EXECVE => {
            let path = process::resolve_path(&user_cstr(a1));
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
