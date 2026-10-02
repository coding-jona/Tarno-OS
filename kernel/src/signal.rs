// SPDX-License-Identifier: GPL-2.0-or-later
//! POSIX signals.
//!
//! Until now `rt_sigaction` & co. were accepted and ignored, and `kill(pid, sig)`
//! killed *the caller*. This is the real thing:
//!
//!  * per-process **handlers**, **blocked mask** and **pending set** (`SigState` in
//!    `Task`); `rt_sigaction`, `rt_sigprocmask`, `rt_sigpending`, `rt_sigsuspend`,
//!    `pause`, `rt_sigreturn`;
//!  * `kill` / `tkill` / `tgkill` to a process, a process group, or everyone, with the
//!    uid permission check;
//!  * **delivery** at the end of every system call: ignore, terminate (the process
//!    reports `WIFSIGNALED`), or run a user handler on a Linux-compatible `rt_sigframe`
//!    (siginfo + ucontext + the full x87/SSE state, restored by `rt_sigreturn`);
//!  * a CPU-bound process that never makes a system call is still terminated by a
//!    fatal default signal: the timer interrupt checks for it;
//!  * blocking calls (console read, pipes, `wait4`, `nanosleep`, `pause`) return `EINTR`
//!    when a signal is pending, or are restarted for handlers installed with
//!    `SA_RESTART`;
//!  * process groups / sessions and the terminal's **foreground group**, so Ctrl+C
//!    sends `SIGINT` to the foreground processes (`console.rs`).
//!
//! Limits, stated plainly: a user handler runs only at a system-call boundary (a
//! handled signal sent to a process spinning in user mode waits for its next syscall);
//! job-control stop signals are treated as ignored (there is no `SIGCONT` machinery);
//! no real-time queues, no `SA_ONSTACK` alternate stacks.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use crate::process::{self, Task};
use crate::syscall::UserFrame;
use crate::usercopy::{self, EFAULT};
use crate::{kprintln, sched};

pub const NSIG: u32 = 64;

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGABRT: u32 = 6;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;
pub const SIGURG: u32 = 23;
pub const SIGWINCH: u32 = 28;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

const SA_SIGINFO: u64 = 0x4;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;

pub const EINTR: i64 = -4;
const EINVAL: i64 = -22;
const EPERM: i64 = -1;
const ESRCH: i64 = -3;

/// The terminal's foreground process group: where Ctrl+C / Ctrl+\ go. Set by
/// `ioctl(TIOCSPGRP)` and when a login shell is started; 0 = none.
pub static FG_PGRP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

const DEFAULT_ACTION: SigAction = SigAction { handler: SIG_DFL, flags: 0, restorer: 0, mask: 0 };

pub struct SigState {
    /// Index = signal number (1..=64); slot 0 unused.
    pub actions: [SigAction; 65],
    pub blocked: u64,
    pub pending: u64,
    /// Set by `rt_sigsuspend`: the mask to put back once its handler has run.
    pub restore_mask: Option<u64>,
}

impl SigState {
    pub const fn new() -> Self {
        Self { actions: [DEFAULT_ACTION; 65], blocked: 0, pending: 0, restore_mask: None }
    }

    /// `execve`: caught signals revert to their default; ignored ones stay ignored.
    pub fn reset_for_exec(&mut self) {
        for a in self.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = DEFAULT_ACTION;
            }
        }
        self.restore_mask = None;
    }
}

fn bit(sig: u32) -> u64 {
    1u64 << (sig - 1)
}

/// What a signal does when its action is `SIG_DFL`.
#[derive(PartialEq)]
enum Dfl {
    Ignore,
    Stop,
    Terminate,
}

fn default_action(sig: u32) -> Dfl {
    match sig {
        SIGCHLD | SIGCONT | SIGURG | SIGWINCH => Dfl::Ignore,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => Dfl::Stop,
        _ => Dfl::Terminate,
    }
}

/// Is `sig` currently discarded (explicit `SIG_IGN`, or a default that does
/// nothing here — stop signals count: there is no job-control stop yet)?
fn is_ignored(st: &SigState, sig: u32) -> bool {
    match st.actions[sig as usize].handler {
        SIG_IGN => true,
        SIG_DFL => default_action(sig) != Dfl::Terminate,
        _ => false,
    }
}

