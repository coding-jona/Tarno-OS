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

use crate::{exit_qemu, gdt, hcf, kprintln, ExitCode};

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    idt.breakpoint.set_handler_fn(breakpoint);
    idt.divide_error.set_handler_fn(divide_error);
    idt.invalid_opcode.set_handler_fn(invalid_opcode);
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

// --- fatal ---

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
    fatal("#DE divide error", &frame, None);
}

extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
    fatal("#UD invalid opcode", &frame, None);
}

extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    fatal("NMI", &frame, None);
}

extern "x86-interrupt" fn general_protection_fault(frame: InterruptStackFrame, code: u64) {
    fatal("#GP general protection fault", &frame, Some(code));
}

extern "x86-interrupt" fn stack_segment_fault(frame: InterruptStackFrame, code: u64) {
    fatal("#SS stack-segment fault", &frame, Some(code));
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    fatal("#DF double fault", &frame, Some(code));
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let cr2 = x86_64::registers::control::Cr2::read_raw();
    kprintln!("THOS trap: #PF page fault  cr2={:#x}  err={:?}", cr2, code);
    kprintln!("{:#?}", frame);
    exit_qemu(ExitCode::Failed);
    hcf();
}

fn fatal(name: &str, frame: &InterruptStackFrame, code: Option<u64>) -> ! {
    match code {
        Some(c) => kprintln!("THOS trap: {} (error {:#x})", name, c),
        None => kprintln!("THOS trap: {}", name),
    }
    kprintln!("{:#?}", frame);
    exit_qemu(ExitCode::Failed);
    hcf();
}
