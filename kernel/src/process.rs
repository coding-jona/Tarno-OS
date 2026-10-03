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

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use spin::Mutex;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::{PageTable, PageTableFlags, PhysFrame};
use x86_64::PhysAddr;

use crate::elf::{self, Image};
use crate::file::{ConsoleFile, FileOps, KeyboardFile};
use crate::mm::{hhdm_offset, phys_to_virt, FRAME_ALLOC};
use crate::wait::{Event, Mutant, Semaphore};
use crate::syscall::{self, UserFrame};
use crate::{gdt, sched, vmm};

/// One address space. Shared kernel half (by pointer) + private user half.
pub struct Process {
    pml4_phys: u64,
    /// Next free user virtual address for `mmap` / stacks.
    next_user_va: AtomicU64,
    /// Program break for `brk`.
    brk: AtomicU64,
    /// Active `NtMapViewOfSection` views in this address space, so
    /// `NtUnmapViewOfSection` / `NtFlushVirtualMemory` can find which
    /// `Section` (and how many pages) a base VA names.
    views: Mutex<Vec<SectionView>>,
}

struct SectionView {
    base: u64,
    pages: usize,
    section: Arc<Section>,
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

        // Copy every entry of the kernel PML4 so the kernel half + HHDM are
        // shared with this process, then drop PML4[0] — the kernel's low-4 GiB
        // identity map. Kernel-CR3 threads keep it; a process must have its
        // entire low half free so an ELF (e.g. static musl at 0x400000) can map
        // there without colliding with a 1 GiB identity huge page.
        unsafe {
            let dst = phys_to_virt(PhysAddr::new(pml4_phys)).as_mut_ptr::<u8>();
            core::ptr::copy_nonoverlapping(
                phys_to_virt(PhysAddr::new(vmm::kernel_pml4_phys())).as_ptr::<u8>(),
                dst,
                4096,
            );
            *(dst as *mut u64) = 0; // PML4[0]
        }

