// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — GDT + TSS.
//!
//! A fresh flat GDT (64-bit kernel code + data) plus one TSS whose IST slots
//! give the nastiest faults their own known-good stacks:
//!   * IST0 — #DF double fault (a bad kernel stack must not turn #DF into a
//!     triple fault / reboot)
//!   * IST1 — NMI
//!   * IST2 — #PF page fault (so a stack-overflow page fault is still handled)
//!
//! Per-CPU GDT/TSS come with SMP bring-up; this is the BSP's.

use spin::Lazy;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const NMI_IST_INDEX: u16 = 1;
pub const PAGE_FAULT_IST_INDEX: u16 = 2;

/// 20 KiB per emergency stack. Not huge, but these handlers only log and halt.
const IST_STACK_SIZE: usize = 4096 * 5;

fn ist_stack() -> VirtAddr {
    // One private static arena per call site. `#[used]` static mut, addr-of only.
    static mut STACKS: [[u8; IST_STACK_SIZE]; 3] = [[0; IST_STACK_SIZE]; 3];
    static NEXT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let i = NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let base = unsafe { core::ptr::addr_of_mut!(STACKS[i]) } as u64;
    // x86 stacks grow down: hand out the top.
    VirtAddr::new(base + IST_STACK_SIZE as u64)
}

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = ist_stack();
    tss.interrupt_stack_table[NMI_IST_INDEX as usize] = ist_stack();
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = ist_stack();
    tss
});

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let tss = gdt.append(Descriptor::tss_segment(&TSS));
    (gdt, Selectors { code, data, tss })
});

pub fn init() {
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        SS::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
}
