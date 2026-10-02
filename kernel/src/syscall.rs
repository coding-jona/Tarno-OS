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

#[cfg(feature = "interactive")]
use crate::cred;
use crate::usercopy::{self, EFAULT};
use crate::{ext2, gdt, kprintln, power, process, sched, signal, smp, sock_sys};

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
const SYS_MKDIR: u64 = 83;
const SYS_MKDIRAT: u64 = 258;
const SYS_CHMOD: u64 = 90;
const SYS_FCHMODAT: u64 = 268;
const SYS_CHOWN: u64 = 92;
const SYS_LCHOWN: u64 = 94;
const SYS_FCHOWNAT: u64 = 260;
const SYS_UTIMENSAT: u64 = 280;

/// THOS-native calls (not part of the Linux ABI's own number space), same
/// shape as `nt::NT_BASE` — a caller does `mov eax, THOS_BASE|idx; syscall`.
/// Safe from collision: every real Linux x86-64 syscall number is well
/// under `0xFFFF`, let alone this base.
const THOS_BASE: u64 = 0x5448_0000; // 'T' 'H'
const SYS_THOS_ELEVATE: u64 = THOS_BASE;
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
const SYS_REBOOT: u64 = 169;
const SYS_GETTIMEOFDAY: u64 = 96;
const SYS_UNAME: u64 = 63;
const SYS_GETCWD: u64 = 79;
const SYS_READLINK: u64 = 89;
const SYS_RT_SIGRETURN: u64 = 15;
const SYS_SIGALTSTACK: u64 = 131;
const SYS_SOCKET: u64 = 41;
const SYS_CONNECT: u64 = 42;
const SYS_ACCEPT: u64 = 43;
const SYS_SENDTO: u64 = 44;
const SYS_RECVFROM: u64 = 45;
const SYS_SHUTDOWN: u64 = 48;
const SYS_BIND: u64 = 49;
const SYS_LISTEN: u64 = 50;
const SYS_GETSOCKNAME: u64 = 51;
const SYS_GETPEERNAME: u64 = 52;
const SYS_SETSOCKOPT: u64 = 54;
const SYS_GETSOCKOPT: u64 = 55;
const SYS_ACCEPT4: u64 = 288;
const SYS_PAUSE: u64 = 34;
const SYS_RT_SIGPENDING: u64 = 127;
const SYS_RT_SIGSUSPEND: u64 = 130;
const SYS_GETSID: u64 = 124;
const SYS_GETTID: u64 = 186;
const SYS_FUTEX: u64 = 202;
const SYS_SCHED_GETAFFINITY: u64 = 204;
const SYS_TKILL: u64 = 200;
const SYS_TGKILL: u64 = 234;
const SYS_CLOCK_GETTIME: u64 = 228;
const SYS_PPOLL: u64 = 271;
const SYS_READLINKAT: u64 = 267;

const ENOSYS: i64 = -38;
const ENOEXEC: i64 = -8;
const E2BIG: i64 = -7;
const EBADF: i64 = -9;
const ECHILD: i64 = -10;
#[allow(dead_code)]
const _USE_ECHILD: i64 = ECHILD;
const EINVAL: i64 = -22;
const ENOTTY: i64 = -25;
const ENOENT: i64 = -2;
const EIO: i64 = -5;
pub(crate) const EACCES: i64 = -13;
const EISDIR: i64 = -21;
const ENOTDIR: i64 = -20;
const ENOTEMPTY: i64 = -39;
const EEXIST: i64 = -17;
const EPERM: i64 = -1;

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
            match usercopy::slice(ptr, len as usize) {
                Ok(bytes) => f.write(bytes),
                Err(e) => e,
            }
        }
        None => EBADF,
    }
}

fn sys_read(fd: u64, ptr: u64, len: u64) -> i64 {
    match cur_fd(fd) {
        Some(f) => {
            match usercopy::slice_mut(ptr, len as usize) {
                Ok(buf) => f.read(buf),
                Err(e) => e,
            }
        }
        None => EBADF,
    }
}