        Arc::new(Self {
            pml4_phys,
            next_user_va: AtomicU64::new(USER_ALLOC_BASE),
            brk: AtomicU64::new(BRK_BASE),
            views: Mutex::new(Vec::new()),
        })
    }

    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    fn copy_alloc_state_from(&self, other: &Process) {
        self.next_user_va
            .store(other.next_user_va.load(Ordering::Relaxed), Ordering::Relaxed);
        self.brk.store(other.brk.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    /// Visit every present 4 KiB user page: `(virt, phys, writable, exec)`.
    fn for_each_user_page(&self, mut f: impl FnMut(u64, u64, bool, bool, bool)) {
        let hhdm = hhdm_offset();
        let tbl = |phys: u64| unsafe { &*((phys + hhdm) as *const PageTable) };
        // 0..256 = the whole user half. Index 0 matters: a static-musl ELF
        // loads at 0x400000, so its text/data live under PML4[0]. (The kernel's
        // low identity map was already dropped from PML4[0] in `Process::new`.)
        for i4 in 0..256u64 {
            let e4 = &tbl(self.pml4_phys)[i4 as usize];
            if !e4.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            for i3 in 0..512u64 {
                let e3 = &tbl(e4.addr().as_u64())[i3 as usize];
                if !e3.flags().contains(PageTableFlags::PRESENT)
                    || e3.flags().contains(PageTableFlags::HUGE_PAGE)
                {
                    continue;
                }
                for i2 in 0..512u64 {
                    let e2 = &tbl(e3.addr().as_u64())[i2 as usize];
                    if !e2.flags().contains(PageTableFlags::PRESENT)
                        || e2.flags().contains(PageTableFlags::HUGE_PAGE)
                    {
                        continue;
                    }
                    for i1 in 0..512u64 {
                        let e1 = &tbl(e2.addr().as_u64())[i1 as usize];
                        let fl = e1.flags();
                        if !fl.contains(PageTableFlags::PRESENT)
                            || !fl.contains(PageTableFlags::USER_ACCESSIBLE)
                        {
                            continue;
                        }
                        let virt = (i4 << 39) | (i3 << 30) | (i2 << 21) | (i1 << 12);
                        f(
                            virt,
                            e1.addr().as_u64(),
                            fl.contains(PageTableFlags::WRITABLE),
                            !fl.contains(PageTableFlags::NO_EXECUTE),
                            fl.contains(vmm::DEVICE_PAGE),
                        );
                    }
                }
            }
        }
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

    /// POSIX `mmap`: `fixed` places it at `addr` (replacing whatever was mapped there),
    /// otherwise a fresh range is chosen. `prot == 0` only reserves address space (nothing
    /// is mapped; `MAP_FIXED` over an old mapping removes it). With `file` the pages are
    /// filled from `(data, offset)` — a private copy, which is what `MAP_PRIVATE` means.
    /// Returns the base.
    pub fn mmap_region(&self, addr: u64, fixed: bool, len: u64, prot: u64, data: Option<&[u8]>) -> u64 {
        let len = (len + 0xFFF) & !0xFFF;
        let base = if fixed {
            addr
        } else {
            self.next_user_va.fetch_add(len + 0x1000, Ordering::Relaxed)
        };
        let (writable, exec) = (prot & 2 != 0, prot & 4 != 0);
        let mut v = base;
        while v < base + len {
            if fixed {
                self.release_page(v); // MAP_FIXED replaces whatever was there
            }
            if prot != 0 {
                let frame = FRAME_ALLOC.lock().alloc().expect("no frame for mmap");
                let dst = phys_to_virt(frame.start_address()).as_mut_ptr::<u8>();
                unsafe { core::ptr::write_bytes(dst, 0, 4096) };
                if let Some(d) = data {
                    let off = (v - base) as usize;
                    if off < d.len() {
                        let n = (d.len() - off).min(4096);
                        unsafe { core::ptr::copy_nonoverlapping(d.as_ptr().add(off), dst, n) };
                    }
                }
                self.map(v, frame.start_address().as_u64(), writable, exec);
            }
            v += 4096;
        }
        base
    }

    /// Unmap one user page and give its frame back — unless it belongs to a shared section
    /// view (`NtMapViewOfSection`), whose frames are owned by the section, not by this process.
    fn release_page(&self, virt: u64) {
        let Some(phys) = self.translate(virt) else { return };
        let shared = vmm::is_device_page(self.pml4_phys, virt)
            || self
                .views
                .lock()
                .iter()
                .any(|v| virt >= v.base && virt < v.base + v.pages as u64 * 4096);
        vmm::unmap_page_in(self.pml4_phys, virt); // also flushes this CPU's TLB entry
        if !shared {
            FRAME_ALLOC.lock().dealloc(x86_64::structures::paging::PhysFrame::containing_address(
                x86_64::PhysAddr::new(phys & !0xFFF),
            ));
        }
    }

    /// Map device memory (`phys..phys+len`, e.g. the framebuffer) into this process.
    pub fn mmap_device(&self, addr: u64, fixed: bool, len: u64, prot: u64, phys: u64) -> u64 {
        let len = (len + 0xFFF) & !0xFFF;
        let base = if fixed { addr } else { self.next_user_va.fetch_add(len + 0x1000, Ordering::Relaxed) };
        let mut off = 0;
        while off < len {
            if fixed {
                self.release_page(base + off);
            }
            vmm::map_device_page_in(self.pml4_phys, base + off, phys + off, prot & 2 != 0);
            off += 4096;
        }
        base
    }

    /// `munmap`: drop the mappings in `[addr, addr+len)` and return their frames.
    pub fn munmap(&self, addr: u64, len: u64) {
        let (start, end) = (addr & !0xFFF, (addr + len + 0xFFF) & !0xFFF);
        let mut v = start;
        while v < end {
            self.release_page(v);
            v += 4096;
        }
    }

    /// `mprotect`. `PROT_NONE` removes the pages (their frames go back — decommitting is what
    /// allocators use it for); a real protection on pages that are not mapped yet maps them
    /// zeroed (a `PROT_NONE` reservation being committed, e.g. a thread stack); mapped pages
    /// just change their writable / executable bits.
    pub fn mprotect(&self, addr: u64, len: u64, prot: u64) -> i64 {
        let (start, end) = (addr & !0xFFF, (addr + len + 0xFFF) & !0xFFF);
        let (writable, exec) = (prot & 2 != 0, prot & 4 != 0);
        let mut v = start;
        while v < end {
            if prot == 0 {
                self.release_page(v);
            } else if vmm::page_present_in(self.pml4_phys, v) {
                let _ = self.protect(v, 4096, writable, exec);
            } else {
                let Some(frame) = FRAME_ALLOC.lock().alloc() else { return -12 }; // ENOMEM
                unsafe {
                    core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
                }
                self.map(v, frame.start_address().as_u64(), writable, exec);
            }
            v += 4096;
        }
        0
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

    /// `NtMapViewOfSection`: map `sec`'s frames covering `[offset, offset+len)`
    /// into a freshly reserved VA range in *this* address space. Unlike
    /// `mmap_anon`, no new physical memory is allocated — the same frames get
    /// mapped, so a write here is a write into `sec` itself, visible to every
    /// other view of it (this process or any other) with no copy. Returns the
    /// view's base VA; the view is remembered so `unmap_view`/`flush_view` can
    /// find `sec` again from that VA.
    pub fn map_section_view(&self, sec: &Arc<Section>, offset: usize, len: usize) -> u64 {
        let frames = sec.frames_for(offset, len);
        let base = self.next_user_va.fetch_add(frames.len() as u64 * 4096 + 0x1000, Ordering::Relaxed);
        for (i, f) in frames.iter().enumerate() {
            self.map(base + i as u64 * 4096, f.start_address().as_u64(), true, false);
        }
        self.views.lock().push(SectionView { base, pages: frames.len(), section: sec.clone() });
        base
    }

    fn view_at(&self, base: u64) -> Option<Arc<Section>> {
        self.views.lock().iter().find(|v| v.base == base).map(|v| v.section.clone())
    }

    /// `NtFlushVirtualMemory`: write the view at `base`'s section back to its
    /// file (a no-op success for an anonymous section). Keeps the mapping.
    pub fn flush_view(&self, base: u64) -> bool {
        self.view_at(base).is_some_and(|s| s.flush())
    }

    /// `NtUnmapViewOfSection`: flush (best-effort — the unmap proceeds either
    /// way, matching real NT), then tear down the PTEs and forget the view.
    /// `false` if `base` doesn't name an active view in this process.
    pub fn unmap_view(&self, base: u64) -> bool {
        let removed = {
            let mut views = self.views.lock();
            let pos = views.iter().position(|v| v.base == base);
            pos.map(|i| views.swap_remove(i))
        };
        let Some(v) = removed else { return false };
        v.section.flush();
        for i in 0..v.pages {
            vmm::unmap_page_in(self.pml4_phys, v.base + i as u64 * 4096);
        }
        true
    }

    /// `NtProtectVirtualMemory`/`VirtualProtect`: change `writable`/`exec` on
    /// every page in `[virt, virt+len)` (rounded outward to page
    /// boundaries). Real `VirtualProtect` validates the *whole* region is
    /// committed before changing anything and fails the call entirely
    /// otherwise (`ERROR_INVALID_ADDRESS`) — so this checks every page is
    /// present first, then applies. `None` if any page in the range isn't
    /// mapped; otherwise `Some((old_writable, old_exec))` from the first
    /// page (real NT reports one previous-protection value for the region
    /// too, so this matches even though THOS doesn't track per-page history
    /// beyond what the PTE itself already encodes).
    pub fn protect(&self, virt: u64, len: u64, writable: bool, exec: bool) -> Option<(bool, bool)> {
        let start = virt & !0xFFF;
        let end = (virt + len + 0xFFF) & !0xFFF;
        let mut v = start;
        while v < end {
            vmm::page_present_in(self.pml4_phys, v).then_some(())?;
            v += 4096;
        }
        let mut old = None;
        let mut v = start;
        while v < end {
            let this_old = vmm::protect_page_in(self.pml4_phys, v, writable, exec)?;
            old.get_or_insert(this_old);
            v += 4096;
        }
        old
    }

    /// Reclaim this address space's frames back to `FRAME_ALLOC`: every
    /// section view's PTEs (never the frames — those belong to the
    /// `Section`, which may still be live elsewhere), then every remaining
    /// present user-half page (now provably this process's *own* — ELF/PE
    /// image, stacks, heap, TLS, TEB/PEB/stub pages) plus the PT/PD/PDPT
    /// frames that mapped them, and finally the PML4 frame itself.
    ///
    /// **Caller's responsibility, not this function's**: this process's
    /// `pml4_phys` must not be the live CR3 on *any* CPU, now or later — the
    /// two call sites (`execve`, right after its own `Cr3::write` off this
    /// space; `sched::reap`, once the last `Thread` referencing this
    /// `Process`'s `Task` is confirmed not running anywhere) each establish
    /// that before calling this.
    fn teardown(&self) {
        let bases: alloc::vec::Vec<u64> = self.views.lock().iter().map(|v| v.base).collect();
        for base in bases {
            self.unmap_view(base);
        }
        self.free_user_address_space();
    }

    /// The raw page-table walk `teardown` uses: PML4[0..256] only (the
    /// user half — PML4[256..512] plus the low identity map that PML4[0]'s
    /// *kernel* half briefly overlapped are the kernel's own frames, shared
    /// by pointer, never this process's to free). A 1 GiB/2 MiB entry is
    /// skipped rather than freed as if it were a 4 KiB frame — THOS never
    /// creates a user huge-page mapping, but this is not the place to first
    /// notice one exists.
    fn free_user_address_space(&self) {
        let hhdm = hhdm_offset();
        let tbl = |phys: u64| unsafe { &*((phys + hhdm) as *const PageTable) };
        let mut fa = FRAME_ALLOC.lock();

        for i4 in 0..256usize {
            let e4 = &tbl(self.pml4_phys)[i4];
            if !e4.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            let pdpt_phys = e4.addr().as_u64();
            for i3 in 0..512usize {
                let e3 = &tbl(pdpt_phys)[i3];
                let f3 = e3.flags();
                if !f3.contains(PageTableFlags::PRESENT) || f3.contains(PageTableFlags::HUGE_PAGE) {
                    continue;
                }
                let pd_phys = e3.addr().as_u64();
                for i2 in 0..512usize {
                    let e2 = &tbl(pd_phys)[i2];
                    let f2 = e2.flags();
                    if !f2.contains(PageTableFlags::PRESENT) || f2.contains(PageTableFlags::HUGE_PAGE) {
                        continue;
                    }
                    let pt_phys = e2.addr().as_u64();
                    for i1 in 0..512usize {
                        let e1 = &tbl(pt_phys)[i1];
                        if e1.flags().contains(PageTableFlags::PRESENT) && !e1.flags().contains(vmm::DEVICE_PAGE) {
                            fa.dealloc(PhysFrame::containing_address(e1.addr()));
                        }
                    }
                    fa.dealloc(PhysFrame::containing_address(PhysAddr::new(pt_phys)));
                }
                fa.dealloc(PhysFrame::containing_address(PhysAddr::new(pd_phys)));
            }
            fa.dealloc(PhysFrame::containing_address(PhysAddr::new(pdpt_phys)));
        }
        fa.dealloc(PhysFrame::containing_address(PhysAddr::new(self.pml4_phys)));
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

        let mut rnd = [0u8; 16];
        crate::random::fill(&mut rnd); // AT_RANDOM: seeds the stack-protector canary / malloc
        let rand_addr = push(&mut cur, &rnd);
        let cstr = |cur: &mut u64, s: &str| -> u64 {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            push(cur, &b)
        };
        let arg_ptrs: Vec<u64> = argv.iter().map(|s| cstr(&mut cur, s)).collect();
        let env_ptrs: Vec<u64> = envp.iter().map(|s| cstr(&mut cur, s)).collect();
        let execfn = arg_ptrs.first().copied().unwrap_or(0);

        // auxv (type, value) pairs — AT_NULL last.
        let aux: [(u64, u64); 11] = [
            (3, img.phdr),        // AT_PHDR
            (4, img.phent),       // AT_PHENT
            (5, img.phnum),       // AT_PHNUM
            (6, 4096),            // AT_PAGESZ
            (7, img.interp_base), // AT_BASE (the interpreter's load address, 0 = none)
            (9, img.prog_entry),  // AT_ENTRY
            (17, 100),            // AT_CLKTCK
            (23, 0),              // AT_SECURE
            (25, rand_addr),      // AT_RANDOM
            (31, execfn),         // AT_EXECFN
            (0, 0),               // AT_NULL
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

// ===========================================================================
//  Tasks — the process-tree layer (pid, parent, exit status). Threads point
//  here; `Task` owns the (execve-swappable) address space.
// ===========================================================================

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
static TASKS: Mutex<BTreeMap<u64, Arc<Task>>> = Mutex::new(BTreeMap::new());
/// Woken whenever any task records its exit status — `wait4` sleeps on it
/// instead of polling.
static CHILD_EXIT: crate::wait::WaitQueue = crate::wait::WaitQueue::new();

/// The logged-in session identity, stamped onto every task created after login.
/// `(uid, name)`. Grows into the executive `Principal`.
static SESSION_UID: AtomicU64 = AtomicU64::new(0);
static SESSION_NAME: Mutex<String> = Mutex::new(String::new());

/// Record the identity resolved by `login` (see `login::establish`).
#[allow(dead_code)] // only the `interactive` build has a login flow
pub fn set_session(name: &str, uid: u32) {
    SESSION_UID.store(uid as u64, Ordering::Relaxed);
    *SESSION_NAME.lock() = name.into();
}

/// This task's user id (from the login session; 0 for tasks spawned pre-login).
pub fn current_uid() -> u32 {
    sched::current().task().map(|t| t.uid).unwrap_or(0)
}

/// This task's primary group id (see `Task::gid`).
pub fn current_gid() -> u32 {
    sched::current().task().map(|t| t.gid).unwrap_or(0)
}

/// Is the calling task a native PE image? `false` (never PE) if there is no
/// current task at all. The one place this matters: whether it's safe to
/// even *look* at the PE-only vectored-exception-handler slot (`seh.rs`) —
/// that page is only ever mapped for a PE process; reading it for a plain
/// ELF one faults.
pub fn current_is_pe() -> bool {
    sched::current().task().is_some_and(|t| t.is_pe.load(Ordering::Relaxed))
}

/// What a HANDLE / file descriptor points at. Both personalities share one
/// per-process table: a POSIX fd and a Win32 `HANDLE` are the same integer
/// into the same `Vec` — a file, or an executive object.
/// A section object's backing store: physical frames, not a plain buffer.
/// Every view any process maps of this section (`Process::map_section_view`)
/// maps these *same* frames — a write through one view is visible through
/// every other view immediately, in any process, with no copy. That's what
/// makes it "shared": the sharing happens at the MMU, not in this struct.
///
/// Anonymous (`file: None`) ⇒ frames start zeroed, no writeback target.
/// File-backed ⇒ frames start as a copy of the file's bytes, and the file is
/// kept open so [`Section::flush`] (`NtFlushVirtualMemory`, or an implicit
/// flush on `NtUnmapViewOfSection`) can write the current bytes back to it —
/// the "writeback" half.
pub struct Section {
    pub size: usize,
    frames: Vec<PhysFrame>,
    file: Option<Arc<dyn FileOps>>,
}

impl Section {
    /// Distribute `init` across freshly allocated physical frames (the tail
    /// of the last page, past `init.len()`, is zeroed). `file`, if given, is
    /// the source this section can write its current contents back to.
    pub fn new(init: &[u8], file: Option<Arc<dyn FileOps>>) -> Self {
        let pages = init.len().div_ceil(4096).max(1);
        let mut frames = Vec::with_capacity(pages);
        for i in 0..pages {
            let f = FRAME_ALLOC.lock().alloc().expect("no frame for section");
            let dst = unsafe {
                core::slice::from_raw_parts_mut(phys_to_virt(f.start_address()).as_mut_ptr::<u8>(), 4096)
            };
            let start = i * 4096;
            let end = (start + 4096).min(init.len());
            dst[..end - start].copy_from_slice(&init[start..end]);
            dst[end - start..].fill(0);
            frames.push(f);
        }
        Self { size: init.len(), frames, file }
    }

    /// The frames covering `[offset, offset+len)`, page-rounded outward.
    fn frames_for(&self, offset: usize, len: usize) -> &[PhysFrame] {
        let first = offset / 4096;
        let last = (offset + len).div_ceil(4096).min(self.frames.len());
        &self.frames[first..last]
    }

    /// Write every backing frame's bytes back to the file this section was
    /// created from. `true` (no-op) for an anonymous section — there is
    /// nothing to write back to. THOS writes the *whole* section back rather
    /// than tracking dirty pages — simpler, correct, just not incremental.
    pub fn flush(&self) -> bool {
        let Some(f) = &self.file else { return true };
        if f.seek(0, crate::file::SEEK_SET) < 0 {
            return false;
        }
        let mut remaining = self.size;
        for frame in &self.frames {
            let n = remaining.min(4096);
            let src = unsafe {
                core::slice::from_raw_parts(phys_to_virt(frame.start_address()).as_ptr::<u8>(), n)
            };
            if f.write(src) != n as i64 {
                return false;
            }
            remaining -= n;
        }
        f.sync() >= 0 // write-back files only reach the disk now
    }
}

#[derive(Clone)]
pub enum HandleObject {
    File(Arc<dyn FileOps>),
    Event(Arc<Event>),
    Semaphore(Arc<Semaphore>),
    Mutant(Arc<Mutant>),
    Section(Arc<Section>),
    /// A registry key — the canonical `\`-joined path into [`crate::registry`]'s
    /// global tree (ops re-walk under that module's lock).
    RegKey(String),
}

/// A polymorphic view of the dispatcher objects a `NtWaitFor*` call can wait on.
/// `tid` is threaded through only for the mutant's ownership check.
#[derive(Clone)]
pub enum Waitable {
    Event(Arc<Event>),
    Semaphore(Arc<Semaphore>),
    Mutant(Arc<Mutant>),
}

impl Waitable {
    /// Non-blocking: take/consume the signal if present.
    pub fn try_take(&self, tid: u64) -> bool {
        match self {
            Waitable::Event(e) => e.try_take(),
            Waitable::Semaphore(s) => s.try_take(),
            Waitable::Mutant(m) => m.try_acquire(tid),
        }
    }
    /// Block until signalled, then consume it.
    pub fn wait(&self, tid: u64) {
        match self {
            Waitable::Event(e) => e.wait(),
            Waitable::Semaphore(s) => s.wait(),
            Waitable::Mutant(m) => m.acquire(tid),
        }
    }

    /// Timed [`wait`]: fully blocking (enqueued on the object *and* the timer
    /// wheel — no yield-poll). `true` = signalled + consumed, `false` = timed
    /// out at `deadline` (a timer-wheel tick count).
    pub fn wait_until(&self, tid: u64, deadline: u64) -> bool {
        match self {
            Waitable::Event(e) => e.wait_until(deadline),
            Waitable::Semaphore(s) => s.wait_until(deadline),
            Waitable::Mutant(m) => m.acquire_until(tid, deadline),
        }
    }
    /// Would `try_take` succeed right now? (No consume.)
    pub fn is_signaled(&self, tid: u64) -> bool {
        match self {
            Waitable::Event(e) => e.is_signaled(),
            Waitable::Semaphore(s) => s.is_signaled(),
            Waitable::Mutant(m) => m.is_signaled(tid),
        }
    }

    /// The underlying wait queue — for `wait::wait_any_until`
    /// (`NtWaitForMultipleObjects`'s real multi-object block).
    pub fn queue(&self) -> &crate::wait::WaitQueue {
        match self {
            Waitable::Event(e) => e.queue(),
            Waitable::Semaphore(s) => s.queue(),
            Waitable::Mutant(m) => m.queue(),
        }
    }
}

/// One table slot: the object plus its close-on-exec flag (per-descriptor, not
/// per open-file-description).
#[derive(Clone)]
pub struct FdEntry {
    pub obj: HandleObject,
    pub cloexec: bool,
}
pub(crate) type Fd = Option<FdEntry>;

/// One queued user-mode APC (see [`crate::apc`]). `routine` is the
/// `PKNORMAL_ROUTINE`; `arg1..arg3` are `NtQueueApcThread`'s `ApcArgument1..3`
/// (NormalContext / SystemArgument1 / SystemArgument2).
#[derive(Clone, Copy)]
pub struct ApcEntry {
    pub routine: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
}

pub struct Task {
    pub pid: u64,
    pub ppid: u64,
    pub uid: u32,
    /// Primary (and, today, only) group id. THOS has no supplementary
    /// groups yet — every account is its own "user private group", gid ==
    /// uid, same convention `cred::save`'s `/home/<name>` already assumes.
    pub gid: u32,
    space: Mutex<Arc<Process>>,
    exit_status: Mutex<Option<i32>>,
    exited: AtomicBool,
    /// File descriptor table. 0/1/2 seeded with the console.
    fds: Mutex<Vec<Fd>>,
    /// Current working directory, always a normalised absolute path.
    cwd: Mutex<String>,
    /// Pending user-mode APCs, delivered when the thread next goes alertable.
    apcs: Mutex<VecDeque<ApcEntry>>,
    /// Process group and session ids (a new task leads its own group until it joins
    /// another; `fork` inherits both).
    pgid: AtomicU64,
    sid: AtomicU64,
    /// POSIX signal state: handlers, blocked mask, pending set (see `signal.rs`).
    pub sig: Mutex<crate::signal::SigState>,
    /// The task's (first) thread, so a signal can wake it out of a blocking wait.
    thread: Mutex<Option<alloc::sync::Weak<crate::sched::Thread>>>,
    /// Non-zero if the task ended because of that signal (`wait4` reports it).
    term_sig: AtomicU32,
    /// argv of the running image, NUL-separated (for `/proc/<pid>/cmdline`).
    cmdline: Mutex<Vec<u8>>,
    /// Debug: log this task's system calls (set for dynamically linked programs for now).
    pub trace: AtomicBool,
    /// `true` for a native PE image, `false` for an ELF — for a `ps` view.
    is_pe: AtomicBool,
    /// How many of this task's threads are still alive — *not* the same
    /// thing as this `Arc<Task>`'s own strong count, which stays >= 2 for
    /// the task's entire lifetime (one held by `TASKS`, permanently, until
    /// some parent `wait4`s it — many of THOS's test-spawned processes never
    /// get one). Threads are what actually run on a CR3, so this is the
    /// right signal for "is it safe to reclaim the address space now" —
    /// `sched::spawn_user`/`spawn_user_pe`/`spawn_user_frame` increment it,
    /// `sched::reap` decrements it once a thread's stack is confirmed safe
    /// to free, and reclaims the address space right when it hits zero.
    active_threads: AtomicU64,
    /// Threads that have not yet called exit (unlike `active_threads`, which counts until
    /// the corpse is reaped): the last one to exit ends the process.
    live_threads: AtomicU32,
    /// Every thread of the task (weak), so exit_group / signals can wake them all.
    threads: Mutex<Vec<alloc::sync::Weak<crate::sched::Thread>>>,
}

fn seed_fds() -> Vec<Fd> {
    let stdin: Arc<dyn FileOps> = Arc::new(KeyboardFile);
    let out: Arc<dyn FileOps> = Arc::new(ConsoleFile { writable: true });
    let e = |f: Arc<dyn FileOps>| Some(FdEntry { obj: HandleObject::File(f), cloexec: false });
    alloc::vec![e(stdin), e(out.clone()), e(out)]
}

impl Task {
    fn new(ppid: u64, space: Arc<Process>) -> Arc<Self> {
        let uid = SESSION_UID.load(Ordering::Relaxed) as u32;
        Self::new_with_ids(ppid, space, uid, uid)
    }

    /// Like `new`, but with an explicit uid/gid instead of inheriting the
    /// session's — the primitive behind `elevate()`: a process that is
    /// privileged from the moment it starts, not one that started
    /// unprivileged and had its token upgraded in place (THOS has no
    /// in-place token upgrade — a fresh process is the only way a task ever
    /// becomes uid 0).
    fn new_with_ids(ppid: u64, space: Arc<Process>, uid: u32, gid: u32) -> Arc<Self> {
        let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let t = Arc::new(Self {
            pid,
            ppid,
            uid,
            gid,
            space: Mutex::new(space),
            exit_status: Mutex::new(None),
            exited: AtomicBool::new(false),
            fds: Mutex::new(seed_fds()),
            cwd: Mutex::new(String::from("/")),
            apcs: Mutex::new(VecDeque::new()),
            pgid: AtomicU64::new(pid),
            sid: AtomicU64::new(pid),
            sig: Mutex::new(crate::signal::SigState::new()),
            thread: Mutex::new(None),
            term_sig: AtomicU32::new(0),
            cmdline: Mutex::new(Vec::new()),
            trace: AtomicBool::new(false),
            is_pe: AtomicBool::new(false),
            active_threads: AtomicU64::new(0),
            live_threads: AtomicU32::new(0),
            threads: Mutex::new(Vec::new()),
        });
        TASKS.lock().insert(t.pid, t.clone());
        t
    }

    pub fn cmdline(&self) -> Vec<u8> {
        self.cmdline.lock().clone()
    }
    pub fn set_cmdline<S: AsRef<str>>(&self, argv: &[S]) {
        let mut v = Vec::new();
        for a in argv {
            v.extend_from_slice(a.as_ref().as_bytes());
            v.push(0);
        }
        *self.cmdline.lock() = v;
    }
    pub fn pgid(&self) -> u64 {
        self.pgid.load(Ordering::Relaxed)
    }
    pub fn sid(&self) -> u64 {
        self.sid.load(Ordering::Relaxed)
    }
    pub fn set_pgid(&self, v: u64) {
        self.pgid.store(v, Ordering::Relaxed);
    }
    pub fn set_sid(&self, v: u64) {
        self.sid.store(v, Ordering::Relaxed);
    }
    /// Remember the task's thread (weakly: the thread owns an `Arc<Task>`).
    pub fn set_thread(&self, t: alloc::sync::Weak<crate::sched::Thread>) {
        self.threads.lock().push(t.clone());
        let mut first = self.thread.lock();
        if first.as_ref().map_or(true, |w| w.upgrade().is_none()) {
            *first = Some(t);
        }
    }
    pub fn thread(&self) -> Option<Arc<crate::sched::Thread>> {
        self.thread.lock().as_ref().and_then(|w| w.upgrade())
    }
    pub fn term_sig(&self) -> u32 {
        self.term_sig.load(Ordering::Relaxed)
    }
    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    pub fn mark_pe(&self) {
        self.is_pe.store(true, Ordering::Relaxed);
    }

    pub fn space(&self) -> Arc<Process> {
        self.space.lock().clone()
    }

    /// Install `new`, returning the *old* space still alive (not dropped
    /// here) — unlike `set_space`, so a caller that is still running on the
    /// old space's CR3 (`execve`, until its own `Cr3::write` a few
    /// instructions later) controls exactly when it becomes safe to free.
    fn swap_space(&self, new: Arc<Process>) -> Arc<Process> {
        core::mem::replace(&mut *self.space.lock(), new)
    }

    /// Record that a new thread of this task just started (`sched::spawn_user`
    /// / `spawn_user_pe` / `spawn_user_frame` — every path that creates a
    /// `Thread` bound to this `Task`, the initial one included).
    pub(crate) fn thread_spawned(&self) {
        self.active_threads.fetch_add(1, Ordering::AcqRel);
        self.live_threads.fetch_add(1, Ordering::AcqRel);
    }

    /// One of this task's threads is leaving (called by the thread itself). `true` if it was
    /// the last live one, i.e. the whole process is ending.
    pub fn thread_leaving(&self) -> bool {
        self.live_threads.fetch_sub(1, Ordering::AcqRel) == 1
    }

    /// Wake every thread of the task (it is exiting, or a signal is pending).
    pub fn wake_all_threads(&self) {
        let ts: Vec<Arc<crate::sched::Thread>> = self.threads.lock().iter().filter_map(|w| w.upgrade()).collect();
        for t in ts {
            sched::unblock(t);
        }
    }

    /// Record that one of this task's threads is confirmed gone (`sched::reap`,
    /// right as it is about to free that thread's kernel stack — i.e. once
    /// nothing is running on it anywhere). `true` if that was the *last* one —
    /// the task's address space is then provably safe to reclaim: whatever CR3
    /// its threads used cannot be loaded on any CPU with none of them left.
    pub(crate) fn thread_exited(&self) -> bool {
        self.active_threads.fetch_sub(1, Ordering::AcqRel) == 1
    }

    /// Reclaim this task's address-space frames — called once `thread_exited`
    /// reports the last thread gone. The `Arc<Process>` strong-count check is
    /// still a belt-and-suspenders guard, not the trigger: a `Task`'s own
    /// strong count stays >= 2 for its whole life (one held by `TASKS`
    /// permanently — many of THOS's test-spawned processes are never
    /// `wait4`'d — one by this thread), so it was never a usable signal for
    /// "safe to free"; this only skips the rare case something else (a
    /// concurrent `execve` mid-swap, a `ps` snapshot) holds a temporary extra
    /// clone of the `Process` itself at this exact moment.
    pub(crate) fn teardown_space_if_unreferenced(&self) {
        let space = self.space.lock();
        if Arc::strong_count(&space) == 1 {
            space.teardown();
        }
    }

    pub fn fd_get(&self, fd: i32) -> Option<Arc<dyn FileOps>> {
        match &self.fds.lock().get(fd as usize)?.as_ref()?.obj {
            HandleObject::File(f) => Some(f.clone()),
            _ => None,
        }
    }

    /// The `Event` a HANDLE names, if it is one.
    pub fn handle_event(&self, h: i32) -> Option<Arc<Event>> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::Event(e) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn fd_alloc(&self, file: Arc<dyn FileOps>) -> i32 {
        self.fd_alloc_flags(file, false)
    }
    pub fn fd_alloc_flags(&self, file: Arc<dyn FileOps>, cloexec: bool) -> i32 {
        self.handle_alloc(HandleObject::File(file), cloexec)
    }
    pub fn handle_alloc_event(&self, ev: Arc<Event>) -> i32 {
        self.handle_alloc(HandleObject::Event(ev), false)
    }
    pub fn handle_alloc_semaphore(&self, s: Arc<Semaphore>) -> i32 {
        self.handle_alloc(HandleObject::Semaphore(s), false)
    }
    pub fn handle_alloc_mutant(&self, m: Arc<Mutant>) -> i32 {
        self.handle_alloc(HandleObject::Mutant(m), false)
    }
    pub fn handle_alloc_section(&self, s: Arc<Section>) -> i32 {
        self.handle_alloc(HandleObject::Section(s), false)
    }
    pub fn handle_section(&self, h: i32) -> Option<Arc<Section>> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::Section(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The dispatcher object a HANDLE names, as a [`Waitable`], if it is one.
    pub fn handle_waitable(&self, h: i32) -> Option<Waitable> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::Event(e) => Some(Waitable::Event(e.clone())),
            HandleObject::Semaphore(s) => Some(Waitable::Semaphore(s.clone())),
            HandleObject::Mutant(m) => Some(Waitable::Mutant(m.clone())),
            _ => None,
        }
    }

    /// The registry-key path a HANDLE names, if it is one.
    pub fn handle_regkey(&self, h: i32) -> Option<String> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::RegKey(p) => Some(p.clone()),
            _ => None,
        }
    }
    pub fn handle_alloc_regkey(&self, path: String) -> i32 {
        self.handle_alloc(HandleObject::RegKey(path), false)
    }

    /// Append a user APC to this task's queue.
    pub fn apc_queue(&self, e: ApcEntry) {
        self.apcs.lock().push_back(e);
    }
    /// Dequeue the oldest pending user APC, if any.
    pub fn apc_take(&self) -> Option<ApcEntry> {
        self.apcs.lock().pop_front()
    }
    /// `true` if at least one user APC is queued.
    pub fn apc_pending(&self) -> bool {
        !self.apcs.lock().is_empty()
    }

    /// Install `obj` at the lowest free descriptor, with the given cloexec flag.
    pub fn handle_alloc(&self, obj: HandleObject, cloexec: bool) -> i32 {
        let mut fds = self.fds.lock();
        let entry = Some(FdEntry { obj, cloexec });
        match fds.iter().position(|f| f.is_none()) {
            Some(i) => {
                fds[i] = entry;
                i as i32
            }
            None => {
                fds.push(entry);
                (fds.len() - 1) as i32
            }
        }
    }

    pub fn fd_close(&self, fd: i32) -> bool {
        let mut fds = self.fds.lock();
        match fds.get_mut(fd as usize) {
            Some(slot @ Some(_)) => {
                *slot = None;
                true
            }
            _ => false,
        }
    }

    /// `F_GETFD` / `F_SETFD` on the close-on-exec flag. Returns `0`/`1`, or
    /// `-EBADF`.
    pub fn fd_get_cloexec(&self, fd: i32) -> i32 {
        match self.fds.lock().get(fd as usize).and_then(|f| f.as_ref()) {
            Some(e) => e.cloexec as i32,
            None => -9,
        }
    }
    pub fn fd_set_cloexec(&self, fd: i32, on: bool) -> i32 {
        match self.fds.lock().get_mut(fd as usize).and_then(|f| f.as_mut()) {
            Some(e) => {
                e.cloexec = on;
                0
            }
            None => -9,
        }
    }

    /// execve: drop every descriptor marked close-on-exec.
    fn close_on_exec(&self) {
        for slot in self.fds.lock().iter_mut() {
            if slot.as_ref().map_or(false, |e| e.cloexec) {
                *slot = None;
            }
        }
    }

    /// Duplicate `oldfd` to the lowest free descriptor at or above `min`. The
    /// new descriptor never inherits close-on-exec (`F_DUPFD` semantics).
    pub fn fd_dup(&self, oldfd: i32, min: i32) -> i32 {
        let mut fds = self.fds.lock();
        let Some(Some(mut entry)) = fds.get(oldfd as usize).cloned() else {
            return -9; // EBADF
        };
        entry.cloexec = false;
        let min = min.max(0) as usize;
        if let Some(i) = (min..fds.len()).find(|&i| fds[i].is_none()) {
            fds[i] = Some(entry);
            return i as i32;
        }
        while fds.len() < min {
            fds.push(None);
        }
        fds.push(Some(entry));
        (fds.len() - 1) as i32
    }

    /// `dup2` / `dup3`: force `newfd` to refer to `oldfd`'s file (closing
    /// whatever was there). `cloexec` sets the new descriptor's flag (always
    /// `false` for `dup2`). Returns `newfd`, or `-EBADF` if `oldfd` is invalid.
    pub fn fd_dup2(&self, oldfd: i32, newfd: i32) -> i32 {
        self.fd_dup3(oldfd, newfd, false)
    }
    pub fn fd_dup3(&self, oldfd: i32, newfd: i32, cloexec: bool) -> i32 {
        let mut fds = self.fds.lock();
        let Some(Some(mut entry)) = fds.get(oldfd as usize).cloned() else {
            return -9;
        };
        if oldfd == newfd {
            return newfd; // POSIX: dup2 no-op keeps the fd (dup3 would EINVAL)
        }
        entry.cloexec = cloexec;
        let n = newfd.max(0) as usize;
        while fds.len() <= n {
            fds.push(None);
        }
        fds[n] = Some(entry);
        newfd
    }

    /// fork inherits the parent's open files (shared, like POSIX).
    fn clone_fds(&self) -> Vec<Fd> {
        self.fds.lock().clone()
    }

    pub fn cwd(&self) -> String {
        self.cwd.lock().clone()
    }
    pub fn set_cwd(&self, path: String) {
        *self.cwd.lock() = path;
    }
}

/// The current task's working directory (`"/"` if there is no task).
pub fn current_cwd() -> String {
    sched::current().task().map(|t| t.cwd()).unwrap_or_else(|| String::from("/"))
}

/// Store an already-normalised absolute path as the current task's cwd.
pub fn set_current_cwd(path: String) {
    if let Some(t) = sched::current().task() {
        t.set_cwd(path);
    }
}

/// Resolve `path` against the current task's cwd into a clean absolute path:
/// `.` is dropped, `..` pops a component (never past `/`), and repeated or
/// trailing slashes collapse. No symlink following (we have no symlinks).
pub fn resolve_path(path: &str) -> String {
    let base = if path.starts_with('/') { String::new() } else { current_cwd() };
    let mut comps: Vec<&str> = Vec::new();
    for part in base.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            p => comps.push(p),
        }
    }
    let mut out = String::from("/");
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(c);
    }
    out
}

