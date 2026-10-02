// SPDX-License-Identifier: GPL-2.0-or-later
//! Per-CPU control-register setup that has to run on every core.

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// Enable SSE/SSE2 so userland (and the compiler's vector codegen) can use
/// `xmm` registers. Limine enables this on the BSP but not reliably on APs, so
/// every CPU does it itself.
pub fn enable_sse() {
    unsafe {
        Cr0::update(|f| {
            f.remove(Cr0Flags::EMULATE_COPROCESSOR); // EM = 0
            f.insert(Cr0Flags::MONITOR_COPROCESSOR); // MP = 1
        });
        Cr4::update(|f| {
            f.insert(Cr4Flags::OSFXSR); // FXSAVE/FXRSTOR + SSE
            f.insert(Cr4Flags::OSXMMEXCPT_ENABLE);
        });
    }
}

/// Turn on SMEP (supervisor-mode execution prevention) where the CPU has it
/// (Ivy Bridge and later; the Acer's Westmere does not): the kernel can then
/// never execute a user page, which removes the classic "return to a user
/// payload" step from any kernel exploit. Returns whether it was enabled.
/// SMAP (no kernel *reads/writes* of user pages outside `stac`/`clac`) would
/// need every user access to be bracketed; `usercopy` is the place to add that.
pub fn enable_smep() -> bool {
    let max = core::arch::x86_64::__cpuid(0).eax;
    if max < 7 {
        return false;
    }
    let ebx = core::arch::x86_64::__cpuid_count(7, 0).ebx;
    if ebx & (1 << 7) == 0 {
        return false;
    }
    unsafe { Cr4::update(|f| f.insert(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION)) };
    true
}
