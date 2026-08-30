// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — the process / address-space object.
//!
//! A `Process` owns a private top-level page table: a full copy of the kernel
//! PML4 (so the kernel half + HHDM + identity map are shared, by pointer, with
//! every process) plus its own user-half entries. A user thread carries the
//! physical base of its process's PML4; the scheduler loads it into CR3 on the
//! switch.
//!
//! No `fork` sharing / COW yet, no address-space teardown (a reaper frees the
//! frames later) — this is here to give ELF programs real isolation.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::structures::paging::{PageTable, PageTableFlags};
use x86_64::PhysAddr;

use crate::elf::Image;
use crate::mm::{hhdm_offset, phys_to_virt, FRAME_ALLOC};
use crate::vmm;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

#[allow(dead_code)] // pid consumed by SYS_GETPID soon
pub struct Process {
    pub pid: u64,
    pml4_phys: u64,
    /// Next free user virtual address for `mmap` / stacks.
    next_user_va: AtomicU64,
    /// Program break for `brk`.
    brk: AtomicU64,
}

/// User virtual space for `mmap` / stacks, clear of typical ELF load addresses.
const USER_ALLOC_BASE: u64 = 0x0000_7000_0000_0000;
const USER_STACK_SIZE: u64 = 64 * 1024;
const BRK_BASE: u64 = 0x0000_6800_0000_0000;
const BRK_MAX: u64 = BRK_BASE + 256 * 1024 * 1024;