pub fn user_selectors() -> (u64, u64) {
    let s = gdt::selectors();
    ((s.user_code.0 | 3) as u64, (s.user_data.0 | 3) as u64)
}

/// The current task's file object for `fd`, if open.
pub fn current_fd(fd: i32) -> Option<Arc<dyn FileOps>> {
    sched::current().task().and_then(|t| t.fd_get(fd))
}

/// The current task's `Event` for HANDLE `h`, if it names one.
pub fn current_event(h: i32) -> Option<Arc<Event>> {
    sched::current().task().and_then(|t| t.handle_event(h))
}

/// Install `ev` in the current task's HANDLE table; returns the HANDLE, or -1.
pub fn current_alloc_event(ev: Arc<Event>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_event(ev))
}
pub fn current_alloc_semaphore(s: Arc<Semaphore>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_semaphore(s))
}
pub fn current_alloc_mutant(m: Arc<Mutant>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_mutant(m))
}
pub fn current_alloc_section(s: Arc<Section>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_section(s))
}
pub fn current_section(h: i32) -> Option<Arc<Section>> {
    sched::current().task().and_then(|t| t.handle_section(h))
}

/// The current task's [`Waitable`] for HANDLE `h`, if it names a dispatcher
/// object (event / semaphore / mutant).
pub fn current_waitable(h: i32) -> Option<Waitable> {
    sched::current().task().and_then(|t| t.handle_waitable(h))
}

