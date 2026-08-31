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
use crate::process::Task;
use crate::syscall::{thos_user_thread_resume, UserFrame};
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
    call thos_finish_switch        // release the thread that yielded to us
    mov rdi, r13
    call r12
    call thos_thread_exit

.globl thos_user_thread_start
// entered via `ret` from thos_ctx_switch with
//   r12=user rip, r13=user rsp, r14=user cs, r15=user ss
thos_user_thread_start:
    call thos_finish_switch        // release the thread that yielded to us
    push r15
    push r13
    push 0x202          // RFLAGS with IF=1 -> the user thread is preemptible
    push r14
    push r12
    swapgs
    iretq

// Same, but IF=0: the thread is *not* timer-preemptible in user mode. Used for
// PE threads so the ring-3 `swapgs` discipline stays trivial (a preempted PE
// thread would need a ring-3 IRQ swapgs shim — a later item).
.globl thos_user_thread_start_coop
thos_user_thread_start_coop:
    call thos_finish_switch
    push r15
    push r13
    push 0x002          // RFLAGS with IF=0
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
    fn thos_user_thread_start_coop() -> !;
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
    cr3: AtomicU64,
    /// Top of this thread's kernel stack — programmed into TSS.RSP0 and the
    /// syscall entry (`gs:[kernel_rsp]`) when it runs, so a ring transition
    /// from user lands here. `None` = never takes a ring transition.
    kstack_top: Option<u64>,
    /// Held to keep the task (and its address space) alive for the thread's
    /// lifetime.
    task: Option<Arc<Task>>,
    /// Owned resume frame for a fork child (kept alive; `r12` points at it).
    _uframe: Option<Box<UserFrame>>,
    /// `true` while some CPU is executing on this thread's kernel stack. A CPU
    /// about to resume this thread spins on this first, so a thread that
    /// yielded from a syscall on CPU A is never re-entered on CPU B before A
    /// has finished unwinding off its stack.
    running: AtomicBool,
    /// User `%fs` base (TLS pointer). Thread-private CPU state that must be
    /// reloaded on every switch into this thread — musl dereferences `%fs`
    /// constantly. 0 for kernel threads and for a fresh image before its
    /// first `arch_prctl(SET_FS)`.
    fsbase: AtomicU64,
    /// User `%gs` base (the Win64 TEB pointer for PE threads). `0` = use the
    /// per-CPU pointer, i.e. behave exactly as a POSIX/kernel thread. Loaded
    /// into `IA32_KERNEL_GS_BASE` on every switch in, so the exit `swapgs`
    /// brings it live for ring 3.
    gsbase: AtomicU64,
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
            cr3: AtomicU64::new(vmm::kernel_pml4_phys()),
            kstack_top: None,
            task: None,
            _uframe: None,
            running: AtomicBool::new(true), // it is running right now
            fsbase: AtomicU64::new(0),
            gsbase: AtomicU64::new(0),
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
            cr3: AtomicU64::new(vmm::kernel_pml4_phys()),
            kstack_top: Some(top),
            task: None,
            _uframe: None,
            running: AtomicBool::new(false),
            fsbase: AtomicU64::new(0),
            gsbase: AtomicU64::new(0),
        })
    }

    fn spawned_user(
        id: u64,
        name: &'static str,
        task: Arc<Task>,
        entry: u64,
        user_rsp: u64,
        cooperative: bool,
        gsbase: u64,
    ) -> Arc<Self> {
        let s = gdt::selectors();
        let cr3 = task.space().pml4_phys();
        let trampoline = if cooperative {
            thos_user_thread_start_coop as *const () as u64
        } else {
            thos_user_thread_start as *const () as u64
        };
        let (stack, sp, top) = Self::build_stack(
            trampoline,
            entry,
            user_rsp,
            (s.user_code.0 | 3) as u64,
            (s.user_data.0 | 3) as u64,
        );
        Arc::new(Self {
            id,
            name,
            state: Mutex::new(State::Ready),
            ctx: UnsafeCell::new(sp),
            _stack: Some(stack),
            is_idle: false,
            cr3: AtomicU64::new(cr3),
            kstack_top: Some(top),
            task: Some(task),
            _uframe: None,
            running: AtomicBool::new(false),
            fsbase: AtomicU64::new(0),
            gsbase: AtomicU64::new(gsbase),
        })
    }

    fn spawned_user_frame(
        id: u64,
        name: &'static str,
        task: Arc<Task>,
        frame: UserFrame,
        fsbase: u64,
    ) -> Arc<Self> {
        let uframe = Box::new(frame);
        let fptr = &*uframe as *const UserFrame as u64;
        let cr3 = task.space().pml4_phys();
        let (stack, sp, top) = Self::build_stack(
            thos_user_thread_resume as *const () as u64,
            fptr, // r12 = &UserFrame
            0,
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
            cr3: AtomicU64::new(cr3),
            kstack_top: Some(top),
            task: Some(task),
            _uframe: Some(uframe),
            running: AtomicBool::new(false),
            fsbase: AtomicU64::new(fsbase),
            gsbase: AtomicU64::new(0),
        })
    }

    fn cr3(&self) -> u64 {
        self.cr3.load(Ordering::Relaxed)
    }
}