impl Process {
    pub fn new() -> Arc<Self> {
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame for process PML4");
        let pml4_phys = frame.start_address().as_u64();

        // Copy every entry of the kernel PML4: kernel-half + HHDM + identity are
        // then shared with this process; the user half starts empty.
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(PhysAddr::new(vmm::kernel_pml4_phys())).as_ptr::<u8>(),
                phys_to_virt(PhysAddr::new(pml4_phys)).as_mut_ptr::<u8>(),
                4096,
            );
        }

        Arc::new(Self {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            pml4_phys,
            next_user_va: AtomicU64::new(USER_ALLOC_BASE),
            brk: AtomicU64::new(BRK_BASE),
        })
    }

    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    /// Map one 4 KiB user page into this address space.
    pub fn map(&self, virt: u64, phys: u64, writable: bool, exec: bool) {
        vmm::map_page_in(self.pml4_phys, virt, phys, writable, true, exec);
    }

    fn map_zeroed(&self, virt: u64) {
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame");
        unsafe {
            core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
        }
        self.map(virt, frame.start_address().as_u64(), true, false);
    }

    /// `brk(0)` returns the current break; `brk(addr)` grows/sets it.
    pub fn brk(&self, req: u64) -> u64 {
        let cur = self.brk.load(Ordering::Relaxed);
        if req < BRK_BASE || req > BRK_MAX {
            return cur;
        }
        let mut v = (cur + 0xFFF) & !0xFFF;
        let end = (req + 0xFFF) & !0xFFF;
        while v < end {
            self.map_zeroed(v);
            v += 4096;
        }
        self.brk.store(req, Ordering::Relaxed);
        req
    }

    /// Anonymous `mmap`: bump-allocate + map `len` bytes RW, return the base.
    pub fn mmap_anon(&self, len: u64) -> u64 {
        let len = (len + 0xFFF) & !0xFFF;
        let base = self.next_user_va.fetch_add(len + 0x1000, Ordering::Relaxed);
        let mut v = base;
        while v < base + len {
            self.map_zeroed(v);
            v += 4096;
        }
        base
    }

    /// Allocate + map a fresh user stack; returns the (page-aligned) stack top.
    pub fn new_user_stack(&self) -> u64 {
        let base = self.next_user_va.fetch_add(USER_STACK_SIZE + 0x1000, Ordering::Relaxed);
        let pages = USER_STACK_SIZE / 4096;
        for i in 0..pages {
            let frame = FRAME_ALLOC.lock().alloc().expect("no frame for user stack");
            unsafe {
                core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
            }
            self.map(base + i * 4096, frame.start_address().as_u64(), true, false);
        }
        base + USER_STACK_SIZE
    }

    /// Walk this address space's page tables (via HHDM) to a physical address.
    pub fn translate(&self, virt: u64) -> Option<u64> {
        let hhdm = hhdm_offset();
        let idx = [
            (virt >> 39) & 0x1FF,
            (virt >> 30) & 0x1FF,
            (virt >> 21) & 0x1FF,
            (virt >> 12) & 0x1FF,
        ];
        let mut table_phys = self.pml4_phys;
        for (level, &i) in idx.iter().enumerate() {
            let table = unsafe { &*((table_phys + hhdm) as *const PageTable) };
            let e = &table[i as usize];
            if !e.flags().contains(PageTableFlags::PRESENT) {
                return None;
            }
            if level < 3 && e.flags().contains(PageTableFlags::HUGE_PAGE) {
                return None;
            }
            table_phys = e.addr().as_u64();
        }
        Some(table_phys + (virt & 0xFFF))
    }

    /// Copy bytes into this address space at `virt` (page by page, via HHDM).
    pub fn write_user(&self, mut virt: u64, mut data: &[u8]) {
        let hhdm = hhdm_offset();
        while !data.is_empty() {
            let phys = self.translate(virt).expect("write_user: unmapped page");
            let off = (virt & 0xFFF) as usize;
            let n = (4096 - off).min(data.len());
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), (phys + hhdm) as *mut u8, n);
            }
            virt += n as u64;
            data = &data[n..];
        }
    }

    /// Lay out the SysV AMD64 initial process stack (argc, argv, envp, auxv,
    /// AT_RANDOM) at the top of a fresh user stack. Returns the entry `rsp`.
    pub fn init_stack(&self, stack_top: u64, argv: &[&str], envp: &[&str], img: &Image) -> u64 {
        let mut cur = stack_top;
        let push = |cur: &mut u64, bytes: &[u8]| -> u64 {
            *cur -= bytes.len() as u64;
            self.write_user(*cur, bytes);
            *cur
        };

        let rand_addr = push(&mut cur, &[0x5Au8; 16]); // AT_RANDOM material
        let cstr = |cur: &mut u64, s: &str| -> u64 {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            push(cur, &b)
        };
        let arg_ptrs: Vec<u64> = argv.iter().map(|s| cstr(&mut cur, s)).collect();
        let env_ptrs: Vec<u64> = envp.iter().map(|s| cstr(&mut cur, s)).collect();
        let execfn = arg_ptrs.first().copied().unwrap_or(0);

        // auxv (type, value) pairs — AT_NULL last.
        let aux: [(u64, u64); 8] = [
            (3, img.phdr),   // AT_PHDR
            (4, img.phent),  // AT_PHENT
            (5, img.phnum),  // AT_PHNUM
            (6, 4096),       // AT_PAGESZ
            (9, img.entry),  // AT_ENTRY
            (25, rand_addr), // AT_RANDOM
            (31, execfn),    // AT_EXECFN
            (0, 0),          // AT_NULL
        ];

        let words = 1                       // argc
            + (arg_ptrs.len() + 1)          // argv + NULL
            + (env_ptrs.len() + 1)          // envp + NULL
            + aux.len() * 2; // auxv pairs
        let block_bytes = (words * 8) as u64;

        // Align so that the final rsp is 16-byte aligned.
        cur -= (cur - block_bytes) % 16;
        let rsp = cur - block_bytes;

        let mut block: Vec<u8> = Vec::with_capacity(words * 8);
        let mut w = |v: u64| block.extend_from_slice(&v.to_le_bytes());
        w(argv.len() as u64);
        for p in &arg_ptrs {
            w(*p);
        }
        w(0);
        for p in &env_ptrs {
            w(*p);
        }
        w(0);
        for (t, v) in aux {
            w(t);
            w(v);
        }
        self.write_user(rsp, &block);
        rsp
    }
}