/// `pipe(fds)` / `pipe2(fds, flags)` — bounded in-memory pipe, two fresh fds
/// written to `fds[0]` (read) and `fds[1]` (write). `O_CLOEXEC` (0o2000000) in
/// `flags` marks both fds close-on-exec.
fn sys_pipe(fds_ptr: u64, flags: u64) -> i64 {
    let Some(task) = sched::current().task() else { return EBADF };
    if !usercopy::user_ok(fds_ptr, 8, true) {
        return EFAULT;
    }
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
    let path = match user_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
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

fn sys_open(path_ptr: u64, flags: u64) -> i64 {
    match user_path(path_ptr) {
        Ok(p) => open_resolved(&p, flags),
        Err(e) => e,
    }
}

/// `mkdir(path, mode)` — `mode` isn't honoured yet (new directories are
/// always `0755`, matching `ext2::mkdir_path`'s prior hardcoded behaviour);
/// what's new here is a real owner (the calling task's uid) instead of the
/// permanent system uid every directory got before this increment.
fn sys_mkdir(path_ptr: u64) -> i64 {
    let path = match user_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Some(task) = sched::current().task() else { return EBADF };
    let Some(fs) = ext2::open().ok() else { return EIO };
    // Same rule as O_CREAT in `open_resolved`: creating an entry is a write
    // to the *parent* directory, checked against the parent's own mode bits.
    if let Some(parent) = parent_of(&path) {
        if let Some(pino) = fs.path_lookup(parent) {
            if !fs.read_inode(pino).access_ok(task.uid, task.gid, true) {
                return EACCES;
            }
        }
    }
    match fs.mkdir_path_owned(&path, task.uid, task.gid) {
        Ok(()) => 0,
        Err("already exists") => EEXIST,
        Err("parent dir missing") | Err("no such directory") => ENOENT,
        Err("not a directory") => ENOTDIR,
        Err(_) => EINVAL,
    }
}

/// `chmod(path, mode)` — only the owner or root (uid 0) may change a file's
/// permission bits, checked here (`ext2::chmod_path` itself does no check —
/// see its doc comment).
fn sys_chmod(path_ptr: u64, mode: u64) -> i64 {
    let path = match user_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Some(task) = sched::current().task() else { return EBADF };
    let Some(fs) = ext2::open().ok() else { return EIO };
    let Some(ino) = fs.path_lookup(&path) else { return ENOENT };
    if task.uid != 0 && task.uid != fs.read_inode(ino).uid {
        return EPERM;
    }
    match fs.chmod_path(&path, mode as u16) {
        Ok(()) => 0,
        Err(_) => EINVAL,
    }
}

/// `chown(path, uid, gid)` — root-only (matches modern Unix: even the owner
/// can't give a file away), stricter than `chmod`'s owner-or-root.
fn sys_chown(path_ptr: u64, uid: u64, gid: u64) -> i64 {
    let path = match user_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Some(task) = sched::current().task() else { return EBADF };
    if task.uid != 0 {
        return EPERM;
    }
    let Some(fs) = ext2::open().ok() else { return EIO };
    match fs.chown_path(&path, uid as u32, gid as u32) {
        Ok(()) => 0,
        Err(_) => ENOENT,
    }
}

/// THOS-native `elevate(path, argv, password)`: re-authenticate the calling
/// session's own credentials, then spawn `path` as a brand-new process
/// running uid/gid 0 — see `process::spawn_elevated` for why this is a new
/// process and not an in-place privilege upgrade. Two checks, both real:
///
/// - **Admin-only policy**: only the session that logged in as the (one)
///   admin principal may call this at all — `task.uid != cred::ADMIN_UID`
///   is `EPERM` before the password is even looked at. THOS has exactly
///   one principal that can ever be admin (`cred.rs`), so "caller in the
///   admin group" collapses to this one comparison.
/// - **Re-authentication**: the caller's freshly typed password is checked
///   against the real credential store (`cred::load` + `Cred::verify`),
///   not merely "you're already logged in" — a session left unlocked at a
///   desk can't silently elevate.
///
/// Deliberately not yet built: the **trusted path** (a secure-attention key
/// so no app can draw a fake password prompt) — that needs a global
/// keyboard-capture mechanism this slice doesn't add. Documented as a real
/// gap, not silently skipped: today `password_ptr` is whatever the calling
/// process handed the kernel directly, trusted only because THOS has
/// exactly one interactive session and no other app that could impersonate
/// this prompt yet.
#[cfg(feature = "interactive")]
fn sys_elevate(path_ptr: u64, argv_ptr: u64, password_ptr: u64) -> i64 {
    let Some(task) = sched::current().task() else { return EBADF };
    if task.uid != cred::ADMIN_UID {
        return EPERM;
    }
    let Some(fs) = ext2::open().ok() else { return EIO };
    let Some(stored) = cred::load(&fs) else { return EPERM };
    let password = user_cstr(password_ptr);
    if !stored.verify(&stored.name, &password) {
        return EACCES;
    }
    let path = match user_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Some(bytes) = fs.read_path(&path) else { return ENOENT };
    let argv = user_cstr_array(argv_ptr);
    let argv_refs: alloc::vec::Vec<&str> = argv.iter().map(alloc::string::String::as_str).collect();
    match process::spawn_elevated(task.pid, &bytes, &argv_refs, &[], 0, 0) {
        Ok(pid) => pid as i64,
        Err(_) => EINVAL,
    }
}

const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;

/// Open an already-resolved absolute path, returning a new fd (or `-errno`).
/// `flags`' `O_ACCMODE` bits (`O_RDONLY`=0, `O_WRONLY`=1, `O_RDWR`=2) decide
/// which permission(s) to actually check — shared by `open`/`openat`.
///
/// `O_CREAT`: if `path` doesn't exist yet, create it as an empty file owned
/// by the calling task's uid/gid, *provided* that task has write permission
/// on the parent directory (`Inode::access_ok`'s owner/group/other tiers) —
/// creating a file is a write to the directory it lands in, not to the
/// (not yet existing) file itself. `O_CREAT|O_EXCL` against an existing
/// path is `EEXIST`, same as Linux. `O_CREAT` against an existing path with
/// no `O_EXCL` is a no-op (POSIX: the flag is ignored).
pub fn open_resolved(path: &str, flags: u64) -> i64 {
    if let Some(r) = open_dev(path, flags & 0x3 != 1, flags & 0x3 != 0) {
        return r;
    }
    if flags & O_CREAT != 0 {
        let Some(task) = sched::current().task() else {
            return EBADF;
        };
        let Some(fs) = ext2::open().ok() else { return EIO };
        match fs.path_lookup(path) {
            Some(_) if flags & O_EXCL != 0 => return EEXIST,
            Some(_) => {}
            None => {
                if let Some(parent) = parent_of(path) {
                    if let Some(pino) = fs.path_lookup(parent) {
                        if !fs.read_inode(pino).access_ok(task.uid, task.gid, true) {
                            return EACCES;
                        }
                    }
                }
                if fs.write_path_owned(path, &[], task.uid, task.gid).is_err() {
                    return ENOENT; // parent dir missing (or similar layout failure)
                }
            }
        }
    }
    let accmode = flags & 0x3;
    open_resolved_access(path, accmode != 1, accmode != 0)
}

/// `/dev/null` & co: `Some(fd)` if `path` is a virtual device node.
fn open_dev(path: &str, want_read: bool, want_write: bool) -> Option<i64> {
    let dev = crate::file::open_device(path, want_read, want_write)?;
    let task = sched::current().task()?;
    Some(task.fd_alloc(dev) as i64)
}

/// The directory component of an absolute path (`"/"` for a top-level name).
fn parent_of(path: &str) -> Option<&str> {
    let idx = path.rfind('/')?;
    Some(if idx == 0 { "/" } else { &path[..idx] })
}

/// The permission-checked open underneath [`open_resolved`] — also the NT
/// personality's `CreateFileA`, which decides `want_read`/`want_write` from
/// `DesiredAccess` instead of `O_ACCMODE`. `EACCES` if the calling task's
/// uid/gid don't clear whichever of `want_read`/`want_write` it asked for
/// against the target inode's owner/group/other mode bits
/// (`Inode::access_ok`) — the DAC check every file open goes through now,
/// not just a mode-bits-ignored lookup.
pub fn open_resolved_access(path: &str, want_read: bool, want_write: bool) -> i64 {
    if let Some(r) = open_dev(path, want_read, want_write) {
        return r;
    }
    let Some(task) = sched::current().task() else {
        return EBADF;
    };
    let Some(fs) = ext2::open().ok() else { return EIO };
    let Some(ino) = fs.path_lookup(path) else { return ENOENT };
    let node = fs.read_inode(ino);
    if (want_read && !node.access_ok(task.uid, task.gid, false))
        || (want_write && !node.access_ok(task.uid, task.gid, true))
    {
        return EACCES;
    }
    if node.mode & 0xF000 == 0x4000 {
        // A directory: hand back a `getdents64`-able stream.
        let entries: alloc::vec::Vec<(u64, u8, alloc::string::String)> =
            fs.read_dir(ino).into_iter().map(|(i, t, n)| (i as u64, t, n)).collect();
        task.fd_alloc(crate::file::DirFile::new(&entries)) as i64
    } else {
        task.fd_alloc(crate::file::Ext2File::new(path.into(), fs.read_file(&node))) as i64
    }
}

/// Minimal `struct stat` (x86-64 layout): mode @24, nlink @16, size @48,
/// blksize @56, blocks @64. Everything else zero.
fn sys_fstat(fd: u64, buf: u64) -> i64 {
    let Some(f) = cur_fd(fd) else { return EBADF };
    if !usercopy::user_ok(buf, 144, true) {
        return EFAULT;
    }
    let (mode, size) = f.stat();
    unsafe {
        core::ptr::write_bytes(buf as *mut u8, 0, 144);
        *((buf + 16) as *mut u64) = 1; // st_nlink
        *((buf + 24) as *mut u32) = mode;
        *((buf + 48) as *mut i64) = size as i64;
        *((buf + 56) as *mut i64) = 4096; // st_blksize
        *((buf + 64) as *mut i64) = ((size + 511) / 512) as i64;
    }
    let (a, m, c) = f.times();
    fill_stat_times(buf, a, m, c);
    0
}

/// `st_atim` / `st_mtim` / `st_ctim` of the x86-64 `struct stat` (each a
/// `timespec`: seconds at +0, nanoseconds at +8) at offsets 72 / 88 / 104.
fn fill_stat_times(buf: u64, atime: u32, mtime: u32, ctime: u32) {
    unsafe {
        *((buf + 72) as *mut i64) = atime as i64;
        *((buf + 88) as *mut i64) = mtime as i64;
        *((buf + 104) as *mut i64) = ctime as i64;
    }
}

/// Minimal terminal `ioctl`: report a canonical-mode line discipline with the
/// terminal's own echo *off* (our line-disciplined console already echoes and
/// edits), so BusyBox `sh` goes interactive — prompt on — but leaves line
/// editing to us.
fn sys_ioctl(fd: u64, cmd: u64, arg: u64) -> i64 {
    if cur_fd(fd).is_none() {
        return EBADF;
    }
    // Every command below writes a fixed-size struct through `arg`.
    let need = match cmd {
        0x5401 => 36,
        0x5413 => 8,
        0x5421 => 4,
        0x540f => 4,
        _ => 0,
    };
    let need = if cmd == 0x5410 { 0 } else { need };
    if need > 0 && !usercopy::user_ok(arg, need, true) {
        return EFAULT;
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
            // TIOCGWINSZ: rows, cols of the framebuffer console (25x80 without one)
            let (cols, rows) = crate::fbcon::size();
            *(arg as *mut [u16; 4]) = [rows as u16, cols as u16, 0, 0];
            0
        },
        0x540f => unsafe {
            // TIOCGPGRP: the foreground process group of the console
            *(arg as *mut u32) = signal::FG_PGRP.load(Ordering::Relaxed) as u32;
            0
        },
        0x5421 => {
            // FIONBIO
            let on = usercopy::read_u32(arg).unwrap_or(0) != 0;
            if let Some(f) = cur_fd(fd) {
                f.set_nonblock(on);
            }
            0
        }
        0x5410 => match usercopy::read_u32(arg) {
            // TIOCSPGRP (job-control shells make a child's group the foreground one)
            Ok(g) => {
                signal::FG_PGRP.store(g as u64, Ordering::Relaxed);
                0
            }
            Err(e) => e,
        },
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
    if off_ptr != 0 && !usercopy::user_ok(off_ptr, 8, true) {
        return EFAULT;
    }
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
    let raw = match usercopy::cstr(path_ptr, 4096) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if raw.is_empty() && flags & 0x1000 != 0 {
        return sys_fstat(dirfd, buf);
    }
    let path = process::resolve_path(&raw);
    if !usercopy::user_ok(buf, 144, true) {
        return EFAULT;
    }
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
    fill_stat_times(buf, node.atime, node.mtime, node.ctime);
    0
}

/// A path argument, resolved against the cwd. A pointer that is not valid user
/// memory is `EFAULT` — **not** the empty string, which would silently mean "the
/// current directory".
fn user_path(ptr: u64) -> Result<alloc::string::String, i64> {
    Ok(process::resolve_path(&usercopy::cstr(ptr, 4096)?))
}

/// Read a NUL-terminated string from user memory (at most 4 KiB; a bad pointer
/// reads as the empty string, which every caller already treats as "no such
/// file" — nothing outside the user half is ever touched).
fn user_cstr(ptr: u64) -> alloc::string::String {
    usercopy::cstr(ptr, 4096).unwrap_or_default()
}

/// Most bytes `execve` accepts for argv + envp together (strings, their NULs and
/// the pointer arrays). The initial process stack is 64 KiB and also has to hold
/// the auxiliary vector and leave the program room to run, so this is 48 KiB —
/// Linux's equivalent is a quarter of the stack limit, up to `ARG_MAX`.
const EXEC_ARG_LIMIT: usize = 48 * 1024;
/// Most entries in argv or envp.
const EXEC_ARGC_MAX: usize = 8192;
/// Longest single argument string.
const EXEC_STR_MAX: usize = 32 * 1024;

/// Read a NULL-terminated array of user string pointers for `execve`, charging
/// every string against `budget`. Unlike [`user_cstr_array`] this never truncates:
/// a list that does not fit is `E2BIG`, so the new program can't silently see a
/// shortened command line (and `init_stack` can never be asked to write past the
/// stack it mapped).
fn user_exec_args(ptr: u64, budget: &mut usize) -> Result<alloc::vec::Vec<alloc::string::String>, i64> {
    let mut out = alloc::vec::Vec::new();
    if ptr == 0 {
        return Ok(out);
    }
    for i in 0..=EXEC_ARGC_MAX as u64 {
        let sp = usercopy::read_u64(ptr.checked_add(i * 8).ok_or(EFAULT)?)?;
        if sp == 0 {
            return Ok(out);
        }
        if out.len() == EXEC_ARGC_MAX {
            return Err(E2BIG);
        }
        let bytes = match usercopy::cstr_bytes(sp, EXEC_STR_MAX) {
            Ok(b) => b,
            Err(-36) => return Err(E2BIG), // longer than EXEC_STR_MAX
            Err(e) => return Err(e),
        };
        let cost = bytes.len() + 1 + 8; // string + NUL + its pointer slot
        if cost > *budget {
            return Err(E2BIG);
        }
        *budget -= cost;
        out.push(alloc::string::String::from_utf8_lossy(&bytes).into_owned());
    }
    Err(E2BIG)
}

/// Nanoseconds on Linux clock `id`, or `None` for an unknown clock. Realtime-like
/// clocks (REALTIME, REALTIME_COARSE, REALTIME_ALARM, TAI) give wall-clock time; all
/// the others (MONOTONIC*, BOOTTIME*, the CPU-time clocks — approximated) count from boot.
fn clock_ns(id: u64) -> Option<u64> {
    match id {
        0 | 5 | 8 | 11 => Some(crate::timer::realtime_ns()),
        1 | 2 | 3 | 4 | 6 | 7 | 9 => Some(crate::timer::monotonic_ns()),
        _ => None,
    }
}

/// `nanosleep` / `clock_nanosleep` (`flags & 1` = `TIMER_ABSTIME`).
fn sys_nanosleep(clock: u64, flags: u64, req: u64, rem: u64) -> i64 {
    let (Ok(sec), Ok(nsec)) = (usercopy::read_u64(req), usercopy::read_u64(req.wrapping_add(8))) else {
        return EFAULT;
    };
    let (sec, nsec) = (sec as i64, nsec as i64);
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return EINVAL;
    }
    let want = (sec as u64).saturating_mul(1_000_000_000).saturating_add(nsec as u64);
    let ns = if flags & 1 != 0 {
        let Some(now) = clock_ns(clock) else { return EINVAL };
        want.saturating_sub(now) // absolute deadline
    } else {
        if clock_ns(clock).is_none() {
            return EINVAL;
        }
        want
    };
    let left = crate::timer::sleep_ns(ns);
    if rem != 0 && flags & 1 == 0 {
        let _ = usercopy::write_u64(rem, left / 1_000_000_000);
        let _ = usercopy::write_u64(rem.wrapping_add(8), left % 1_000_000_000);
    }
    if left > 0 { signal::EINTR } else { 0 }
}