/// Signals that can never be blocked.
const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

/// Pop the lowest-numbered pending, unblocked, non-ignored signal. Pending signals
/// that turn out to be ignored are discarded on the way.
fn next_deliverable(st: &mut SigState) -> Option<u32> {
    loop {
        let avail = st.pending & !(st.blocked & !UNBLOCKABLE);
        if avail == 0 {
            return None;
        }
        let sig = avail.trailing_zeros() + 1;
        if is_ignored(st, sig) {
            st.pending &= !bit(sig);
            continue;
        }
        return Some(sig);
    }
}

/// Is there a signal the current process would act on (without consuming it)?
pub fn interrupted() -> bool {
    let Some(task) = sched::current().task() else { return false };
    let st = task.sig.lock();
    let avail = st.pending & !(st.blocked & !UNBLOCKABLE);
    (0..NSIG).any(|i| avail & (1 << i) != 0 && !is_ignored(&st, i + 1))
}

// ---------------------------------------------------------------------------
//  sending
// ---------------------------------------------------------------------------

/// Queue `sig` for `task` and wake it if it is blocked (so an interruptible wait
/// can notice).
pub fn send(task: &Arc<Task>, sig: u32) {
    if sig == 0 || sig > NSIG {
        return;
    }
    task.sig.lock().pending |= bit(sig);
    if let Some(t) = task.thread() {
        sched::unblock(t);
    }
}

/// Send to the calling process (SIGPIPE).
pub fn send_current(sig: u32) {
    if let Some(t) = sched::current().task() {
        send(&t, sig);
    }
}

/// Send to every live process of group `pgid`. Returns how many received it.
pub fn send_pgrp(pgid: u64, sig: u32) -> usize {
    if pgid == 0 {
        return 0;
    }
    let tasks = process::tasks_in_pgrp(pgid);
    for t in &tasks {
        send(t, sig);
    }
    tasks.len()
}

