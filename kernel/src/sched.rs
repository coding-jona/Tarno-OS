// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — the kernel thread scheduler.
//!
//! Preemptive, round-robin, one shared ready queue that every CPU (BSP + APs)
//! schedules from. Preemption is driven by the per-CPU APIC timer tick.
//!
//! There is exactly one kind of runnable entity here — a kernel `Thread`. In
//! Phase 2/3 a user thread is the same object with a user-mode context bolted
//! on; the wait primitive in `wait.rs` blocks/wakes *these* threads, and
//! `futex` / `KEVENT` / a Win32 event `HANDLE` all resolve down to it.
//!
//! Context switch saves only the SysV callee-saved registers + `rsp`; the
//! interrupt frame, when a switch happens from the timer handler, stays on the
//! preempted thread's own kernel stack.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::PhysFrame;
use x86_64::{PhysAddr, VirtAddr};

use crate::gdt::MAX_CPUS;
use crate::process::Process;
use crate::{gdt, smp, vmm};

core::arch::global_asm!(
    r#"
.text
.globl thos_ctx_switch
// (save_to: *mut u64 in rdi, load_from: *const u64 in rsi)
thos_ctx_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, [rsi]
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.globl thos_thread_trampoline
// entered via `ret` from thos_ctx_switch with r12=entry, r13=arg
thos_thread_trampoline:
    mov rdi, r13
    call r12
    call thos_thread_exit

.globl thos_user_thread_start
// entered via `ret` from thos_ctx_switch with
//   r12=user rip, r13=user rsp, r14=user cs, r15=user ss
thos_user_thread_start:
    push r15
    push r13
    push 0x202          // RFLAGS with IF=1 -> the user thread is preemptible
    push r14
    push r12
    swapgs
    iretq
"#
);

extern "C" {
    fn thos_ctx_switch(save_to: *mut u64, load_from: *const u64);
    fn thos_thread_trampoline() -> !;
    fn thos_user_thread_start() -> !;
}

#[no_mangle]
extern "C" fn thos_thread_exit() -> ! {
    exit()
}

const KSTACK_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Blocked,
    Exited,
}

#[allow(dead_code)] // id/name/proc are for debugging, lifetime, /proc-style views
pub struct Thread {
    pub id: u64,
    pub name: &'static str,
    state: Mutex<State>,
    /// Saved `rsp` for the next `thos_ctx_switch` into this thread.
    ctx: UnsafeCell<u64>,
    /// Owned kernel stack. `None` for threads that run on a bootstrap stack
    /// the bootloader gave us (the BSP boot thread, each AP's idle thread).
    _stack: Option<Box<[u8]>>,
    is_idle: bool,
    /// Physical PML4 base to load into CR3 for this thread (kernel PML4 for
    /// kernel threads; the process PML4 for user threads).
    cr3: u64,
    /// Top of this thread's kernel stack — programmed into TSS.RSP0 and the
    /// syscall entry (`gs:[kernel_rsp]`) when it runs, so a ring transition
    /// from user lands here. `None` = never takes a ring transition.
    kstack_top: Option<u64>,
    /// Held to keep the address space alive for the thread's lifetime.
    proc: Option<Arc<Process>>,
}

unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    fn set_state(&self, s: State) {
        *self.state.lock() = s;
    }
    fn state(&self) -> State {
        *self.state.lock()
    }
    fn ctx_ptr(&self) -> *mut u64 {
        self.ctx.get()
    }

    /// A thread that adopts the current (bootloader-provided) stack. Its `ctx`
    /// becomes valid the first time we switch away from it.
    fn adopting(id: u64, name: &'static str, is_idle: bool) -> Arc<Self> {
        Arc::new(Self {
            id,
            name,
            state: Mutex::new(State::Running),
            ctx: UnsafeCell::new(0),
            _stack: None,
            is_idle,
            cr3: vmm::kernel_pml4_phys(),
            kstack_top: None,
            proc: None,
        })
    }

    /// Lay out the initial kernel stack so the first `thos_ctx_switch` pops the
    /// six saved registers and `ret`s into `entry_asm` with r12..r15 preloaded.
    fn build_stack(entry_asm: u64, r12: u64, r13: u64, r14: u64, r15: u64) -> (Box<[u8]>, u64, u64) {
        let mut stack = vec![0u8; KSTACK_SIZE].into_boxed_slice();
        let base = stack.as_mut_ptr() as usize;
        let top = (base + KSTACK_SIZE) & !0xF;
        let mut sp = top - 8; // so the callee sees rsp % 16 == 0 after its `call`
        sp -= 7 * 8;
        unsafe {
            let s = sp as *mut u64;
            *s.add(0) = r15;
            *s.add(1) = r14;
            *s.add(2) = r13;
            *s.add(3) = r12;
            *s.add(4) = 0; // rbx
            *s.add(5) = 0; // rbp
            *s.add(6) = entry_asm; // return address
        }
        (stack, sp as u64, top as u64)
    }

    fn spawned(id: u64, name: &'static str, entry: extern "C" fn(usize) -> !, arg: usize) -> Arc<Self> {
        let (stack, sp, top) = Self::build_stack(
            thos_thread_trampoline as *const () as u64,
            entry as u64,
            arg as u64,
            0,
            0,
        );
        Arc::new(Self {
            id,
            name,
            state: Mutex::new(State::Ready),
            ctx: UnsafeCell::new(sp),
            _stack: Some(stack),
            is_idle: false,
            cr3: vmm::kernel_pml4_phys(),
            kstack_top: Some(top),
            proc: None,
        })
    }

    fn spawned_user(id: u64, name: &'static str, proc: Arc<Process>, entry: u64, user_rsp: u64) -> Arc<Self> {
        let s = gdt::selectors();
        let (stack, sp, top) = Self::build_stack(
            thos_user_thread_start as *const () as u64,
            entry,
            user_rsp,
            (s.user_code.0 | 3) as u64,
            (s.user_data.0 | 3) as u64,
        );
        let cr3 = proc.pml4_phys();
        Arc::new(Self {
            id,
            name,
            state: Mutex::new(State::Ready),
            ctx: UnsafeCell::new(sp),
            _stack: Some(stack),
            is_idle: false,
            cr3,
            kstack_top: Some(top),
            proc: Some(proc),
        })
    }
}

struct CpuSlot {
    current: Option<Arc<Thread>>,
    idle: Option<Arc<Thread>>,
}

struct Inner {
    ready: VecDeque<Arc<Thread>>,
    cpus: [CpuSlot; MAX_CPUS],
    /// Exited threads park here so their stacks are not freed while a CPU may
    /// still be unwinding off them. A real reaper frees these later.
    graveyard: alloc::vec::Vec<Arc<Thread>>,
}

static SCHED: Mutex<Inner> = Mutex::new(Inner {
    ready: VecDeque::new(),
    cpus: [const {
        CpuSlot { current: None, idle: None }
    }; MAX_CPUS],
    graveyard: alloc::vec::Vec::new(),
});

static NEXT_TID: AtomicU64 = AtomicU64::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);
static CTX_SWITCHES: AtomicU64 = AtomicU64::new(0);

pub fn ctx_switches() -> u64 {
    CTX_SWITCHES.load(Ordering::Relaxed)
}

/// The thread running on this CPU right now.
pub fn current() -> Arc<Thread> {
    let cpu = smp::this_cpu() as usize;
    SCHED
        .lock()
        .cpus[cpu]
        .current
        .clone()
        .expect("current: no current thread")
}

/// BSP: turn the current execution into thread 0 and give this CPU an idle
/// thread. Scheduling becomes active on return.
pub fn init_bsp() {
    let boot = Thread::adopting(0, "cpu0/boot", false);
    let idle = Thread::spawned(idle_tid(0), "cpu0/idle", idle_entry, 0);
    {
        let mut s = SCHED.lock();
        s.cpus[0].current = Some(boot);
        s.cpus[0].idle = Some(idle);
    }
    STARTED.store(true, Ordering::Release);
}

/// AP: adopt the current execution as this CPU's idle thread, then run the idle
/// loop. The timer will preempt it whenever there is work.
pub fn cpu_enter() -> ! {
    let cpu = smp::this_cpu() as usize;
    let me = Thread::adopting(idle_tid(cpu as u64), "ap/idle", true);
    {
        let mut s = SCHED.lock();
        s.cpus[cpu].idle = Some(me.clone());
        s.cpus[cpu].current = Some(me);
    }
    interrupts::enable();
    loop {
        interrupts::enable();
        x86_64::instructions::hlt();
    }
}

fn idle_tid(cpu: u64) -> u64 {
    0xffff_0000 | cpu
}