/// The current task's registry-key path for HANDLE `h`, if it names one.
pub fn current_regkey(h: i32) -> Option<String> {
    sched::current().task().and_then(|t| t.handle_regkey(h))
}

/// Install a registry-key HANDLE (by canonical path) in the current task.
pub fn current_alloc_regkey(path: String) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_regkey(path))
}

/// Queue a user APC on the current task; `false` if there is no current task.
pub fn current_queue_apc(e: ApcEntry) -> bool {
    match sched::current().task() {
        Some(t) => {
            t.apc_queue(e);
            true
        }
        None => false,
    }
}

/// Dequeue one pending user APC for the current task.
pub fn current_take_apc() -> Option<ApcEntry> {
    sched::current().task().and_then(|t| t.apc_take())
}

/// `true` if the current task has a user APC queued — the alertable
/// `NtWaitForSingleObject` short-circuit (`nt.rs`) checks this before
/// blocking.
pub fn current_apc_pending() -> bool {
    sched::current().task().map(|t| t.apc_pending()).unwrap_or(false)
}

/// This thread's id (distinct per thread within a process — unlike
/// [`current_pid`], which is the shared `Task` id).
pub fn current_tid() -> u64 {
    sched::current().id
}

/// Per-worker-thread exit events, keyed by thread id. A thread's
/// `NtCreateThreadEx` registers one; `NtTerminateThread` signals + removes it,
/// so a `NtWaitForSingleObject` on the returned handle completes on exit.
static THREAD_EXITS: Mutex<BTreeMap<u64, Arc<Event>>> = Mutex::new(BTreeMap::new());

