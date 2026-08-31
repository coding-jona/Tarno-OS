// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — SMP bring-up.
//!
//! We let Limine start the APs: they hand over **already in 64-bit long mode**
//! with the same page tables as the BSP (HHDM mapped), so there is no real-mode
//! trampoline / INIT-SIPI-SIPI here. Each AP then does its own CPU-local setup
//! (GDT+TSS, shared IDT, Local APIC, GS base) and parks.
//!
//! A hand-rolled INIT-SIPI-SIPI path replaces this once THOS owns its own boot
//! path and page tables (post-Limine); see docs/thos/architecture.md.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use limine::mp::MpInfo;
use limine::request::MpResponse;
use x86_64::registers::model_specific::{GsBase, KernelGsBase};
use x86_64::VirtAddr;

use crate::gdt::MAX_CPUS;
use crate::{apic, gdt, idt, kprintln};

/// CPU-local block. Reachable via the GS base once set; `self_ptr` at offset 0
/// lets `gs:[0]` recover the pointer. Field offsets 16/24 are referenced by the
/// `syscall` entry stub — keep them in sync with `syscall::PERCPU_*`.
#[repr(C)]
pub struct PerCpu {
    pub self_ptr: usize,   // 0
    pub cpu_index: u32,    // 8
    pub lapic_id: u32,     // 12
    pub kernel_rsp: u64,   // 16 — kernel stack loaded by `syscall`
    pub user_scratch: u64, // 24 — user rsp stashed across `syscall`
    /// The value currently programmed into `IA32_KERNEL_GS_BASE` for this CPU
    /// (= the ring-3 `%gs` base after the exit `swapgs`). The per-CPU pointer
    /// for POSIX/kernel threads; a PE thread's TEB while one is scheduled. Lets
    /// `apply_cpu_state` skip the `wrmsr` on the common POSIX→POSIX switch.
    pub user_gs: u64, // 32
}

static mut PER_CPU: [PerCpu; MAX_CPUS] = [const {
    PerCpu {
        self_ptr: 0,
        cpu_index: 0,
        lapic_id: 0,
        kernel_rsp: 0,
        user_scratch: 0,
        user_gs: 0,
    }
}; MAX_CPUS];

/// `IA32_KERNEL_GS_BASE` currently programmed for `cpu`.
pub fn user_gs(cpu: usize) -> u64 {
    assert!(cpu < MAX_CPUS);
    unsafe { (*core::ptr::addr_of!(PER_CPU[cpu])).user_gs }
}
pub fn set_user_gs(cpu: usize, v: u64) {
    assert!(cpu < MAX_CPUS);
    unsafe { (*core::ptr::addr_of_mut!(PER_CPU[cpu])).user_gs = v }
}

/// Set the kernel stack the `syscall` entry stub loads (`gs:[kernel_rsp]`).
/// The scheduler points this at the running user thread's kernel stack.
pub fn set_kernel_rsp(cpu: usize, rsp: u64) {
    assert!(cpu < MAX_CPUS);
    unsafe { (*core::ptr::addr_of_mut!(PER_CPU[cpu])).kernel_rsp = rsp }
}

static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);
static CPUS_ONLINE: AtomicUsize = AtomicUsize::new(0);
/// Next dense CPU index to hand out to an AP (0 is the BSP).
static NEXT_INDEX: AtomicU32 = AtomicU32::new(1);

pub fn cpu_count() -> usize {
    CPU_COUNT.load(Ordering::Relaxed)
}

/// This CPU's dense index. Reads `gs:0` (`PerCpu::self_ptr`), so it is only
/// valid after this CPU has run `install_percpu` (BSP: `smp::init`; AP:
/// `ap_entry`).
pub fn this_cpu() -> u32 {
    unsafe {
        let pc: *const PerCpu;
        core::arch::asm!("mov {}, gs:0", out(reg) pc, options(nostack, preserves_flags));
        (*pc).cpu_index
    }
}

/// This CPU's per-CPU block pointer (`gs:0`). Only read while `gs` is the
/// kernel per-CPU base (kernel context) — used to fill `KERNEL_GS_BASE` for a
/// POSIX/kernel thread on a context switch.
pub fn this_cpu_ptr() -> u64 {
    unsafe {
        let p: u64;
        core::arch::asm!("mov {}, gs:0", out(reg) p, options(nostack, preserves_flags));
        p
    }
}

pub fn online() -> usize {
    CPUS_ONLINE.load(Ordering::Relaxed)
}

/// Wire up a CPU's `PerCpu` and point GS at it.
///
/// # Safety
/// `index` must be unique and `< MAX_CPUS`; call once per CPU.
unsafe fn install_percpu(index: u32, lapic_id: u32) {
    let pc = core::ptr::addr_of_mut!(PER_CPU[index as usize]);
    (*pc).self_ptr = pc as usize;
    (*pc).cpu_index = index;
    (*pc).lapic_id = lapic_id;
    (*pc).user_gs = pc as u64;
    // Both bases point at the per-CPU block, so the `swapgs` in the syscall
    // entry stub is a no-op value-wise until a PE thread installs its TEB.
    GsBase::write(VirtAddr::new(pc as u64));
    KernelGsBase::write(VirtAddr::new(pc as u64));
}

/// Finish BSP-side CPU-local setup (the BSP's GDT/IDT/APIC were already brought
/// up in `kmain`), then start every application processor.
pub fn init(mp: &MpResponse) {
    let cpus = mp.cpus();
    CPU_COUNT.store(cpus.len(), Ordering::Relaxed);

    unsafe { install_percpu(0, mp.bsp_lapic_id) };
    CPUS_ONLINE.fetch_add(1, Ordering::Relaxed);

    for cpu in cpus {
        if cpu.lapic_id == mp.bsp_lapic_id {
            continue;
        }
        cpu.bootstrap(ap_entry, 0);
    }

    // Wait for every AP to report in.
    while CPUS_ONLINE.load(Ordering::Acquire) < cpus.len() {
        core::hint::spin_loop();
    }

    kprintln!(
        "THOS: SMP              {}/{} CPUs online",
        online(),
        cpu_count()
    );
}

/// Limine AP entry point. Runs on the AP's own stack, in long mode.
unsafe extern "C" fn ap_entry(_info: &MpInfo) -> ! {
    // Move off Limine's page tables onto the shared kernel tables first.
    crate::vmm::activate();
    crate::cpu::enable_sse();

    let index = NEXT_INDEX.fetch_add(1, Ordering::Relaxed);
    assert!((index as usize) < MAX_CPUS, "more CPUs than MAX_CPUS");

    gdt::init(index as usize);
    idt::init(); // shared IDT, just `lidt`
    let lapic_id = apic::enable_this_cpu();
    install_percpu(index, lapic_id);
    crate::syscall::init_cpu(index as usize);
    apic::start_periodic_timer();

    kprintln!("THOS: AP {} online     lapic {}", index, lapic_id);
    CPUS_ONLINE.fetch_add(1, Ordering::Release);

    // Enter the scheduler as this CPU's idle thread; the timer preempts us
    // into real work whenever the ready queue is non-empty.
    crate::sched::cpu_enter();
}