struct CpuSlot {
    current: Option<Arc<Thread>>,
    idle: Option<Arc<Thread>>,
    /// The thread this CPU just switched away from, waiting for the thread we
    /// switched *into* to run `thos_finish_switch` and release it. `bool` =
    /// it was blocking (do not return it to the ready queue).
    handoff: Option<(Arc<Thread>, bool)>,
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
        CpuSlot { current: None, idle: None, handoff: None }
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

impl Thread {
    /// The task (process-tree node) this thread belongs to, if it is a user
    /// thread.
    pub fn task(&self) -> Option<Arc<Task>> {
        self.task.clone()
    }
    /// Repoint this thread's CR3 (used by `execve` when it swaps the address
    /// space).
    pub fn set_cr3(&self, v: u64) {
        self.cr3.store(v, Ordering::Relaxed);
    }
    /// This thread's saved user `%fs` base.
    pub fn fsbase(&self) -> u64 {
        self.fsbase.load(Ordering::Relaxed)
    }
    /// Record this thread's user `%fs` base (from `arch_prctl(SET_FS)` / execve).
    pub fn set_fsbase(&self, v: u64) {
        self.fsbase.store(v, Ordering::Relaxed);
    }
    /// This thread's user `%gs` base (Win64 TEB pointer, `0` for non-PE).
    pub fn gsbase(&self) -> u64 {
        self.gsbase.load(Ordering::Relaxed)
    }
}

/// The address space of the thread running on this CPU, if it is a user thread.
pub fn current_proc() -> Option<Arc<crate::process::Process>> {
    current().task().map(|t| t.space())
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
pub fn spawn_user(name: &'static str, task: Arc<Task>, entry: u64, user_rsp: u64) -> u64 {
    let id = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    let t = Thread::spawned_user(id, name, task, entry, user_rsp, false, 0);
    SCHED.lock().ready.push_back(t);
    id
}

/// A user thread for a native PE image: cooperatively scheduled (IF=0 in ring 3)
/// and carrying `teb` as its `%gs` base.
pub fn spawn_user_pe(name: &'static str, task: Arc<Task>, entry: u64, user_rsp: u64, teb: u64) -> u64 {
    let id = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    let t = Thread::spawned_user(id, name, task, entry, user_rsp, true, teb);
    SCHED.lock().ready.push_back(t);
    id
}

/// Create a runnable user thread that resumes from a full [`UserFrame`] (a
/// fork child).
pub fn spawn_user_frame(name: &'static str, task: Arc<Task>, frame: UserFrame, fsbase: u64) -> u64 {
    let id = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    let t = Thread::spawned_user_frame(id, name, task, frame, fsbase);
    SCHED.lock().ready.push_back(t);
    id
}

/// Point CR3 + the ring-transition stacks at the thread we're about to run.
/// Runs with interrupts disabled, right before `thos_ctx_switch`.
fn apply_cpu_state(cpu: usize, cr3: u64, kstack_top: Option<u64>, fsbase: u64, gsbase: u64) {
    use x86_64::registers::model_specific::{FsBase, KernelGsBase};
    let cur = Cr3::read().0.start_address().as_u64();
    if cr3 != cur {
        let frame = PhysFrame::from_start_address(PhysAddr::new(cr3)).expect("cr3 aligned");
        unsafe { Cr3::write(frame, Cr3Flags::empty()) };
    }
    // Reload the user TLS base for the thread we're switching to (0 for kernel
    // threads). Cheap wrmsr; without it a forked musl child runs on the
    // parent's — or a stale — `%fs` and faults on its first TLS access.
    FsBase::write(VirtAddr::new(fsbase));
    // The thread's user `%gs` base goes into KERNEL_GS_BASE; the exit `swapgs`
    // makes it live for ring 3. `0` = the per-CPU pointer, so POSIX/kernel
    // threads keep `gs` == per-CPU. Skip the `wrmsr` unless the value actually
    // changes — the common POSIX→POSIX switch then costs nothing (and behaves
    // exactly as before PE support).
    let want = if gsbase != 0 { gsbase } else { smp::this_cpu_ptr() };
    if smp::user_gs(cpu) != want {
        KernelGsBase::write(VirtAddr::new(want));
        smp::set_user_gs(cpu, want);
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
    let (load, next, cpu, cr3, kstack_top, fsbase, gsbase) = {
        let mut s = SCHED.lock();
        let cpu = smp::this_cpu() as usize;
        let prev = s.cpus[cpu].current.take().expect("exit: no current thread");
        prev.set_state(State::Exited);
        let next = pick_next(&mut s, cpu);
        next.set_state(State::Running);
        let out = (
            next.ctx_ptr(),
            next.clone(),
            cpu,
            next.cr3(),
            next.kstack_top,
            next.fsbase(),
            next.gsbase(),
        );
        s.cpus[cpu].current = Some(next);
        // Hand the corpse off like a blocking switch: `finish_switch` (running
        // in `next`) clears its `running` flag once this CPU is fully off its
        // stack, which is exactly when `reap()` may free it.
        s.cpus[cpu].handoff = Some((prev.clone(), true));
        s.graveyard.push(prev);
        out
    };
    apply_cpu_state(cpu, cr3, kstack_top, fsbase, gsbase);
    while next.running.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }
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
    let (save, load, next, cpu, cr3, kstack_top, fsbase, gsbase) = {
        let mut s = SCHED.lock();
        let cpu = smp::this_cpu() as usize;

        let prev = s.cpus[cpu].current.clone().expect("reschedule: no current thread");
        let next = pick_next(&mut s, cpu);

        if Arc::ptr_eq(&prev, &next) {
            // Only reachable when `prev` is this CPU's idle thread and the ready
            // queue was empty. Nothing was mutated — just stay put.
            return;
        }

        // Commit only now that we know a real switch is happening.
        if block {
            prev.set_state(State::Blocked);
        } else if prev.state() == State::Running && !prev.is_idle {
            prev.set_state(State::Ready);
        }
        next.set_state(State::Running);

        // Defer requeueing `prev` (and clearing its `running` claim) until the
        // thread we switch into runs `thos_finish_switch` — by then this CPU is
        // fully off `prev`'s kernel stack, so no other CPU can resume `prev`
        // mid-unwind.
        s.cpus[cpu].handoff = Some((prev.clone(), block));
        let out = (
            prev.ctx_ptr(),
            next.ctx_ptr(),
            next.clone(),
            cpu,
            next.cr3(),
            next.kstack_top,
            next.fsbase(),
            next.gsbase(),
        );
        s.cpus[cpu].current = Some(next.clone());
        out
    };

    apply_cpu_state(cpu, cr3, kstack_top, fsbase, gsbase);
    CTX_SWITCHES.fetch_add(1, Ordering::Relaxed);

    // Claim `next`'s stack: wait out any CPU still unwinding off it.
    while next.running.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }

    unsafe { thos_ctx_switch(save, load) };

    // Back in a freshly-resumed thread's context on this CPU: release whatever
    // thread this CPU last switched away from.
    finish_switch();
}

/// Release the thread this CPU handed off in its last context switch: clear its
/// `running` claim and, unless it was blocking, return it to the ready queue.
/// Called right after `thos_ctx_switch` and at the top of every thread
/// trampoline (a fresh thread's first run does not return through `reschedule`).
#[no_mangle]
extern "C" fn thos_finish_switch() {
    finish_switch();
}

fn finish_switch() {
    let mut s = SCHED.lock();
    let cpu = smp::this_cpu() as usize;
    let Some((prev, was_blocking)) = s.cpus[cpu].handoff.take() else {
        return;
    };
    if !was_blocking && prev.state() == State::Ready && !prev.is_idle {
        s.ready.push_back(prev.clone());
    }
    prev.running.store(false, Ordering::Release);
}

/// Free the kernel stacks of exited threads. Safe to call from anywhere: a
/// corpse is only dropped once no CPU is on its stack (`running` cleared by
/// `finish_switch`) and nothing else still holds a reference.
#[allow(dead_code)] // driven by the `stress` milestone today; a reaper thread later
pub fn reap() {
    let mut s = SCHED.lock();
    let mut i = 0;
    while i < s.graveyard.len() {
        let dead =
            !s.graveyard[i].running.load(Ordering::Acquire) && Arc::strong_count(&s.graveyard[i]) == 1;
        if dead {
            s.graveyard.swap_remove(i); // Arc drops -> the Box<[u8]> stack is freed
        } else {
            i += 1;
        }
    }
}