pub fn register_thread_exit(tid: u64, ev: Arc<Event>) {
    THREAD_EXITS.lock().insert(tid, ev);
}

/// The ring-3 callback mechanism's save stack (`CallWindowProcA` /
/// `NtCallbackReturn` — see `nt::dispatch_user32`): the syscall frame a
/// thread was in when it asked the kernel to call back into ring-3 code, so
/// `NtCallbackReturn` can resume *that* context (with the callback's result
/// in `rax`) instead of the trampoline that invoked it. A `Vec` per thread,
/// not just one slot, so a callback that itself triggers another callback
/// nests correctly (LIFO, matching real call/return order).
static CALLBACK_FRAMES: Mutex<BTreeMap<u64, Vec<crate::syscall::UserFrame>>> =
    Mutex::new(BTreeMap::new());

/// Stash `frame` before diverging into a ring-3 callback.
pub fn push_callback_frame(tid: u64, frame: crate::syscall::UserFrame) {
    CALLBACK_FRAMES.lock().entry(tid).or_default().push(frame);
}

/// Pop the most recently stashed frame for `tid` (`NtCallbackReturn`'s doing
/// the popping) — `None` if the thread has no callback in flight (a stray or
/// duplicate `NtCallbackReturn`).
pub fn pop_callback_frame(tid: u64) -> Option<crate::syscall::UserFrame> {
    let mut frames = CALLBACK_FRAMES.lock();
    let stack = frames.get_mut(&tid)?;
    let f = stack.pop();
    if stack.is_empty() {
        frames.remove(&tid);
    }
    f
}

