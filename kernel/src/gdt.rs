// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — per-CPU GDT + TSS.
//!
//! Each logical CPU gets its own GDT and its own TSS: a fault on one CPU must
//! not land on another CPU's IST stack. IST slots:
//!   * IST0 — #DF double fault
//!   * IST1 — NMI
//!   * IST2 — #PF page fault
//!
//! Descriptor order is fixed so `SYSCALL`/`SYSRETQ` work:
//!   1 kernel code · 2 kernel data · 3 user data · 4 user code · 5/6 TSS
//! Layout is identical on every CPU, so the selectors are shared.

use spin::Once;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const MAX_CPUS: usize = 32;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const NMI_IST_INDEX: u16 = 1;
pub const PAGE_FAULT_IST_INDEX: u16 = 2;

const IST_STACK_SIZE: usize = 4096 * 5;
const IST_SLOTS: [u16; 3] = [DOUBLE_FAULT_IST_INDEX, NMI_IST_INDEX, PAGE_FAULT_IST_INDEX];

#[repr(align(16))]
struct IstStacks([[u8; IST_STACK_SIZE]; 3]);

static mut GDT: [GlobalDescriptorTable; MAX_CPUS] =
    [const { GlobalDescriptorTable::new() }; MAX_CPUS];
static mut TSS: [TaskStateSegment; MAX_CPUS] = [const { TaskStateSegment::new() }; MAX_CPUS];
static mut IST_STACKS: [IstStacks; MAX_CPUS] =
    [const { IstStacks([[0; IST_STACK_SIZE]; 3]) }; MAX_CPUS];

#[derive(Clone, Copy)]
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_data: SegmentSelector,
    pub user_code: SegmentSelector,
}

static SELECTORS: Once<Selectors> = Once::new();

/// The shared segment selectors (identical on every CPU). Valid after the BSP's
/// `init(0)`.
pub fn selectors() -> Selectors {
    *SELECTORS.get().expect("gdt::init not called")
}

/// Build, load, and activate this CPU's GDT + TSS. `cpu` is a dense index
/// (0 = BSP). Call once per CPU, early, with interrupts disabled.
pub fn init(cpu: usize) {
    assert!(cpu < MAX_CPUS, "cpu index out of range");
    unsafe {
        let tss = core::ptr::addr_of_mut!(TSS[cpu]);
        for (i, slot) in IST_SLOTS.iter().enumerate() {
            let stack = core::ptr::addr_of!(IST_STACKS[cpu].0[i]);
            let top = stack as u64 + IST_STACK_SIZE as u64;
            (*tss).interrupt_stack_table[*slot as usize] = VirtAddr::new(top);
        }

        let gdt = core::ptr::addr_of_mut!(GDT[cpu]);
        let kernel_code = (*gdt).append(Descriptor::kernel_code_segment());
        let kernel_data = (*gdt).append(Descriptor::kernel_data_segment());
        let user_data = (*gdt).append(Descriptor::user_data_segment());
        let user_code = (*gdt).append(Descriptor::user_code_segment());
        let tss_sel = (*gdt).append(Descriptor::tss_segment(&*tss));

        (*gdt).load_unsafe();
        CS::set_reg(kernel_code);
        DS::set_reg(kernel_data);
        ES::set_reg(kernel_data);
        SS::set_reg(kernel_data);
        load_tss(tss_sel);

        SELECTORS.call_once(|| Selectors {
            kernel_code,
            kernel_data,
            user_data,
            user_code,
        });
    }
}

/// Set this CPU's TSS ring-0 stack pointer (`RSP0`), used when an interrupt or
/// `int` is taken while in ring 3.
pub fn set_kernel_stack(cpu: usize, rsp0: VirtAddr) {
    assert!(cpu < MAX_CPUS);
    unsafe {
        (*core::ptr::addr_of_mut!(TSS[cpu])).privilege_stack_table[0] = rsp0;
    }
}
