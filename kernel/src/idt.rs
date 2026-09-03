// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — IDT + CPU exception handlers.
//!
//! Only the CPU-defined vectors (0..32) for now. Hardware IRQs (APIC) and the
//! `syscall` fast path come with the interrupt-controller and personality work.
//!
//! Every fatal handler dumps the trap frame over serial and halts via
//! `exit_qemu(Failed)` so a headless run fails loudly instead of spinning.

use spin::Lazy;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::{apic, exit_qemu, gdt, hcf, kprintln, ExitCode};

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    idt.breakpoint.set_handler_fn(breakpoint);
    idt.divide_error.set_handler_fn(divide_error);
    // #UD routes through a GPR-saving stub so a PE process's `#UD` can be
    // delivered to ring-3 SEH (see `crate::seh`).
    unsafe {
        idt.invalid_opcode
            .set_handler_addr(x86_64::VirtAddr::new(crate::seh::thos_ud_entry as *const () as u64));
    }
    idt.general_protection_fault.set_handler_fn(general_protection_fault);
    idt.stack_segment_fault.set_handler_fn(stack_segment_fault);

    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        idt.non_maskable_interrupt
            .set_handler_fn(nmi)
            .set_stack_index(gdt::NMI_IST_INDEX);
        idt.page_fault
            .set_handler_fn(page_fault)
            .set_stack_index(gdt::PAGE_FAULT_IST_INDEX);
    }

    // APIC vectors (>= 32).
    idt[apic::TIMER_VECTOR].set_handler_fn(apic_timer);
    idt[apic::AHCI_VECTOR].set_handler_fn(ahci_irq);
    idt[apic::SPURIOUS_VECTOR].set_handler_fn(apic_spurious);

    idt
});

pub fn init() {
    IDT.load();
}

// --- non-fatal ---

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    kprintln!(
        "THOS trap: #BP at {:#x}",
        frame.instruction_pointer.as_u64()
    );
}

extern "x86-interrupt" fn apic_timer(_frame: InterruptStackFrame) {
    apic::on_timer_tick();
    apic::eoi();
    crate::ahci::poll_wake(); // safety net for a dropped AHCI completion IRQ
    crate::sched::on_tick();
}

extern "x86-interrupt" fn ahci_irq(_frame: InterruptStackFrame) {
    crate::ahci::on_irq();
    apic::eoi();
}

extern "x86-interrupt" fn apic_spurious(_frame: InterruptStackFrame) {
    // A spurious interrupt gets no EOI by design.
}

// --- fatal ---

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
    fatal("#DE divide error", &frame, None);
}

extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    kprintln!("THOS trap: NMI\n{:#?}", frame);
    exit_qemu(ExitCode::Failed);
    hcf();
}

extern "x86-interrupt" fn general_protection_fault(frame: InterruptStackFrame, code: u64) {
    fatal("#GP general protection fault", &frame, Some(code));
}

extern "x86-interrupt" fn stack_segment_fault(frame: InterruptStackFrame, code: u64) {
    fatal("#SS stack-segment fault", &frame, Some(code));
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    kprintln!("THOS trap: #DF double fault (error {:#x})\n{:#?}", code, frame);
    exit_qemu(ExitCode::Failed);
    hcf();
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let cr2 = x86_64::registers::control::Cr2::read_raw();
    kprintln!(
        "THOS trap: #PF cr2={:#x} err={:?} rip={:#x}",
        cr2,
        code,
        frame.instruction_pointer.as_u64()
    );
    fatal("#PF page fault", &frame, None);
}

/// A fault from ring 3 kills the process; a fault in the kernel is fatal.
fn fatal(name: &str, frame: &InterruptStackFrame, code: Option<u64>) -> ! {
    let from_user = frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    match code {
        Some(c) => kprintln!("THOS trap: {} (error {:#x}){}", name, c, if from_user { " [user]" } else { "" }),
        None => kprintln!("THOS trap: {}{}", name, if from_user { " [user]" } else { "" }),
    }
    if from_user {
        kprintln!("  killed user rip={:#x}", frame.instruction_pointer.as_u64());
        crate::process::set_exit_status(139); // 128 + SIGSEGV
        crate::syscall::note_user_exit();
        crate::sched::exit();
    }
    kprintln!("{:#?}", frame);
    exit_qemu(ExitCode::Failed);
    hcf();
}
