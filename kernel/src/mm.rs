// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — memory bring-up.
//!
//! Two pieces, both minimal on purpose:
//!   * [`FrameAllocator`] — a free-list of physical 4 KiB frames, threaded through
//!     the frames themselves via Limine's higher-half direct map (HHDM). No side
//!     table.
//!   * a bootstrap kernel heap — a fixed static arena handed to
//!     `linked_list_allocator`, so `alloc` (Box/Vec) works before we do any of
//!     our own page-table management. Grown properly in a later step.
//!
//! Paging (our own page tables, `vmspace` objects) and per-CPU allocators come
//! after the trap/IRQ scaffold.

use core::sync::atomic::{AtomicU64, Ordering};

use limine::memmap::{Entry, MEMMAP_USABLE};
use linked_list_allocator::LockedHeap;
use spin::Mutex;
use x86_64::structures::paging::{FrameDeallocator, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

const FRAME_SIZE: u64 = 4096;

/// Bootstrap heap: 8 MiB static arena. Enough through early Phase 2;
/// bookkeeping; replaced by a page-backed heap once we own the page tables.
const HEAP_SIZE: usize = 8 * 1024 * 1024;
static mut HEAP_ARENA: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

/// Limine HHDM offset: `virt = phys + HHDM_OFFSET` for all physical RAM.
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}

/// Translate a physical address into its HHDM virtual address.
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    VirtAddr::new(pa.as_u64() + hhdm_offset())
}

pub static FRAME_ALLOC: Mutex<FrameAllocator> = Mutex::new(FrameAllocator::new());

/// Summary printed at Milestone 1a.
pub struct MemStats {
    pub usable_bytes: u64,
    pub usable_frames: u64,
    pub largest_region_bytes: u64,
    pub heap_bytes: usize,
}

/// # Safety
/// Call exactly once, early, with Limine's HHDM offset and memory-map entries.
pub unsafe fn init(hhdm: u64, entries: &[&Entry]) -> MemStats {
    HHDM_OFFSET.store(hhdm, Ordering::Relaxed);

    let mut stats = MemStats {
        usable_bytes: 0,
        usable_frames: 0,
        largest_region_bytes: 0,
        heap_bytes: HEAP_SIZE,
    };

    let mut alloc = FRAME_ALLOC.lock();
    for entry in entries {
        if entry.type_ != MEMMAP_USABLE {
            continue;
        }
        stats.usable_bytes += entry.length;
        stats.largest_region_bytes = stats.largest_region_bytes.max(entry.length);

        let start = align_up(entry.base, FRAME_SIZE);
        let end = align_down(entry.base + entry.length, FRAME_SIZE);
        let mut pa = start;
        while pa + FRAME_SIZE <= end {
            // Don't hand the allocator the frames backing the static heap arena;
            // those are inside the kernel image, already excluded from USABLE.
            alloc.push(hhdm, PhysAddr::new(pa));
            stats.usable_frames += 1;
            pa += FRAME_SIZE;
        }
    }
    drop(alloc);

    // Bring the heap up on the static arena.
    HEAP.lock().init(core::ptr::addr_of_mut!(HEAP_ARENA) as *mut u8, HEAP_SIZE);

    stats
}

/// Intrusive free-list node stored in the first bytes of a free frame.
#[repr(C)]
struct FreeNode {
    next: Option<core::ptr::NonNull<FreeNode>>,
}

pub struct FrameAllocator {
    head: Option<core::ptr::NonNull<FreeNode>>,
    free_count: u64,
}

// The list is only ever touched under `FRAME_ALLOC`'s Mutex.
unsafe impl Send for FrameAllocator {}

impl FrameAllocator {
    pub const fn new() -> Self {
        Self { head: None, free_count: 0 }
    }

    pub fn free_frames(&self) -> u64 {
        self.free_count
    }

    /// Push a physical frame onto the free list, writing the link through HHDM.
    fn push(&mut self, hhdm: u64, pa: PhysAddr) {
        let node = (pa.as_u64() + hhdm) as *mut FreeNode;
        unsafe {
            (*node).next = self.head;
            self.head = core::ptr::NonNull::new(node);
        }
        self.free_count += 1;
    }

    pub fn alloc(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let node = self.head?;
        let hhdm = hhdm_offset();
        unsafe {
            self.head = node.as_ref().next;
        }
        self.free_count -= 1;
        let pa = node.as_ptr() as u64 - hhdm;
        Some(PhysFrame::containing_address(PhysAddr::new(pa)))
    }

    pub fn dealloc(&mut self, frame: PhysFrame<Size4KiB>) {
        self.push(hhdm_offset(), frame.start_address());
    }
}

unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for FrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.alloc()
    }
}

impl FrameDeallocator<Size4KiB> for FrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.dealloc(frame)
    }
}

const fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}
const fn align_down(v: u64, a: u64) -> u64 {
    v & !(a - 1)
}

#[alloc_error_handler]
fn oom(layout: core::alloc::Layout) -> ! {
    panic!("kernel heap OOM: {layout:?}");
}