/// `execve(path, argv, envp)`. Does not return on success.
fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> i64 {
    let path = match user_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut budget = EXEC_ARG_LIMIT;
    let argv = match user_exec_args(argv_ptr, &mut budget) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let envp = match user_exec_args(envp_ptr, &mut budget) {
        Ok(v) => v,
        Err(e) => return e,
    };
    match ext2::open().ok().and_then(|fs| fs.read_path(&path)) {
        // Not something we can run (a script, a text file, a truncated
        // binary): ENOEXEC lets the shell fall back instead of the kernel dying
        // inside `execve`'s own `expect`.
        Some(bytes) if crate::elf::validate(&bytes).is_err() => ENOEXEC,
        Some(bytes) => process::execve(&bytes, &argv, &envp), // -> ! on success
        None => ENOENT,
    }
}

/// Read a NULL-terminated array of user string pointers (at most 256 entries;
/// used by `elevate`, whose argv is tiny).
fn user_cstr_array(ptr: u64) -> alloc::vec::Vec<alloc::string::String> {
    let mut out = alloc::vec::Vec::new();
    if ptr == 0 {
        return out;
    }
    for i in 0..256u64 {
        match usercopy::read_u64(ptr + i * 8) {
            Ok(0) | Err(_) => break,
            Ok(sp) => out.push(user_cstr(sp)),
        }
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
            if a3 > 1024 {
                frame.rax = EINVAL as u64;
                return;
            }
            if !usercopy::user_ok(a2, a3 as usize * 16, false) {
                frame.rax = EFAULT as u64;
                return;
            }
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

        SYS_OPEN => sys_open(a1, a2),
        SYS_OPENAT => sys_open(a2, a3), // dirfd ignored; paths are absolute

        SYS_UNLINK => sys_unlink(a1, false),
        SYS_RMDIR => sys_unlink(a1, true),
        SYS_UNLINKAT => sys_unlink(a2, a3 & 0x200 != 0), // flags=a3; AT_REMOVEDIR=0x200
        SYS_MKDIR => sys_mkdir(a1),
        SYS_MKDIRAT => sys_mkdir(a2), // dirfd ignored; paths are absolute
        SYS_CHMOD => sys_chmod(a1, a2),
        SYS_FCHMODAT => sys_chmod(a2, a3), // dirfd ignored; flags (a4) ignored
        SYS_CHOWN => sys_chown(a1, a2, a3),
        SYS_LCHOWN => sys_chown(a1, a2, a3), // no symlinks yet, so == chown
        SYS_FCHOWNAT => sys_chown(a2, a3, a4), // dirfd ignored; flags (a5) ignored

        // utimensat(dirfd, path, times, flags): no mtime storage yet, so
        // `times` is ignored — success is "the path exists" (a real touch of
        // an existing file becomes a no-op). A NULL path (a2==0) means
        // futimens on dirfd itself, always fine. ENOENT on a missing path
        // is deliberate: BusyBox `touch` tries utimensat first and falls
        // back to its own open(O_CREAT) only on ENOENT — this is what makes
        // that fallback actually fire instead of touch just giving up.
        SYS_UTIMENSAT => {
            if a2 == 0 {
                0
            } else {
                match user_path(a2) {
                    Err(e) => e,
                    Ok(path) => match ext2::open().ok().and_then(|fs| fs.path_lookup(&path)) {
                        Some(_) => 0,
                        None => ENOENT,
                    },
                }
            }
        }
        SYS_CLOSE => {
            if sched::current().task().map(|t| t.fd_close(a1 as i32)).unwrap_or(false) {
                0
            } else {
                EBADF
            }
        }
        SYS_LSEEK => cur_fd(a1).map(|f| f.seek(a2 as i64, a3 as u32)).unwrap_or(EBADF),

        // time(2): seconds since the epoch, from the RTC + the TSC clock.
        SYS_TIME => {
            let t = crate::timer::unix_secs() as i64;
            if a1 != 0 && usercopy::write_u64(a1, t as u64).is_err() {
                EFAULT
            } else {
                t
            }
        }

        SYS_GETTIMEOFDAY => {
            let ns = crate::timer::realtime_ns();
            if a1 != 0 {
                match usercopy::slice_mut(a1, 16) {
                    Ok(b) => {
                        b[..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
                        b[8..].copy_from_slice(&((ns % 1_000_000_000) / 1000).to_le_bytes());
                    }
                    Err(e) => {
                        frame.rax = e as u64;
                        return;
                    }
                }
            }
            if a2 != 0 {
                let _ = usercopy::slice_mut(a2, 8).map(|b| b.fill(0)); // struct timezone: UTC
            }
            0
        }

        // sendfile(out, in, *offset, count): plain copy loop through a bounce
        // buffer. Lets `cat` / `cp` use their fast path instead of falling back.
        SYS_SENDFILE => sys_sendfile(a1, a2, a3, a4),

        SYS_GETDENTS64 => match cur_fd(a1) {
            Some(f) => match usercopy::slice_mut(a2, a3 as usize) {
                Ok(buf) => f.getdents64(buf),
                Err(e) => e,
            },
            None => EBADF,
        },
        SYS_FSTAT => sys_fstat(a1, a2),

        SYS_ARCH_PRCTL => match a1 {
            ARCH_SET_FS => {
                FsBase::write(VirtAddr::new(a2));
                sched::current().set_fsbase(a2); // survive context switches
                0
            }
            ARCH_GET_FS => match usercopy::write_u64(a2, FsBase::read().as_u64()) {
                Ok(()) => 0,
                Err(e) => e,
            },
            _ => EINVAL,
        },

        SYS_BRK => sched::current_proc().map(|p| p.brk(a1) as i64).unwrap_or(EINVAL),
        SYS_MMAP => sched::current_proc().map(|p| p.mmap_anon(a2) as i64).unwrap_or(EINVAL),

        SYS_GETRANDOM => match usercopy::slice_mut(a1, a2 as usize) {
            Ok(buf) => {
                crate::random::fill(buf);
                a2 as i64
            }
            Err(e) => e,
        },

        SYS_GETPID | SYS_GETTID => process::current_pid() as i64,
        SYS_GETPPID => process::current_ppid() as i64,
        SYS_GETUID | SYS_GETEUID => process::current_uid() as i64,
        SYS_GETGID | SYS_GETEGID => process::current_gid() as i64,
        SYS_SET_TID_ADDRESS => process::current_pid() as i64,
        SYS_IOCTL => sys_ioctl(a1, a2, a3),
        SYS_RT_SIGACTION => signal::sys_sigaction(a1, a2, a3, a4),
        SYS_RT_SIGPROCMASK => signal::sys_sigprocmask(a1, a2, a3, a4),
        SYS_RT_SIGRETURN => signal::sys_sigreturn(frame),
        SYS_RT_SIGPENDING => signal::sys_sigpending(a1, a2),
        SYS_RT_SIGSUSPEND => signal::sys_sigsuspend(a1, a2),
        SYS_PAUSE => signal::sys_pause(),
        SYS_SETPGID => sys_setpgid(a1, a2),
        SYS_SETSID => sys_setsid(),
        SYS_GETSID => match if a1 == 0 { sched::current().task() } else { process::find_task(a1) } {
            Some(t) => t.sid() as i64,
            None => -3, // ESRCH
        },
        SYS_SET_ROBUST_LIST | SYS_PRLIMIT64 | SYS_SIGALTSTACK | SYS_MPROTECT | SYS_MADVISE
        | SYS_MUNMAP | SYS_FUTEX | SYS_PRCTL | SYS_FCHDIR => 0,
        SYS_RSEQ => ENOSYS,

        // chdir: normalise against the cwd, verify it names a directory in ext2.
        SYS_CHDIR => {
            let path = match user_path(a1) {
                Ok(p) => p,
                Err(e) => {
                    frame.rax = e as u64;
                    return;
                }
            };
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
        SYS_GETPGRP => sched::current().task().map_or(0, |t| t.pgid() as i64),
        SYS_GETPGID => match if a1 == 0 { sched::current().task() } else { process::find_task(a1) } {
            Some(t) => t.pgid() as i64,
            None => -3,
        },

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
            3 => 0o2 | cur_fd(a1).map_or(0, |f| if f.is_nonblock() { 0o4000 } else { 0 }), // F_GETFL
            4 => {
                // F_SETFL: only O_NONBLOCK (0o4000) is honoured, and only by sockets
                if let Some(f) = cur_fd(a1) {
                    f.set_nonblock(a3 & 0o4000 != 0);
                }
                0
            }
            _ => 0,
        },

        // nanosleep(req, rem) / clock_nanosleep(clock, flags, req, rem): a real timed
        // block on the timer wheel (10 ms resolution, never shorter than asked).
        SYS_NANOSLEEP => sys_nanosleep(0, 0, a1, a2),
        SYS_CLOCK_NANOSLEEP => sys_nanosleep(a1, a2, a3, a4),

        SYS_SYSINFO => match usercopy::slice_mut(a1, 112) {
            Ok(b) => {
                b.fill(0);
                let free = crate::mm::FRAME_ALLOC.lock().free_frames() * 4096;
                let total = crate::mm::total_frames() * 4096;
                b[0..8].copy_from_slice(&(crate::timer::monotonic_ns() / 1_000_000_000).to_le_bytes()); // uptime
                b[32..40].copy_from_slice(&total.to_le_bytes()); // totalram
                b[40..48].copy_from_slice(&free.to_le_bytes()); // freeram
                b[80..82].copy_from_slice(&1u16.to_le_bytes()); // procs (not tracked)
                b[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit
                0
            }
            Err(e) => e,
        },

        SYS_WAITID => ECHILD,

        SYS_POLL => sock_sys::sys_poll(a1, a2, if (a3 as i32) < 0 { -1 } else { (a3 as i64) * 1_000_000 }),
        SYS_PPOLL => {
            // a3 = *timespec or NULL (forever)
            let ns = if a3 == 0 {
                -1
            } else {
                match (usercopy::read_u64(a3), usercopy::read_u64(a3.wrapping_add(8))) {
                    (Ok(s), Ok(n)) => (s as i64).saturating_mul(1_000_000_000).saturating_add(n as i64),
                    _ => {
                        frame.rax = EFAULT as u64;
                        return;
                    }
                }
            };
            sock_sys::sys_poll(a1, a2, ns)
        }

        SYS_SOCKET => sock_sys::sys_socket(a1, a2, a3),
        SYS_BIND => sock_sys::sys_bind(a1, a2, a3),
        SYS_LISTEN => sock_sys::sys_listen(a1),
        SYS_CONNECT => sock_sys::sys_connect(a1, a2, a3),
        SYS_ACCEPT => sock_sys::sys_accept(a1, a2, a3, 0),
        SYS_ACCEPT4 => sock_sys::sys_accept(a1, a2, a3, a4),
        SYS_SENDTO => sock_sys::sys_sendto(a1, a2, a3, a5, frame.r9),
        SYS_RECVFROM => sock_sys::sys_recvfrom(a1, a2, a3, a5, frame.r9),
        SYS_SHUTDOWN => sock_sys::sys_shutdown(a1, a2),
        SYS_GETSOCKNAME => sock_sys::sys_getsockname(a1, a2, a3),
        SYS_GETPEERNAME => sock_sys::sys_getpeername(a1, a2, a3),
        SYS_SETSOCKOPT => sock_sys::sys_setsockopt(a1),
        SYS_GETSOCKOPT => sock_sys::sys_getsockopt(a1, a2, a3, a4, a5),

        SYS_CLOCK_GETTIME => match clock_ns(a1) {
            None => EINVAL,
            Some(ns) => match usercopy::slice_mut(a2, 16) {
                Ok(b) => {
                    b[..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
                    b[8..].copy_from_slice(&(ns % 1_000_000_000).to_le_bytes());
                    0
                }
                Err(e) => e,
            },
        },

        SYS_SCHED_GETAFFINITY => {
            // report the online CPUs as a bitmask
            let len = (a2 as usize).min(8);
            let mask = ((1u64 << smp::cpu_count().min(64)) - 1).to_le_bytes();
            match usercopy::slice_mut(a3, len) {
                Ok(b) => {
                    b.copy_from_slice(&mask[..len]);
                    len as i64
                }
                Err(e) => e,
            }
        }

        // reboot(magic1, magic2, cmd, arg): the Linux numbers BusyBox uses for
        // `poweroff -f` / `reboot -f` / `halt -f`. Any console user may do this
        // (a single-seat machine, like logind's local-session rule).
        SYS_REBOOT => {
            if a1 != 0xfee1_dead {
                EINVAL
            } else {
                match a3 {
                    0x4321_FEDC => power::poweroff(),
                    0x0123_4567 => power::reboot(),
                    0xCDEF_0123 => power::halt(),
                    0 | 0x89AB_CDEF => 0, // CAD off/on: accepted, no effect
                    _ => EINVAL,
                }
            }
        }

        // BusyBox's plain `poweroff` / `reboot` / `halt` signal init (pid 1):
        // SIGUSR2 = poweroff, SIGTERM = reboot, SIGUSR1 = halt. THOS has no
        // userspace init, so the kernel plays that role for pid 1.
        SYS_KILL if a1 == 1 => match a2 {
            12 => power::poweroff(),
            15 => power::reboot(),
            10 => power::halt(),
            _ => 0,
        },

        SYS_KILL => signal::sys_kill(a1 as i64, a2),
        // THOS has one thread per task, so a tid is a pid.
        SYS_TKILL => signal::sys_kill(a1 as i64, a2),
        SYS_TGKILL => signal::sys_kill(a2 as i64, a3),

        // getcwd(buf, size): write the path + NUL, return its length incl. NUL.
        SYS_GETCWD => {
            let cwd = process::current_cwd();
            let need = cwd.len() + 1;
            if a1 == 0 || (a2 as usize) < need {
                -34 // ERANGE
            } else {
                match usercopy::slice_mut(a1, need) {
                    Ok(b) => {
                        b[..cwd.len()].copy_from_slice(cwd.as_bytes());
                        b[cwd.len()] = 0;
                        need as i64
                    }
                    Err(e) => e,
                }
            }
        }
        SYS_READLINK | SYS_READLINKAT => EINVAL,
        SYS_UNAME => match usercopy::slice_mut(a1, 6 * 65) {
            Ok(b) => {
                b.fill(0);
                0
            }
            Err(e) => e,
        },

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

        SYS_EXECVE => sys_execve(a1, a2, a3),

        SYS_WAIT4 => process::wait4(a1 as i64, a2),

        SYS_EXIT | SYS_EXIT_GROUP => {
            process::set_exit_status(a1 as i32);
            USER_EXITS.fetch_add(1, Ordering::Release);
            sched::exit()
        }

        // NT-personality calls from a PE's import stubs.
        n if n & !0xFFFF == crate::nt::NT_BASE => crate::nt::dispatch((n & 0xFFFF) as u16, frame),

        // THOS-native calls — outside the Linux ABI's own number space,
        // same shape as the NT range above. Just `elevate` so far — and
        // there is no credential store (`cred.rs`) to re-authenticate
        // against outside the `interactive` build, so it's ENOSYS there.
        #[cfg(feature = "interactive")]
        SYS_THOS_ELEVATE => sys_elevate(a1, a2, a3),
        #[cfg(not(feature = "interactive"))]
        SYS_THOS_ELEVATE => ENOSYS,

        n => {
            kprintln!("THOS: unhandled syscall {}", n);
            ENOSYS
        }
    };

    let mut ret = ret;
    // A write to a pipe nobody reads raises SIGPIPE — here, in the syscall layer, so
    // the kernel's own pipe writes (the security service) never kill their caller.
    if ret == -32 && matches!(nr, SYS_WRITE | SYS_WRITEV) {
        signal::send_current(signal::SIGPIPE);
    }
    signal::deliver_at_syscall_exit(frame, nr, &mut ret);
    frame.rax = ret as u64;
}

/// `setpgid(pid, pgid)`: 0 means "the caller" for both arguments. Only the caller
/// itself or a child of the same session may be moved.
fn sys_setpgid(pid: u64, pgid: u64) -> i64 {
    let Some(me) = sched::current().task() else { return -3 };
    let target = if pid == 0 || pid == me.pid { me.clone() } else {
        match process::find_task(pid) {
            Some(t) if t.ppid == me.pid && t.sid() == me.sid() => t,
            Some(_) => return -1, // EPERM
            None => return -3,
        }
    };
    let g = if pgid == 0 { target.pid } else { pgid };
    target.set_pgid(g);
    0
}

/// `setsid()`: a new session and process group led by the caller.
fn sys_setsid() -> i64 {
    let Some(me) = sched::current().task() else { return -3 };
    if me.pgid() == me.pid && me.sid() == me.pid {
        return -1; // already a group leader (EPERM)
    }
    me.set_sid(me.pid);
    me.set_pgid(me.pid);
    me.pid as i64
}

