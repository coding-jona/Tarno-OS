// SPDX-License-Identifier: GPL-2.0-or-later
// THOS: `poweroff` / `reboot` / `halt` — one static binary installed under three
// names (it dispatches on basename(argv[0]), like BusyBox). BusyBox's own versions
// look for `init` in /proc, which THOS does not have, so these call the Linux
// `reboot(2)` syscall directly. Extra flags (-f, -n, ...) are accepted and ignored.
fn main() {
    let name = std::env::args().next().unwrap_or_default();
    let base = name.rsplit('/').next().unwrap_or("");
    let cmd: u64 = match base {
        "reboot" => 0x0123_4567,
        "halt" => 0xCDEF_0123,
        _ => 0x4321_FEDC, // poweroff (and anything else)
    };
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") 169u64 => ret, // SYS_reboot
            in("rdi") 0xfee1_deadu64,
            in("rsi") 672_274_793u64,
            in("rdx") cmd,
            in("r10") 0u64,
            out("rcx") _,
            out("r11") _,
        );
    }
    eprintln!("{base}: failed ({ret})");
    std::process::exit(1);
}