/// Signal + forget `tid`'s exit event. `false` if none was registered (i.e. the
/// caller is the process's original thread).
pub fn signal_thread_exit(tid: u64) -> bool {
    match THREAD_EXITS.lock().remove(&tid) {
        Some(ev) => {
            ev.signal();
            true
        }
        None => false,
    }
}

/// A `ps`-style dump of every spawned task (ELF and PE alike), for the
/// Milestone 3 check that both personalities show up in one listing.
#[allow(dead_code)] // only the `petest` milestone calls this
pub fn ps_dump() {
    let (mut elf, mut pe) = (0u32, 0u32);
    crate::kprintln!("THOS: ps    PID PPID KIND STATE");
    for (pid, t) in TASKS.lock().iter() {
        let is_pe = t.is_pe.load(Ordering::Relaxed);
        if is_pe {
            pe += 1;
        } else {
            elf += 1;
        }
        let kind = if is_pe { "PE " } else { "ELF" };
        let state = if t.exited.load(Ordering::Relaxed) { "exited" } else { "run" };
        crate::kprintln!("THOS: ps    {:>3} {:>4} {}  {}", pid, t.ppid, kind, state);
    }
    crate::kprintln!("THOS: ps ok  {} ELF + {} PE processes in one listing", elf, pe);
}

pub fn current_pid() -> u64 {
    sched::current().task().map(|t| t.pid).unwrap_or(0)
}

pub fn current_ppid() -> u64 {
    sched::current().task().map(|t| t.ppid).unwrap_or(0)
}

/// `dup` / `dup2` / `dup3` / `fcntl` on the current task's fd table.
pub fn current_fd_dup(oldfd: i32, min: i32) -> i32 {
    sched::current().task().map(|t| t.fd_dup(oldfd, min)).unwrap_or(-9)
}
pub fn current_fd_dup2(oldfd: i32, newfd: i32) -> i32 {
    sched::current().task().map(|t| t.fd_dup2(oldfd, newfd)).unwrap_or(-9)
}
pub fn current_fd_dup3(oldfd: i32, newfd: i32, cloexec: bool) -> i32 {
    sched::current().task().map(|t| t.fd_dup3(oldfd, newfd, cloexec)).unwrap_or(-9)
}
pub fn current_fd_get_cloexec(fd: i32) -> i32 {
    sched::current().task().map(|t| t.fd_get_cloexec(fd)).unwrap_or(-9)
}
pub fn current_fd_set_cloexec(fd: i32, on: bool) -> i32 {
    sched::current().task().map(|t| t.fd_set_cloexec(fd, on)).unwrap_or(-9)
}

/// Record an exit status on the current task (called from `exit`/`exit_group`).
///
/// A zombie holds no open files: drop the fd table now so the other end of any
/// pipe sees EOF/`EPIPE` immediately, instead of only once `wait4` reaps the
/// `Task` out of the `TASKS` map.
pub fn set_exit_status(code: i32) {
    if let Some(t) = sched::current().task() {
        *t.exit_status.lock() = Some(code);
        // Take the table out before dropping it: closing the files can write to disk.
        let old = core::mem::take(&mut *t.fds.lock());
        drop(old);
        t.exited.store(true, Ordering::Release);
        notify_parent(&t);
        t.wake_all_threads(); // the other threads see `exited` and die
    }
    CHILD_EXIT.wake_all(); // an interested parent may be blocked in wait4
}

