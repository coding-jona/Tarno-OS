// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared bits for the `thos-boot` binaries.
//!
//! The only thing here is a set of wide-string libc shims. LLVM's loop-idiom
//! pass rewrites `uefi-rs`'s UCS-2 scan loops (`CStr16` length/compare) into
//! calls to `wcslen` / `wcscmp` / `wmemchr`, which the bare
//! `x86_64-unknown-uefi` target provides no implementation for. Each shim uses
//! a volatile read so the optimizer cannot fold it back into the same libcall.

#![no_std]

/// # Safety
/// `s` must point to a NUL-terminated array of `u16`.
#[no_mangle]
pub unsafe extern "C" fn wcslen(s: *const u16) -> usize {
    let mut n = 0usize;
    while core::ptr::read_volatile(s.add(n)) != 0 {
        n += 1;
    }
    n
}

/// # Safety
/// `a` and `b` must be NUL-terminated arrays of `u16`.
#[no_mangle]
pub unsafe extern "C" fn wcscmp(a: *const u16, b: *const u16) -> i32 {
    let mut i = 0usize;
    loop {
        let (x, y) = (core::ptr::read_volatile(a.add(i)), core::ptr::read_volatile(b.add(i)));
        if x != y {
            return x as i32 - y as i32;
        }
        if x == 0 {
            return 0;
        }
        i += 1;
    }
}

/// # Safety
/// `s` must be valid for `n` `u16` reads.
#[no_mangle]
pub unsafe extern "C" fn wmemchr(s: *const u16, c: u16, n: usize) -> *const u16 {
    let mut i = 0usize;
    while i < n {
        if core::ptr::read_volatile(s.add(i)) == c {
            return s.add(i);
        }
        i += 1;
    }
    core::ptr::null()
}
