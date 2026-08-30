// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 (prep) — THOS's own page tables.
//!
//! Until now we ran on Limine's tables. This builds a fresh top-level page
//! table that THOS owns and every CPU loads:
//!   * kernel image, mapped **per section** with W^X (text RX, rodata R, data RW),
//!   * the higher-half direct map (HHDM) of all physical RAM, 1 GiB pages,
//!   * an identity map of the low 4 GiB so low MMIO (LAPIC/IO APIC) and any
//!     bootstrap pointer still resolves.
//!
//! User address space / per-process `vmspace` objects come with the process
//! model in Phase 2/3; this is the shared kernel half they all inherit.

use limine::memmap::Entry;
use spin::Once;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::registers::model_specific::{Efer, EferFlags};
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageTable, PageTableFlags as F, PhysFrame, Size1GiB, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::mm::{self, FRAME_ALLOC};

const GIB: u64 = 1 << 30;
const HHDM_MAX_GIB: u64 = 512;

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
}

fn sym(s: &'static u8) -> u64 {
    core::ptr::addr_of!(*s) as u64
}

static KERNEL_PML4: Once<PhysFrame> = Once::new();

/// Build the kernel page tables and switch this (BSP) CPU onto them.
pub fn init(hhdm: u64, entries: &[&Entry], kernel_phys_base: u64, kernel_virt_base: u64) {
    unsafe { Efer::update(|e| e.insert(EferFlags::NO_EXECUTE_ENABLE)) };

    let pml4_frame = FRAME_ALLOC.lock().alloc().expect("no frame for PML4");
    let pml4: &mut PageTable = unsafe {
        let p = mm::phys_to_virt(pml4_frame.start_address()).as_mut_ptr::<PageTable>();
        p.write(PageTable::new());
        &mut *p
    };
    let mut m = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(hhdm)) };

    // Identity-map the low 4 GiB (Limine's identity region + low MMIO).
    for i in 0..4 {
        map_1g(
            &mut m,
            VirtAddr::new(i * GIB),
            PhysAddr::new(i * GIB),
            F::PRESENT | F::WRITABLE | F::NO_EXECUTE,
        );
    }

    // HHDM covering all physical RAM.
    let mut max_phys = 0u64;
    for e in entries {
        max_phys = max_phys.max(e.base + e.length);
    }
    let n_gib = ((max_phys + GIB - 1) / GIB).clamp(4, HHDM_MAX_GIB);
    for i in 0..n_gib {
        map_1g(
            &mut m,
            VirtAddr::new(hhdm + i * GIB),
            PhysAddr::new(i * GIB),
            F::PRESENT | F::WRITABLE | F::NO_EXECUTE,
        );
    }

    // Kernel image, per section, W^X.
    let phys_of = |v: u64| kernel_phys_base + (v - kernel_virt_base);
    unsafe {
        map_range_4k(&mut m, sym(&__text_start), sym(&__text_end), &phys_of, F::PRESENT);
        map_range_4k(
            &mut m,
            sym(&__rodata_start),
            sym(&__rodata_end),
            &phys_of,
            F::PRESENT | F::NO_EXECUTE,
        );
        map_range_4k(
            &mut m,
            sym(&__data_start),
            sym(&__data_end),
            &phys_of,
            F::PRESENT | F::WRITABLE | F::NO_EXECUTE,
        );
    }

    KERNEL_PML4.call_once(|| pml4_frame);
    unsafe { activate() };
}

/// Load the kernel page tables on the current CPU. Safe to call once `init`
/// has run (APs call this from their bring-up path).
pub unsafe fn activate() {
    let frame = *KERNEL_PML4.get().expect("vmm::init not called yet");
    Cr3::write(frame, Cr3Flags::empty());
}

/// Map a single 4 KiB page into the (active) kernel page tables and flush it.
/// Used for the ring-3 self-test; the real process model gets per-process
/// `vmspace` objects.
pub fn map_page(virt: u64, phys: u64, writable: bool, user: bool, exec: bool) {
    let hhdm = crate::mm::hhdm_offset();
    let pml4_frame = *KERNEL_PML4.get().expect("vmm::init not called yet");
    let pml4: &mut PageTable = unsafe {
        &mut *crate::mm::phys_to_virt(pml4_frame.start_address()).as_mut_ptr::<PageTable>()
    };
    let mut m = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(hhdm)) };

    let mut f = F::PRESENT;
    if writable {
        f |= F::WRITABLE;
    }
    if user {
        f |= F::USER_ACCESSIBLE;
    }
    if !exec {
        f |= F::NO_EXECUTE;
    }

    // Parent tables need USER_ACCESSIBLE too, or a ring-3 walk faults.
    let parent = if user {
        F::PRESENT | F::WRITABLE | F::USER_ACCESSIBLE
    } else {
        F::PRESENT | F::WRITABLE
    };

    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt));
    let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys));
    let mut fa = FRAME_ALLOC.lock();
    unsafe { m.map_to_with_table_flags(page, frame, f, parent, &mut *fa) }
        .expect("map_page")
        .flush();
}

fn map_1g(m: &mut OffsetPageTable<'_>, v: VirtAddr, p: PhysAddr, f: F) {
    let page = Page::<Size1GiB>::containing_address(v);
    let frame = PhysFrame::<Size1GiB>::containing_address(p);
    let mut fa = FRAME_ALLOC.lock();
    unsafe { m.map_to(page, frame, f, &mut *fa) }
        .expect("map_1g")
        .ignore();
}

fn map_range_4k(
    m: &mut OffsetPageTable<'_>,
    start: u64,
    end: u64,
    phys_of: &dyn Fn(u64) -> u64,
    f: F,
) {
    let mut v = start & !0xFFF;
    while v < end {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(v));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys_of(v)));
        let mut fa = FRAME_ALLOC.lock();
        unsafe { m.map_to(page, frame, f, &mut *fa) }
            .expect("map_range_4k")
            .ignore();
        v += 4096;
    }
}

/// GiB of HHDM actually installed — for the milestone print.
pub fn hhdm_gib(entries: &[&Entry]) -> u64 {
    let mut max_phys = 0u64;
    for e in entries {
        max_phys = max_phys.max(e.base + e.length);
    }
    ((max_phys + GIB - 1) / GIB).clamp(4, HHDM_MAX_GIB)
}