/// End the current task because of fatal signal `sig` (`wait4` reports
/// `WIFSIGNALED` / `WTERMSIG`, which shells print as "Terminated" etc.).
pub fn set_term_signal(sig: u32) {
    if let Some(t) = sched::current().task() {
        t.term_sig.store(sig, Ordering::Relaxed);
        crate::kprintln!("THOS: pid {} killed by signal {}", t.pid, sig);
    }
    set_exit_status(128 + sig as i32);
}

/// `SIGCHLD` to the parent when a child ends (default action: ignore, but a
/// handler — or an interruptible `wait4` — sees it).
fn notify_parent(t: &Task) {
    if t.ppid != 0 {
        if let Some(p) = find_task(t.ppid) {
            crate::signal::send(&p, crate::signal::SIGCHLD);
        }
    }
}

/// The live task with this pid.
pub fn find_task(pid: u64) -> Option<Arc<Task>> {
    TASKS.lock().get(&pid).filter(|t| !t.is_exited()).cloned()
}

/// Every live task of process group `pgid`.
pub fn tasks_in_pgrp(pgid: u64) -> Vec<Arc<Task>> {
    TASKS.lock().values().filter(|t| !t.is_exited() && t.pgid() == pgid).cloned().collect()
}

/// Every task that has not exited.
pub fn all_live_tasks() -> Vec<Arc<Task>> {
    TASKS.lock().values().filter(|t| !t.is_exited()).cloned().collect()
}

/// Predicate for `wait4` to sleep on: this task has a matching live child and
/// none of its matching children has exited yet.
fn should_block_in_wait4(me: u64, pid: i64) -> bool {
    let tasks = TASKS.lock();
    let mut has_child = false;
    for t in tasks.values() {
        if t.ppid == me && (pid == -1 || t.pid == pid as u64) {
            has_child = true;
            if t.exited.load(Ordering::Acquire) {
                return false;
            }
        }
    }
    has_child
}

/// The primitive behind `elevate()` (`syscall::sys_elevate` does the
/// re-authentication and admin-only policy check *before* calling this):
/// spawn `bytes` as a brand-new process with an explicit uid/gid instead of
/// the caller's session identity. Scoped to exactly that one process —
/// there is no elevated token or elevated shell that outlives it or that a
/// later, unrelated action could reuse; the next privileged action needs
/// its own `elevate` call. Goes through the same native-exec gate every
/// other entry point into the system does.
#[cfg_attr(not(feature = "interactive"), allow(dead_code))] // only sys_elevate (interactive-only: needs cred.rs) calls this
pub fn spawn_elevated(ppid: u64, bytes: &[u8], argv: &[&str], envp: &[&str], uid: u32, gid: u32) -> Result<u64, &'static str> {
    if let crate::execgate::Verdict::Quarantine(reason) = crate::execgate::check(bytes) {
        crate::kprintln!("THOS: exec gate        quarantined an elevated exec — {reason}");
        return Err("quarantined by the native-exec gate");
    }
    let space = Process::new();
    let img = elf::load(&space, bytes)?;
    let stack_top = space.new_user_stack();
    let rsp = space.init_stack(stack_top, argv, envp, &img);
    let task = Task::new_with_ids(ppid, space, uid, gid);
    task.set_cmdline(argv);
    sched::spawn_user("elevated", task.clone(), img.entry, rsp);
    Ok(task.pid)
}

/// Like [`spawn_elevated`], but the new task's fd table is `fds` instead of
/// the usual console-backed `seed_fds()` — the primitive behind spawning
/// the Security Service (`secsvc.rs`) with its stdin/stdout wired to the
/// kernel<->service pipes instead of the console, so nothing it prints or
/// reads is visible to (or forgeable by) any other process.
pub fn spawn_with_fds(
    ppid: u64,
    bytes: &[u8],
    argv: &[&str],
    envp: &[&str],
    uid: u32,
    gid: u32,
    fds: Vec<Fd>,
) -> Result<u64, &'static str> {
    if let crate::execgate::Verdict::Quarantine(reason) = crate::execgate::check(bytes) {
        crate::kprintln!("THOS: exec gate        quarantined a spawn_with_fds exec — {reason}");
        return Err("quarantined by the native-exec gate");
    }
    let space = Process::new();
    let img = elf::load(&space, bytes)?;
    let stack_top = space.new_user_stack();
    let rsp = space.init_stack(stack_top, argv, envp, &img);
    let task = Task::new_with_ids(ppid, space, uid, gid);
    *task.fds.lock() = fds;
    sched::spawn_user("secsvc", task.clone(), img.entry, rsp);
    Ok(task.pid)
}

/// `spawn` the initial user program: build its address space + entry stack and
/// hand it to the scheduler. Returns the pid.
pub fn spawn_init(bytes: &[u8], argv: &[&str], envp: &[&str]) -> u64 {
    let space = Process::new();
    let img = elf::load(&space, bytes).expect("spawn_init: bad ELF");
    let stack_top = space.new_user_stack();
    let rsp = space.init_stack(stack_top, argv, envp, &img);
    let task = Task::new(0, space);
    task.set_cmdline(argv);
    sched::spawn_user("init", task.clone(), img.entry, rsp);
    task.pid
}

/// `spawn` a statically linked Win64 `.exe`: map the PE and enter its entry
/// point in ring 3 with a bare 16-aligned stack (no SysV block — Win64 entry
/// points take no stack args). Returns the pid. The NT personality (PEB/TEB,
/// `gs` base) is layered on later; a self-contained `.exe` that only makes
/// syscalls runs on this alone.
#[allow(dead_code)] // only the `petest` milestone calls this so far
pub fn spawn_pe(bytes: &[u8]) -> Result<u64, &'static str> {
    // The native-exec gate: every program entering the system passes the
    // hash/signature check + policy engine before `pe::load` ever parses a
    // header. Same rejection shape as a malformed PE — `Err`, kernel alive.
    if let crate::execgate::Verdict::Quarantine(reason) = crate::execgate::check(bytes) {
        crate::kprintln!("THOS: exec gate        quarantined a PE — {reason}");
        return Err("quarantined by the native-exec gate");
    }
    let space = Process::new();
    let stack_top = space.new_user_stack();
    let img = crate::pe::load(&space, bytes, stack_top)?; // malformed .exe -> Err, never panic
    // MS x64 ABI: at the entry instruction RSP+8 must be 16-aligned.
    let rsp = (stack_top & !0xF) - 8;
    let task = Task::new(0, space);
    task.mark_pe();
    sched::spawn_user_pe("pe", task.clone(), img.entry, rsp, img.teb);
    Ok(task.pid)
}

/// `fork`: eager (non-COW) copy of the caller's address space; the child
/// resumes at the same user instruction with `rax = 0`.
pub fn fork(frame: &UserFrame) -> i64 {
    let parent = sched::current().task().expect("fork: not a user task");
    let pspace = parent.space();

    let cspace = Process::new();
    cspace.copy_alloc_state_from(&pspace);
    pspace.for_each_user_page(|virt, phys, w, x, device| {
        if device {
            // device memory is shared, never copied (and never freed by either process)
            vmm::map_device_page_in(cspace.pml4_phys, virt, phys, w);
            return;
        }
        let f = FRAME_ALLOC.lock().alloc().expect("fork: no frame");
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>(),
                phys_to_virt(f.start_address()).as_mut_ptr::<u8>(),
                4096,
            );
        }
        cspace.map(virt, f.start_address().as_u64(), w, x);
    });

    let child = Task::new(parent.pid, cspace);
    *child.fds.lock() = parent.clone_fds();
    child.set_cwd(parent.cwd());
    *child.cmdline.lock() = parent.cmdline();
    child.set_pgid(parent.pgid());
    child.set_sid(parent.sid());
    {
        // Handlers and the blocked mask are inherited; pending signals are not.
        let p = parent.sig.lock();
        let mut c = child.sig.lock();
        c.actions = p.actions;
        c.blocked = p.blocked;
    }
    let (cs, ss) = user_selectors();
    let mut cf = *frame;
    cf.rax = 0;
    cf.cs = cs;
    cf.ss = ss;
    // The child inherits the parent thread's TLS base — it is CPU state, not
    // memory, so copying the address space alone does not carry it over.
    let fsbase = sched::current().fsbase();
    sched::spawn_user_frame("fork-child", child.clone(), cf, fsbase);
    child.pid as i64
}

