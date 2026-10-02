// SPDX-License-Identifier: GPL-2.0-or-later
//! Validation of pointers a process hands the kernel.
//!
//! Until now the syscall layer dereferenced user-supplied pointers directly. A
//! process could pass a *kernel* address as a `read()` buffer (writing into
//! kernel memory) or as a `write()` buffer (leaking it), or an unmapped address
//! (a kernel page fault). Every pointer that crosses the boundary now goes
//! through here first:
//!
//!  * the whole range must lie in the user half of the address space,
//!  * every page must be mapped and user-accessible in the *current* address
//!    space (and writable if the kernel will write to it).
//!
//! Failure is `-EFAULT`. The kernel runs on the caller's page tables during a
//! syscall, so a validated range can be used in place — nothing is copied.
//!
//! Residual race: another thread of the same process could unmap a page between
//! the check and the use. That can no longer touch kernel memory (the range is
//! user-half either way); at worst the kernel faults on a user address, which
//! `seh::thos_fault_dispatch` turns into "kill the process" instead of a panic.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{sched, vmm};

/// First address above the user half of the canonical address space.
pub const USER_TOP: u64 = 0x0000_8000_0000_0000;
pub const EFAULT: i64 = -14;

/// Is `[ptr, ptr+len)` entirely user memory that is mapped (and writable, if
/// `write`) in the calling process? An empty range is always fine.
pub fn user_ok(ptr: u64, len: usize, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = ptr.checked_add(len as u64) else { return false };
    if ptr == 0 || end > USER_TOP {
        return false;
    }
    let Some(proc) = sched::current_proc() else { return false };
    let pml4 = proc.pml4_phys();
    let mut page = ptr & !0xFFF;
    while page < end {
        match vmm::user_page_access(pml4, page) {
            Some(w) if w || !write => {}
            _ => return false,
        }
        page += 4096;
    }
    true
}

/// Validated read-only view of user memory.
pub fn slice(ptr: u64, len: usize) -> Result<&'static [u8], i64> {
    if len == 0 {
        return Ok(&[]);
    }
    if !user_ok(ptr, len, false) {
        return Err(EFAULT);
    }
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
}

/// Validated writable view of user memory.
pub fn slice_mut(ptr: u64, len: usize) -> Result<&'static mut [u8], i64> {
    if len == 0 {
        return Ok(&mut []);
    }
    if !user_ok(ptr, len, true) {
        return Err(EFAULT);
    }
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) })
}

/// Read one `u64` from user memory.
pub fn read_u64(ptr: u64) -> Result<u64, i64> {
    let b = slice(ptr, 8)?;
    Ok(u64::from_le_bytes(b.try_into().unwrap()))
}

/// Read one `u32` from user memory.
pub fn read_u32(ptr: u64) -> Result<u32, i64> {
    let b = slice(ptr, 4)?;
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}

/// Write one `u64` to user memory.
pub fn write_u64(ptr: u64, v: u64) -> Result<(), i64> {
    slice_mut(ptr, 8)?.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

/// Write one `u32` to user memory.
pub fn write_u32(ptr: u64, v: u32) -> Result<(), i64> {
    slice_mut(ptr, 4)?.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

/// Read a NUL-terminated string of at most `max` bytes. `E2BIG`-style callers
/// pass a generous `max`; a string that does not terminate within it is
/// `EFAULT`'s cousin `-ENAMETOOLONG` (-36).
pub fn cstr_bytes(ptr: u64, max: usize) -> Result<Vec<u8>, i64> {
    let mut out = Vec::new();
    let mut p = ptr;
    loop {
        // Validate one page at a time so a long string costs one walk per page.
        let page_end = (p | 0xFFF) + 1;
        let chunk = (page_end - p) as usize;
        let b = slice(p, chunk)?;
        for &c in b {
            if c == 0 {
                return Ok(out);
            }
            if out.len() >= max {
                return Err(-36);
            }
            out.push(c);
        }
        p = page_end;
    }
}

/// [`cstr_bytes`] as a (lossy UTF-8) `String`.
pub fn cstr(ptr: u64, max: usize) -> Result<String, i64> {
    Ok(String::from_utf8_lossy(&cstr_bytes(ptr, max)?).into_owned())
}

/// The `i`-th stack argument of a Win64 call (`[rsp + 0x28 + 8*i]`), read
/// safely: the stack pointer is whatever the process had when it executed
/// `syscall`, i.e. attacker-controlled. Out-of-range reads yield 0.
pub fn win64_stack_arg(rsp: u64, i: u64) -> u64 {
    rsp.checked_add(0x28 + i * 8).and_then(|a| read_u64(a).ok()).unwrap_or(0)
}

/// Anything that names a user address: a raw `u64`, or a (`*mut`/`*const`)
/// pointer such as `buf.add(2)` derived from one.
pub trait Addr {
    fn addr(self) -> u64;
}
impl Addr for u64 {
    fn addr(self) -> u64 {
        self
    }
}
impl<T> Addr for *mut T {
    fn addr(self) -> u64 {
        self as u64
    }
}
impl<T> Addr for *const T {
    fn addr(self) -> u64 {
        self as u64
    }
}

/// Read a `T` from user memory; a bad pointer yields `T::default()` (0) — the
/// kernel never touches memory outside validated user pages.
pub fn get<T: Copy + Default>(ptr: impl Addr) -> T {
    match slice(ptr.addr(), core::mem::size_of::<T>()) {
        Ok(b) => unsafe { core::ptr::read_unaligned(b.as_ptr() as *const T) },
        Err(_) => T::default(),
    }
}

/// Write a `T` to user memory; `false` (and nothing written) if the target is
/// not validated writable user memory.
pub fn put<T: Copy>(ptr: impl Addr, v: T) -> bool {
    match slice_mut(ptr.addr(), core::mem::size_of::<T>()) {
        Ok(b) => {
            unsafe { core::ptr::write_unaligned(b.as_mut_ptr() as *mut T, v) };
            true
        }
        Err(_) => false,
    }
}
