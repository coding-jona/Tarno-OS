// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — the process / address-space object.
//!
//! A `Process` owns a private top-level page table: a full copy of the kernel
//! PML4 (so the kernel half + HHDM + identity map are shared, by pointer, with
//! every process) plus its own user-half entries. A user thread carries the
//! physical base of its process's PML4; the scheduler loads it into CR3 on the
//! switch.
//!
//! No `fork` sharing / COW yet, no address-space teardown (a reaper frees the
//! frames later) — this is here to give ELF programs real isolation.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::PhysAddr;

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::vmm;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

#[allow(dead_code)] // pid consumed by SYS_GETPID soon
pub struct Process {
    pub pid: u64,
    pml4_phys: u64,
    /// Next free user virtual address for ad-hoc allocations (stacks, brk).
    next_user_va: AtomicU64,
}

/// User virtual space we hand out for stacks etc., well clear of typical ELF
/// load addresses.
const USER_ALLOC_BASE: u64 = 0x0000_7000_0000_0000;
const USER_STACK_SIZE: u64 = 64 * 1024;

impl Process {
    pub fn new() -> Arc<Self> {
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame for process PML4");
        let pml4_phys = frame.start_address().as_u64();

        // Copy every entry of the kernel PML4: kernel-half + HHDM + identity are
        // then shared with this process; the user half starts empty.
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(PhysAddr::new(vmm::kernel_pml4_phys())).as_ptr::<u8>(),
                phys_to_virt(PhysAddr::new(pml4_phys)).as_mut_ptr::<u8>(),
                4096,
            );
        }

        Arc::new(Self {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            pml4_phys,
            next_user_va: AtomicU64::new(USER_ALLOC_BASE),
        })
    }

    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    /// Map one 4 KiB user page into this address space.
    pub fn map(&self, virt: u64, phys: u64, writable: bool, exec: bool) {
        vmm::map_page_in(self.pml4_phys, virt, phys, writable, true, exec);
    }

    /// Allocate + map a fresh user stack; returns the (page-aligned) stack top.
    pub fn new_user_stack(&self) -> u64 {
        let base = self.next_user_va.fetch_add(USER_STACK_SIZE + 0x1000, Ordering::Relaxed);
        let pages = USER_STACK_SIZE / 4096;
        for i in 0..pages {
            let frame = FRAME_ALLOC.lock().alloc().expect("no frame for user stack");
            unsafe {
                core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
            }
            self.map(base + i * 4096, frame.start_address().as_u64(), true, false);
        }
        base + USER_STACK_SIZE
    }
}
