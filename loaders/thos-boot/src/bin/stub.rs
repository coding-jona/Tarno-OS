// SPDX-License-Identifier: GPL-2.0-or-later
//! A stand-in "OS loader" for `cargo xtask bootpick-test`: print our own image
//! path so the test can prove the picker chainloaded the right entry, then exit.

#![no_std]
#![no_main]

extern crate alloc;

use thos_boot as _; // link the wide-string libc shims (see lib.rs)

use alloc::string::ToString;

use uefi::boot;
use uefi::proto::loaded_image::LoadedImage;
use uefi::Status;

#[uefi::entry]
fn main() -> Status {
    uefi::helpers::init().expect("helpers::init");

    let path = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
        .ok()
        .and_then(|li| li.file_path().map(|dp| dp.to_string()));

    match path {
        Some(p) => uefi::println!("STUB OK: {p}"),
        None => uefi::println!("STUB OK: <no path>"),
    }
    Status::SUCCESS
}
