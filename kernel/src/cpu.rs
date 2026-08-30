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