extern "C" fn idle_entry(_arg: usize) -> ! {
    loop {
        interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Create a runnable kernel thread.
pub fn spawn(name: &'static str, entry: extern "C" fn(usize) -> !, arg: usize) -> u64 {
    let id = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    let t = Thread::spawned(id, name, entry, arg);
    SCHED.lock().ready.push_back(t);
    id
}

/// Create a runnable *user* thread: it enters ring 3 at `entry` on `user_rsp`
/// the first time it is scheduled, in `proc`'s address space.
pub fn spawn_user(name: &'static str, proc: Arc<Process>, entry: u64, user_rsp: u64) -> u64 {
    let id = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    let t = Thread::spawned_user(id, name, proc, entry, user_rsp);
    SCHED.lock().ready.push_back(t);
    id
}

/// Point CR3 + the ring-transition stacks at the thread we're about to run.
/// Runs with interrupts disabled, right before `thos_ctx_switch`.
fn apply_cpu_state(cpu: usize, cr3: u64, kstack_top: Option<u64>) {
    let cur = Cr3::read().0.start_address().as_u64();
    if cr3 != cur {
        let frame = PhysFrame::from_start_address(PhysAddr::new(cr3)).expect("cr3 aligned");
        unsafe { Cr3::write(frame, Cr3Flags::empty()) };
    }
    if let Some(top) = kstack_top {
        gdt::set_kernel_stack(cpu, VirtAddr::new(top));
        smp::set_kernel_rsp(cpu, top);
    }
}

/// Voluntarily give up the CPU.
pub fn yield_now() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    interrupts::without_interrupts(|| reschedule(false));
}

/// Timer-tick preemption hook. Called from the APIC timer handler after EOI.
pub fn on_tick() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    // Already in interrupt context (IF=0); switch directly.
    reschedule(false);
}

/// Block the current thread. The caller must already hold a reference to this
/// thread somewhere it can be found again (a wait queue).
pub fn block_current() {
    interrupts::without_interrupts(|| reschedule(true));
}

/// Make a previously-blocked thread runnable again.
pub fn unblock(t: Arc<Thread>) {
    t.set_state(State::Ready);
    SCHED.lock().ready.push_back(t);
}

/// Terminate the current thread. Never returns.
pub fn exit() -> ! {
    interrupts::disable();
    let (load, cpu, cr3, kstack_top) = {
        let mut s = SCHED.lock();
        let cpu = smp::this_cpu() as usize;
        let prev = s.cpus[cpu].current.take().expect("exit: no current thread");
        prev.set_state(State::Exited);
        let next = pick_next(&mut s, cpu);
        next.set_state(State::Running);
        let out = (next.ctx_ptr(), cpu, next.cr3, next.kstack_top);
        s.cpus[cpu].current = Some(next);
        s.graveyard.push(prev); // keep the stack alive
        out
    };
    apply_cpu_state(cpu, cr3, kstack_top);
    let mut scratch = 0u64;
    unsafe { thos_ctx_switch(&mut scratch, load) };
    unreachable!("switched back into an exited thread")
}

fn pick_next(s: &mut Inner, cpu: usize) -> Arc<Thread> {
    s.ready
        .pop_front()
        .unwrap_or_else(|| s.cpus[cpu].idle.clone().expect("cpu has no idle thread"))
}

/// The core switch. `block` = don't return the current thread to the ready
/// queue (it is going to sleep). Must run with interrupts disabled.
fn reschedule(block: bool) {
    let (save, load, cpu, cr3, kstack_top) = {
        let mut s = SCHED.lock();
        let cpu = smp::this_cpu() as usize;

        let prev = s.cpus[cpu].current.clone().expect("reschedule: no current thread");
        let next = pick_next(&mut s, cpu);

        if Arc::ptr_eq(&prev, &next) {
            // Only reachable when `prev` is this CPU's idle thread and the ready
            // queue was empty. Nothing was mutated — just stay put.
            return;
        }

        // Commit only now that we know a real switch is happening, so `prev` is
        // never both `current` and queued at the same time.
        if block {
            prev.set_state(State::Blocked);
        } else if prev.state() == State::Running && !prev.is_idle {
            prev.set_state(State::Ready);
            s.ready.push_back(prev.clone());
        }
        next.set_state(State::Running);
        let out = (prev.ctx_ptr(), next.ctx_ptr(), cpu, next.cr3, next.kstack_top);
        s.cpus[cpu].current = Some(next.clone());
        out
        // `prev` / `next` locals drop here. `next` stays alive via
        // `cpus[cpu].current`; `prev` stays alive via the ready queue (yield),
        // a wait queue (block), or the graveyard (exit, handled separately).
    };

    apply_cpu_state(cpu, cr3, kstack_top);
    CTX_SWITCHES.fetch_add(1, Ordering::Relaxed);
    unsafe { thos_ctx_switch(save, load) };
}
