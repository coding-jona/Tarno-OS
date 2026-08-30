// SPDX-License-Identifier: GPL-2.0-or-later
//! Make cargo rebuild the kernel when the linker script changes (it is passed
//! via `-T` in `.cargo/config.toml`, which cargo does not track on its own).

fn main() {
    println!("cargo:rerun-if-changed=linker.ld");
}
