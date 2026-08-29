// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — per-CPU GDT + TSS.
//!
//! Each logical CPU gets its own GDT and its own TSS: a fault on one CPU must
//! not land on another CPU's IST stack. IST slots:
//!   * IST0 — #DF double fault
//!   * IST1 — NMI
//!   * IST2 — #PF page fault
//!
//! Layout is identical on every CPU, so the segment selectors are shared.

use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
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
        let code = (*gdt).append(Descriptor::kernel_code_segment());
        let data = (*gdt).append(Descriptor::kernel_data_segment());
        let tss_sel = (*gdt).append(Descriptor::tss_segment(&*tss));

        (*gdt).load_unsafe();
        CS::set_reg(code);
        DS::set_reg(data);
        ES::set_reg(data);
        SS::set_reg(data);
        load_tss(tss_sel);
    }
}
