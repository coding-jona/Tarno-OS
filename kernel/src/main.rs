// SPDX-License-Identifier: GPL-2.0-or-later
//! THOS kernel entry.
//!
//! Milestone 0: come up under Limine, prove serial + framebuffer output, halt.
//! Milestone 1a: ingest the Limine memory map, stand up the physical frame
//! allocator and a bootstrap heap, print memory stats.
//! Milestone 1b: load a fresh GDT + TSS (IST stacks) and an IDT with CPU
//! exception handlers; `int3` round-trips.
//!
//! Milestone 1d+1e: parse the MADT; bring up the BSP Local APIC + a
//! PIT-calibrated ~100 Hz periodic timer; interrupts fire.
//!
//! Still ahead in Phase 1 (see docs/thos/roadmap.md): `syscall` entry,
//! SMP bring-up of all 24 threads, scheduler, object manager,
//! the one wait/sync primitive, timers.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod acpi;
mod apic;
mod gdt;
mod idt;
mod mm;
mod serial;

use alloc::vec::Vec;
use core::panic::PanicInfo;
use limine::framebuffer::Framebuffer;
use limine::request::{FramebufferRequest, HhdmRequest, MemmapRequest, RsdpRequest};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

/// Limine base-revision marker. Kept in the `.requests` section.
///
/// Pinned to revision 2 (universally supported by Limine >= 4.x). The `limine`
/// crate's `BaseRevision::new()` requests its `MAX_SUPPORTED` (currently 6),
/// which the vendored bootloader does not implement — that mismatch would make
/// `is_supported()` return false.
#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(2);

#[used]
#[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[link_section = ".requests"]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[link_section = ".requests"]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[link_section = ".requests_start_marker"]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".requests_end_marker"]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

#[no_mangle]
extern "C" fn kmain() -> ! {
    serial::init();
    kprintln!("THOS: kmain reached (Milestone 0)");

    assert!(BASE_REVISION.is_supported(), "unsupported Limine base revision");

    match FRAMEBUFFER_REQUEST.response() {
        Some(fb_response) => match fb_response.framebuffers().first() {
            Some(fb) => {
                paint_smoke_test(fb);
                kprintln!("THOS: framebuffer painted");
            }
            None => kprintln!("THOS: no framebuffer in response"),
        },
        None => kprintln!("THOS: framebuffer request unanswered"),
    }

    gdt::init();
    idt::init();
    kprintln!("THOS: GDT + IDT loaded");
    x86_64::instructions::interrupts::int3();
    kprintln!("THOS: traps ok (returned from #BP)");

    memory_bringup();
    acpi_apic_bringup();

    kprintln!("THOS: halting.");
    exit_qemu(ExitCode::Success);
    hcf();
}

/// Milestone 1a: memory map -> frame allocator + heap, then a smoke check that
/// the heap actually serves allocations.
fn memory_bringup() {
    let hhdm = HHDM_REQUEST
        .response()
        .expect("Limine HHDM request unanswered")
        .offset;
    let memmap = MEMMAP_REQUEST
        .response()
        .expect("Limine memory-map request unanswered");

    let stats = unsafe { mm::init(hhdm, memmap.entries()) };

    kprintln!("THOS: HHDM offset      {:#018x}", hhdm);
    kprintln!(
        "THOS: usable RAM       {} MiB in {} frames",
        stats.usable_bytes / (1024 * 1024),
        stats.usable_frames
    );
    kprintln!(
        "THOS: largest region   {} MiB",
        stats.largest_region_bytes / (1024 * 1024)
    );
    kprintln!("THOS: bootstrap heap   {} KiB", stats.heap_bytes / 1024);

    // Prove the global allocator works.
    let mut v: Vec<u64> = Vec::new();
    for i in 0..1024 {
        v.push(i * i);
    }
    let checksum: u64 = v.iter().sum();
    kprintln!("THOS: heap smoke ok    sum(i^2, i<1024) = {}", checksum);

    let free_before = mm::FRAME_ALLOC.lock().free_frames();
    let f = mm::FRAME_ALLOC.lock().alloc().expect("frame alloc failed");
    let free_after = mm::FRAME_ALLOC.lock().free_frames();
    mm::FRAME_ALLOC.lock().dealloc(f);
    let free_restored = mm::FRAME_ALLOC.lock().free_frames();
    kprintln!(
        "THOS: frame alloc ok   {} -> {} -> {} (phys {:#x})",
        free_before,
        free_after,
        free_restored,
        f.start_address().as_u64()
    );
}

/// Milestone 1d + 1e: parse the MADT (CPU list, IO APICs, IRQ overrides), then
/// bring up the BSP Local APIC and its PIT-calibrated periodic timer, and prove
/// interrupts actually fire by waiting on a few ticks.
fn acpi_apic_bringup() {
    let rsdp = RSDP_REQUEST
        .response()
        .expect("Limine RSDP request unanswered")
        .address as *const u8;

    let info = unsafe { acpi::parse(rsdp) };
    let enabled = info.cpus.iter().filter(|c| c.enabled).count();

    kprintln!(
        "THOS: ACPI rev {}       LAPIC @ {:#x}",
        info.revision,
        info.local_apic_addr
    );
    kprintln!(
        "THOS: CPUs             {} ({} enabled now)",
        info.cpus.len(),
        enabled
    );
    for io in &info.io_apics {
        kprintln!(
            "THOS: IOAPIC id {}      @ {:#x}  gsi_base {}",
            io.id,
            io.address,
            io.gsi_base
        );
    }
    kprintln!("THOS: IRQ overrides    {}", info.overrides.len());

    unsafe { apic::init_bsp(info.local_apic_addr) };
    kprintln!(
        "THOS: LAPIC id {}       timer {} counts/ms",
        apic::bsp_apic_id(),
        apic::counts_per_ms()
    );

    x86_64::instructions::interrupts::enable();
    let start = apic::ticks();
    while apic::ticks() < start + 5 {
        x86_64::instructions::hlt();
    }
    x86_64::instructions::interrupts::disable();
    kprintln!(
        "THOS: APIC timer ok    {} ticks @ ~{} Hz",
        apic::ticks(),
        apic::timer_hz()
    );
}

/// Fill the framebuffer with a recognizable gradient so a human at the target
/// machine (which has no serial console by default) can see the kernel ran.
fn paint_smoke_test(fb: &Framebuffer) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    let pitch = fb.pitch as usize;
    let bpp = (fb.bpp / 8) as usize;
    let base = fb.address() as *mut u8;

    for y in 0..height {
        for x in 0..width {
            let r = (x * 255 / width) as u32;
            let b = (y * 255 / height) as u32;
            let pixel = (r << 16) | (0x20 << 8) | b;
            let offset = y * pitch + x * bpp;
            unsafe {
                core::ptr::write_volatile(base.add(offset) as *mut u32, pixel);
            }
        }
    }
}

/// Halt and catch fire: disable interrupts, park the CPU.
pub(crate) fn hcf() -> ! {
    loop {
        unsafe {
            core::arch::asm!("cli; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

/// QEMU `isa-debug-exit` device (see `-device isa-debug-exit,iobase=0xf4`).
/// Writing here makes QEMU exit with `(code << 1) | 1`; used by `cargo xtask run`
/// and CI so a headless boot terminates instead of hanging on `hlt`.
#[derive(Clone, Copy)]
pub(crate) enum ExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub(crate) fn exit_qemu(code: ExitCode) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0xf4u16,
            in("eax") code as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("THOS PANIC: {}", info);
    exit_qemu(ExitCode::Failed);
    hcf();
}