/// `kill(pid, sig)`.
pub fn sys_kill(pid: i64, sig: u64) -> i64 {
    if sig > NSIG as u64 {
        return EINVAL;
    }
    let sig = sig as u32;
    let me = sched::current().task();
    let (my_uid, my_pid, my_pgid) = me.as_ref().map_or((0, 0, 0), |t| (t.uid, t.pid, t.pgid()));
    let targets: Vec<Arc<Task>> = if pid > 0 {
        process::find_task(pid as u64).into_iter().collect()
    } else if pid == 0 {
        process::tasks_in_pgrp(my_pgid)
    } else if pid == -1 {
        process::all_live_tasks().into_iter().filter(|t| t.pid != my_pid && t.pid != 1).collect()
    } else {
        process::tasks_in_pgrp((-pid) as u64)
    };
    if targets.is_empty() {
        return ESRCH;
    }
    let mut allowed = 0;
    for t in &targets {
        if my_uid == 0 || my_uid == t.uid {
            allowed += 1;
            send(t, sig);
        }
    }
    if allowed == 0 {
        EPERM
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
//  sigaction / sigprocmask / sigpending / sigsuspend
// ---------------------------------------------------------------------------

/// `rt_sigaction(sig, act, oldact, sigsetsize)`.
pub fn sys_sigaction(sig: u64, act: u64, old: u64, size: u64) -> i64 {
    if size != 8 || sig == 0 || sig > NSIG as u64 {
        return EINVAL;
    }
    let sig = sig as u32;
    let Some(task) = sched::current().task() else { return EINVAL };
    if act != 0 && (sig == SIGKILL || sig == SIGSTOP) {
        return EINVAL; // can be neither caught nor ignored
    }
    let new = if act != 0 {
        let (Ok(h), Ok(f), Ok(r), Ok(m)) = (
            usercopy::read_u64(act),
            usercopy::read_u64(act + 8),
            usercopy::read_u64(act + 16),
            usercopy::read_u64(act + 24),
        ) else {
            return EFAULT;
        };
        Some(SigAction { handler: h, flags: f, restorer: r, mask: m & !UNBLOCKABLE })
    } else {
        None
    };
    let mut st = task.sig.lock();
    let cur = st.actions[sig as usize];
    if old != 0 {
        let (ok1, ok2, ok3, ok4) = (
            usercopy::write_u64(old, cur.handler).is_ok(),
            usercopy::write_u64(old + 8, cur.flags).is_ok(),
            usercopy::write_u64(old + 16, cur.restorer).is_ok(),
            usercopy::write_u64(old + 24, cur.mask).is_ok(),
        );
        if !(ok1 && ok2 && ok3 && ok4) {
            return EFAULT;
        }
    }
    if let Some(n) = new {
        st.actions[sig as usize] = n;
        if is_ignored(&st, sig) {
            st.pending &= !bit(sig); // POSIX: setting SIG_IGN discards pending ones
        }
    }
    0
}

/// `rt_sigprocmask(how, set, oldset, sigsetsize)`.
pub fn sys_sigprocmask(how: u64, set: u64, old: u64, size: u64) -> i64 {
    if size != 8 {
        return EINVAL;
    }
    let Some(task) = sched::current().task() else { return EINVAL };
    let mut st = task.sig.lock();
    let cur = st.blocked;
    if old != 0 && usercopy::write_u64(old, cur).is_err() {
        return EFAULT;
    }
    if set != 0 {
        let Ok(s) = usercopy::read_u64(set) else { return EFAULT };
        let s = s & !UNBLOCKABLE;
        st.blocked = match how {
            0 => cur | s,  // SIG_BLOCK
            1 => cur & !s, // SIG_UNBLOCK
            2 => s,        // SIG_SETMASK
            _ => return EINVAL,
        };
    }
    0
}

/// `rt_sigpending(set, sigsetsize)`.
pub fn sys_sigpending(set: u64, size: u64) -> i64 {
    if size != 8 {
        return EINVAL;
    }
    let Some(task) = sched::current().task() else { return EINVAL };
    let p = {
        let st = task.sig.lock();
        st.pending & st.blocked
    };
    match usercopy::write_u64(set, p) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// Block the calling thread until a signal it would act on is pending.
fn wait_for_signal() {
    loop {
        let me = sched::current();
        sched::mark_blocking(&me);
        if interrupted() {
            return;
        }
        sched::block_current();
    }
}

/// `pause()`.
pub fn sys_pause() -> i64 {
    wait_for_signal();
    EINTR
}

/// `rt_sigsuspend(mask, sigsetsize)`: swap the mask, wait for a signal, and put the
/// old mask back once the handler has run (the frame saved by delivery carries it).
pub fn sys_sigsuspend(mask: u64, size: u64) -> i64 {
    if size != 8 {
        return EINVAL;
    }
    let Ok(m) = usercopy::read_u64(mask) else { return EFAULT };
    let Some(task) = sched::current().task() else { return EINVAL };
    {
        let mut st = task.sig.lock();
        st.restore_mask = Some(st.blocked);
        st.blocked = m & !UNBLOCKABLE;
    }
    wait_for_signal();
    EINTR
}

// ---------------------------------------------------------------------------
//  delivery
// ---------------------------------------------------------------------------

/// End the process because of fatal signal `sig` (never returns).
fn terminate(sig: u32) -> ! {
    process::set_term_signal(sig);
    crate::syscall::note_user_exit();
    sched::exit()
}

/// Called from the timer interrupt when it interrupted *user* code: a fatal default
/// signal ends a process even if it never makes another system call (a busy loop
/// killed with Ctrl+C or `kill -9`). Handled signals wait for the next syscall.
pub fn irq_check_fatal() {
    let Some(task) = sched::current().task() else { return };
    let sig = {
        let st = task.sig.lock();
        let avail = st.pending & !(st.blocked & !UNBLOCKABLE);
        (0..NSIG).map(|i| i + 1).find(|&s| {
            avail & bit(s) != 0 && st.actions[s as usize].handler == SIG_DFL && default_action(s) == Dfl::Terminate
        })
    };
    if let Some(sig) = sig {
        terminate(sig);
    }
}

/// Offsets inside the frame we push (Linux x86-64 `rt_sigframe`):
/// `[pretcode 8][ucontext 304][siginfo 128]`, plus a 16-aligned 512-byte FXSAVE area.
const UC_OFF: usize = 8;
const MC_OFF: usize = UC_OFF + 40; // uc_mcontext
const SIGMASK_OFF: usize = UC_OFF + 296; // uc_sigmask
const INFO_OFF: usize = UC_OFF + 304;
const FRAME_LEN: usize = INFO_OFF + 128; // 440
// Indices (in u64 slots) inside `sigcontext`.
const SC_R8: usize = 0;
const SC_RDI: usize = 8;
const SC_RSI: usize = 9;
const SC_RBP: usize = 10;
const SC_RBX: usize = 11;
const SC_RDX: usize = 12;
const SC_RAX: usize = 13;
const SC_RCX: usize = 14;
const SC_RSP: usize = 15;
const SC_RIP: usize = 16;
const SC_EFLAGS: usize = 17;
const SC_FPSTATE: usize = 24;

/// Run at the end of every system call. `ret` is the call's result (it becomes
/// `rax`), `nr` its number.
pub fn deliver_at_syscall_exit(frame: &mut UserFrame, nr: u64, ret: &mut i64) {
    let Some(task) = sched::current().task() else { return };
    let (sig, act, old_mask) = {
        let mut st = task.sig.lock();
        let Some(sig) = next_deliverable(&mut st) else { return };
        st.pending &= !bit(sig);
        let old = st.restore_mask.take().unwrap_or(st.blocked);
        (sig, st.actions[sig as usize], old)
    };
    if act.handler == SIG_DFL {
        terminate(sig); // (stop/ignore defaults were filtered out above)
    }

    // A call cut short by this signal restarts after the handler if SA_RESTART says so.
    let interrupted = *ret == EINTR && nr != 15;
    let restart = interrupted && act.flags & SA_RESTART != 0;
    let (saved_rax, saved_rip) = if restart { (nr, frame.rip.wrapping_sub(2)) } else { (*ret as u64, frame.rip) };

    if !build_frame(frame, sig, &act, old_mask, saved_rax, saved_rip) {
        kprintln!("THOS: signal {} — cannot build the handler frame (bad stack or no restorer): SIGSEGV", sig);
        terminate(SIGSEGV);
    }
    {
        let mut st = task.sig.lock();
        let mut add = act.mask;
        if act.flags & SA_NODEFER == 0 {
            add |= bit(sig);
        }
        st.blocked |= add & !UNBLOCKABLE;
        if act.flags & SA_RESETHAND != 0 {
            st.actions[sig as usize] = DEFAULT_ACTION;
        }
    }
    *ret = 0;
}

/// Push a `rt_sigframe` for `sig` onto the user stack and redirect `frame` to the
/// handler. `false` if the stack cannot hold it or there is no `sa_restorer`.
fn build_frame(frame: &mut UserFrame, sig: u32, act: &SigAction, mask: u64, rax: u64, rip: u64) -> bool {
    if act.flags & SA_RESTORER == 0 || act.restorer == 0 {
        return false;
    }
    let top = frame.rsp.wrapping_sub(128); // keep the red zone
    let fp = top.wrapping_sub(512) & !15;
    let base = (fp.wrapping_sub(FRAME_LEN as u64) & !15).wrapping_sub(8); // base % 16 == 8
    let region = frame.rsp.wrapping_sub(base) as usize;
    if frame.rsp < base || !usercopy::user_ok(base, region, true) {
        return false;
    }

    let mut b = [0u8; FRAME_LEN];
    let w = |b: &mut [u8], off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    w(&mut b, 0, act.restorer); // pretcode
    let sc = |i: usize| MC_OFF + i * 8;
    let regs = [
        (SC_R8, frame.r8),
        (SC_R8 + 1, frame.r9),
        (SC_R8 + 2, frame.r10),
        (SC_R8 + 3, frame.rflags), // r11: SYSCALL stored rflags here
        (SC_R8 + 4, frame.r12),
        (SC_R8 + 5, frame.r13),
        (SC_R8 + 6, frame.r14),
        (SC_R8 + 7, frame.r15),
        (SC_RDI, frame.rdi),
        (SC_RSI, frame.rsi),
        (SC_RBP, frame.rbp),
        (SC_RBX, frame.rbx),
        (SC_RDX, frame.rdx),
        (SC_RAX, rax),
        (SC_RCX, rip),
        (SC_RSP, frame.rsp),
        (SC_RIP, rip),
        (SC_EFLAGS, frame.rflags),
        (SC_FPSTATE, fp),
    ];
    for (i, v) in regs {
        w(&mut b, sc(i), v);
    }
    w(&mut b, MC_OFF + 18 * 8, 0x33 | (0x2b << 48)); // cs / ss (informational)
    w(&mut b, SIGMASK_OFF, mask);
    // siginfo: si_signo, si_errno = 0, si_code = SI_USER (0), then si_pid / si_uid.
    b[INFO_OFF..INFO_OFF + 4].copy_from_slice(&sig.to_le_bytes());
    if let Some(t) = sched::current().task() {
        b[INFO_OFF + 16..INFO_OFF + 20].copy_from_slice(&(t.pid as u32).to_le_bytes());
        b[INFO_OFF + 20..INFO_OFF + 24].copy_from_slice(&t.uid.to_le_bytes());
    }
    let Ok(dst) = usercopy::slice_mut(base, FRAME_LEN) else { return false };
    dst.copy_from_slice(&b);
    // The user's x87/SSE registers are live in the CPU right now (the kernel never
    // touches them): park them in the frame so the handler may use them freely.
    let Ok(fx) = usercopy::slice_mut(fp, 512) else { return false };
    unsafe { core::arch::asm!("fxsave64 [{0}]", in(reg) fx.as_mut_ptr(), options(nostack, preserves_flags)) };

    frame.rsp = base;
    frame.rip = act.handler;
    frame.rdi = sig as u64;
    frame.rsi = base + INFO_OFF as u64;
    frame.rdx = base + UC_OFF as u64;
    frame.rflags &= !0x400; // DF clear on handler entry, per the ABI
    true
}

/// `rt_sigreturn()`: undo `build_frame`. Returns the restored `rax` (the dispatcher
/// stores it as the syscall result).
pub fn sys_sigreturn(frame: &mut UserFrame) -> i64 {
    let base = frame.rsp.wrapping_sub(8); // the handler's `ret` popped pretcode
    let Ok(b) = usercopy::slice(base, FRAME_LEN) else { terminate(SIGSEGV) };
    let r = |off: usize| u64::from_le_bytes(b[off..off + 8].try_into().unwrap());
    let sc = |i: usize| r(MC_OFF + i * 8);
    let (rip, rsp) = (sc(SC_RIP), sc(SC_RSP));
    // SYSRET to a non-canonical / kernel address faults in ring 0 — never let a forged
    // frame carry one.
    if rip >= usercopy::USER_TOP || rsp >= usercopy::USER_TOP || rip == 0 {
        terminate(SIGSEGV);
    }
    frame.r8 = sc(SC_R8);
    frame.r9 = sc(SC_R8 + 1);
    frame.r10 = sc(SC_R8 + 2);
    frame.r12 = sc(SC_R8 + 4);
    frame.r13 = sc(SC_R8 + 5);
    frame.r14 = sc(SC_R8 + 6);
    frame.r15 = sc(SC_R8 + 7);
    frame.rdi = sc(SC_RDI);
    frame.rsi = sc(SC_RSI);
    frame.rbp = sc(SC_RBP);
    frame.rbx = sc(SC_RBX);
    frame.rdx = sc(SC_RDX);
    frame.rip = rip;
    frame.rsp = rsp;
    // Only the user-modifiable flags; IF always on, reserved bit 1 set.
    frame.rflags = (sc(SC_EFLAGS) & 0x0CD5) | 0x202;
    let rax = sc(SC_RAX) as i64;
    let fp = sc(SC_FPSTATE);
    if fp & 15 == 0 && usercopy::user_ok(fp, 512, false) {
        unsafe { core::arch::asm!("fxrstor64 [{0}]", in(reg) fp as *const u8, options(nostack, preserves_flags)) };
    }
    if let Some(task) = sched::current().task() {
        task.sig.lock().blocked = r(SIGMASK_OFF) & !UNBLOCKABLE;
    }
    rax
}