/// `execve`: replace the current task's image. Does not return on success.
pub fn execve(bytes: &[u8], argv: &[String], envp: &[String]) -> ! {
    // The native-exec gate: same check `spawn_pe` runs, here for the path a
    // *running* process takes to become a different program. A malformed
    // image already can't panic the kernel past this point (`elf::load`
    // below), but there's no `Result` to hand back through `execve`'s own
    // ABI (the calling thread's image is what's being replaced) — quarantine
    // ends the calling thread cleanly instead, exit code 126 (the shell
    // convention for "found but not executable"), kernel alive either way.
    if let crate::execgate::Verdict::Quarantine(reason) = crate::execgate::check(bytes) {
        crate::kprintln!("THOS: exec gate        quarantined an ELF — {reason}");
        set_exit_status(126);
        crate::syscall::note_user_exit();
        sched::exit();
    }

    let cur = sched::current();
    let task = cur.task().expect("execve: not a user task");

    let space = Process::new();
    let img = elf::load(&space, bytes).expect("execve: bad ELF");
    let stack_top = space.new_user_stack();
    let av: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let ev: Vec<&str> = envp.iter().map(|s| s.as_str()).collect();
    let rsp = space.init_stack(stack_top, &av, &ev, &img);

    task.close_on_exec(); // drop O_CLOEXEC fds before the new image sees them
    task.sig.lock().reset_for_exec(); // caught signals go back to default; ignored stay ignored
    task.set_cmdline(&av);

    let new_cr3 = space.pml4_phys();
    // `swap_space`, not `set_space`: this thread is still running on the
    // *old* space's CR3 for a few more instructions (the actual register
    // switch is the explicit `Cr3::write` below) — dropping the old
    // `Process` here, before that, would free its page tables out from
    // under the very code currently executing off them.
    let old_space = task.swap_space(space);
    cur.set_cr3(new_cr3);
    cur.set_fsbase(0); // fresh image: TLS is re-established by its own arch_prctl

    let (cs, ss) = user_selectors();
    let f = UserFrame {
        rip: img.entry,
        rsp,
        rflags: 0x202,
        cs,
        ss,
        ..Default::default()
    };

    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(0));
    unsafe {
        Cr3::write(
            PhysFrame::from_start_address(PhysAddr::new(new_cr3)).unwrap(),
            Cr3Flags::empty(),
        );
        // This CPU is off the old space now — safe to reclaim it, provided
        // nothing else still references it (a concurrent `wait4`/`ps`
        // snapshot could in principle hold a clone; if so, just leave it —
        // whoever does hold the last reference will drop it in turn).
        if Arc::strong_count(&old_space) == 1 {
            old_space.teardown();
        }
        syscall::thos_user_resume(&f)
    }
}

/// `wait4`: poll for a zombie child (poll + yield; blocking wait comes later).
/// Has task `pid` exited? (`true` also if it has already been reaped away.)
/// For kernel-side milestones that spawn a process and want to await *that*
/// process, not just the first user thread to exit.
#[allow(dead_code)] // only the `pipetest` milestone calls this
pub fn pid_exited(pid: u64) -> bool {
    TASKS.lock().get(&pid).map_or(true, |t| t.exited.load(Ordering::Acquire))
}

pub fn wait4(pid: i64, status_ptr: u64) -> i64 {
    let me = current_pid();
    loop {
        {
            let mut tasks = TASKS.lock();
            let hit = tasks
                .values()
                .find(|t| {
                    t.ppid == me
                        && t.exited.load(Ordering::Acquire)
                        && (pid == -1 || t.pid == pid as u64)
                })
                .map(|t| (t.pid, t.exit_status.lock().unwrap_or(0), t.term_sig()));
            if let Some((cpid, status, tsig)) = hit {
                tasks.remove(&cpid);
                drop(tasks);
                if status_ptr != 0 {
                    // The child is already reaped; a bad pointer only costs the
                    // caller its status word, never kernel memory. Killed by a
                    // signal: the signal number in the low bits; else exit code << 8.
                    let word = if tsig != 0 { tsig } else { ((status & 0xFF) << 8) as u32 };
                    let _ = crate::usercopy::write_u32(status_ptr, word);
                }
                return cpid as i64;
            }
            let has_children = tasks
                .values()
                .any(|t| t.ppid == me && (pid == -1 || t.pid == pid as u64));
            if !has_children {
                return -10; // ECHILD
            }
        }
        if crate::signal::interrupted() {
            return -4; // EINTR
        }
        CHILD_EXIT.wait_if_intr(|| should_block_in_wait4(me, pid));
    }
}


// ---------------------------------------------------------------------------
//  POSIX threads (clone(CLONE_THREAD))
// ---------------------------------------------------------------------------

/// Per-thread data of threads created by `clone`: scheduler thread id -> (tid, clear_child_tid).
static THREAD_INFO: Mutex<BTreeMap<u64, (u64, u64)>> = Mutex::new(BTreeMap::new());
/// tid -> task, for `tkill` / `tgkill` on secondary threads.
static TID_TASKS: Mutex<BTreeMap<u64, alloc::sync::Weak<Task>>> = Mutex::new(BTreeMap::new());

/// Called by the scheduler before a new thread can run.
pub fn register_thread(sched_id: u64, tid: u64, clear_tid: u64, task: &Arc<Task>) {
    THREAD_INFO.lock().insert(sched_id, (tid, clear_tid));
    TID_TASKS.lock().insert(tid, Arc::downgrade(task));
}

/// The caller's POSIX thread id (the process id for the main thread). Not
/// [`current_tid`], which is the scheduler's id that the NT personality keys things by.
pub fn posix_tid() -> u64 {
    let cur = sched::current();
    if let Some(&(tid, _)) = THREAD_INFO.lock().get(&cur.id) {
        return tid;
    }
    cur.task().map(|t| t.pid).unwrap_or(0)
}

/// `set_tid_address`: remember where to clear the tid when this thread exits; returns the tid.
pub fn set_clear_tid(addr: u64) -> u64 {
    let cur = sched::current();
    let tid = posix_tid();
    THREAD_INFO.lock().insert(cur.id, (tid, addr));
    tid
}

/// The task a thread id belongs to (a pid, or a secondary thread's tid).
pub fn task_of_tid(tid: u64) -> Option<Arc<Task>> {
    if let Some(t) = find_task(tid) {
        return Some(t);
    }
    TID_TASKS.lock().get(&tid).and_then(|w| w.upgrade()).filter(|t| !t.is_exited())
}

/// The calling thread is ending: clear its `clear_child_tid` word (waking a joiner) and drop
/// its bookkeeping.
pub fn thread_exit_cleanup() {
    let cur = sched::current();
    let info = THREAD_INFO.lock().remove(&cur.id);
    if let Some((tid, clear)) = info {
        TID_TASKS.lock().remove(&tid);
        crate::futex::thread_cleared(clear);
    }
}

/// `clone` with `CLONE_THREAD`: a new thread in the caller's task (same address space, fds,
/// signal state) starting in the caller's context with `rax = 0` on `stack`.
pub fn clone_thread(frame: &UserFrame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> i64 {
    const CLONE_SETTLS: u64 = 0x80000;
    const CLONE_PARENT_SETTID: u64 = 0x100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x200000;
    const CLONE_CHILD_SETTID: u64 = 0x1000000;
    let cur = sched::current();
    let Some(task) = cur.task() else { return -22 };
    let tid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    if flags & CLONE_PARENT_SETTID != 0 && ptid != 0 {
        if let Err(e) = crate::usercopy::write_u32(ptid, tid as u32) {
            return e;
        }
    }
    if flags & CLONE_CHILD_SETTID != 0 && ctid != 0 {
        if let Err(e) = crate::usercopy::write_u32(ctid, tid as u32) {
            return e;
        }
    }
    let (cs, ss) = user_selectors();
    let mut cf = *frame;
    cf.rax = 0;
    cf.cs = cs;
    cf.ss = ss;
    if stack != 0 {
        cf.rsp = stack;
    }
    let fsbase = if flags & CLONE_SETTLS != 0 { tls } else { cur.fsbase() };
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 { ctid } else { 0 };
    sched::spawn_user_thread(task, cf, fsbase, tid, clear);
    tid as i64
}
