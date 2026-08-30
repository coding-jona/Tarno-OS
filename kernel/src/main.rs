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
//! Milestone 1f: Limine starts the APs; each does its own GDT/TSS, shared
//! IDT, Local APIC, GS base, then enters the scheduler as its idle thread.
//! Milestone 1g: preemptive kernel-thread scheduler on all CPUs, the single
//! wait primitive (`WaitQueue` / `Event`), and the generic handle table.
//!
//! Phase 1 done. Next (Phase 2): VFS, AHCI, the POSIX personality, and the
//! `syscall` fast path.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod acpi;
mod ahci;
mod apic;
mod console;
mod cpu;
mod elf;
mod ext2;
mod file;
mod gdt;
mod idt;
mod mm;
mod object;
mod pci;
mod process;
mod sched;
mod serial;
mod smp;
mod syscall;
mod vfs;
mod vmm;
mod xhci;
mod wait;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use limine::framebuffer::Framebuffer;
use limine::request::{
    ExecutableAddressRequest, FramebufferRequest, HhdmRequest, MemmapRequest, MpRequest, RsdpRequest,
};
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
#[link_section = ".requests"]
static MP_REQUEST: MpRequest = MpRequest::new(0);

#[used]
#[link_section = ".requests"]
static EXEC_ADDR_REQUEST: ExecutableAddressRequest = ExecutableAddressRequest::new();

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

    cpu::enable_sse();
    gdt::init(0);
    idt::init();
    kprintln!("THOS: GDT + IDT loaded");
    x86_64::instructions::interrupts::int3();
    kprintln!("THOS: traps ok (returned from #BP)");

    memory_bringup();
    acpi_apic_bringup();
    vmm_bringup();

    let mp = MP_REQUEST.response().expect("Limine MP request unanswered");
    smp::init(mp);

    syscall::init_cpu(0);

    scheduler_milestone();
    storage_milestone();

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

/// Build THOS's own page tables and switch onto them.
fn vmm_bringup() {
    let hhdm = HHDM_REQUEST.response().expect("HHDM request unanswered").offset;
    let memmap = MEMMAP_REQUEST.response().expect("memory-map request unanswered");
    let ka = EXEC_ADDR_REQUEST
        .response()
        .expect("Limine executable-address request unanswered");

    vmm::init(hhdm, memmap.entries(), ka.physical_base, ka.virtual_base);

    kprintln!(
        "THOS: own page tables  PML4 switched; {} GiB HHDM + 4 GiB identity + W^X kernel",
        vmm::hhdm_gib(memmap.entries())
    );
}

// --- Milestone 1: scheduler + wait primitive + handle table ---

static WORK_DONE: AtomicU64 = AtomicU64::new(0);
static WAITER_WOKE: AtomicBool = AtomicBool::new(false);
static DEMO_EVENT: wait::Event = wait::Event::new();

const N_WORKERS: usize = 6;
const WORK_PER_WORKER: u64 = 50;

extern "C" fn worker(_id: usize) -> ! {
    for _ in 0..WORK_PER_WORKER {
        WORK_DONE.fetch_add(1, Ordering::Relaxed);
        sched::yield_now();
    }
    sched::exit()
}

extern "C" fn waiter(_: usize) -> ! {
    DEMO_EVENT.wait();
    WAITER_WOKE.store(true, Ordering::Release);
    sched::exit()
}

extern "C" fn setter(_: usize) -> ! {
    for _ in 0..20 {
        sched::yield_now();
    }
    DEMO_EVENT.signal();
    sched::exit()
}

/// Milestone 1: stand up the scheduler, run kernel threads across every CPU,
/// block/wake one on the single wait primitive, and round-trip an object handle.
fn scheduler_milestone() {
    sched::init_bsp();

    // Object + handle table round-trip.
    let ev: Arc<wait::Event> = Arc::new(wait::Event::new());
    let h = object::insert(ev.clone());
    assert!(object::get::<wait::Event>(h).is_some(), "handle lookup failed");

    for i in 0..N_WORKERS {
        sched::spawn("worker", worker, i);
    }
    sched::spawn("waiter", waiter, 0);
    sched::spawn("setter", setter, 0);

    let target = N_WORKERS as u64 * WORK_PER_WORKER;
    while WORK_DONE.load(Ordering::Relaxed) < target || !WAITER_WOKE.load(Ordering::Acquire) {
        sched::yield_now();
    }

    assert!(object::close(h), "handle close failed");

    kprintln!(
        "THOS: sched ok         {} threads, {} work units, {} ctx switches",
        N_WORKERS + 2,
        WORK_DONE.load(Ordering::Relaxed),
        sched::ctx_switches()
    );
    kprintln!(
        "THOS: wait primitive   waiter woke via Event; handles open {}",
        object::open_count()
    );
}

/// Phase 2 milestone: a VFS with an in-memory file opened through the handle
/// table, and the AHCI driver reading real sectors off the SATA disk.
fn storage_milestone() {
    vfs::init();
    let f = vfs::create("/hello");
    f.write_at(0, b"hello from the ram fs\n");
    let h = vfs::open("/hello").expect("open /hello");
    let mut buf = [0u8; 64];
    let n = vfs::read(h, &mut buf).unwrap_or(0);
    serial::print(core::str::from_utf8(&buf[..n]).unwrap_or("?"));
    vfs::close(h);
    kprintln!("THOS: vfs ok           /hello {} bytes; entries {:?}", n, vfs::list());

    ahci::init().expect("AHCI init");
    let mut sb = [0u8; ahci::SECTOR];
    ahci::read(2, &mut sb).expect("AHCI read LBA 2");
    let magic = u16::from_le_bytes([sb[56], sb[57]]);
    kprintln!("THOS: ahci ok          LBA 2 read; ext2 magic {:#06x}", magic);

    let fs = ext2::open().expect("mount ext2");
    let init = fs.read_path("/init").expect("read /init from ext2");
    kprintln!("THOS: ext2 ok          /init = {} bytes", init.len());

    // /init forks, the child execve's /child, /init wait4s and prints the exit
    // code. Two user tasks exit in total.
    let pid = process::spawn_init(&init, &["/init"], &["THOS=1"]);
    kprintln!("THOS: init spawned     pid {}", pid);
    while syscall::user_exits() < 2 {
        sched::yield_now();
    }
    kprintln!("THOS: fork/exec/wait4  ok (init + child both exited)");

    // A real statically-linked musl Rust binary.
    let rs = fs.read_path("/rusthello").expect("read /rusthello from ext2");
    kprintln!("THOS: ext2 ok          /rusthello = {} bytes", rs.len());
    process::spawn_init(&rs, &["/rusthello", "arg1"], &["PATH=/", "THOS=1"]);
    while syscall::user_exits() < 3 {
        sched::yield_now();
    }
    kprintln!("THOS: musl binary ok   (static Rust/musl ran to exit)");

    // USB keyboard via xHCI -> the line-disciplined console -> fd 0.
    match xhci::init() {
        Ok(x) => {
            *XHCI.lock() = Some(x);
            sched::spawn("xhci-poll", xhci_poll_thread, 0);
            kprintln!("THOS: xhci ok          USB keyboard attached (poll thread up)");
        }
        Err(e) => kprintln!("THOS: xhci             {}", e),
    }
}

static XHCI: spin::Mutex<Option<xhci::Xhci>> = spin::Mutex::new(None);

extern "C" fn xhci_poll_thread(_: usize) -> ! {
    loop {
        let mut batch: [[u8; 8]; 8] = [[0; 8]; 8];
        let mut n = 0;
        if let Some(x) = XHCI.lock().as_mut() {
            while n < batch.len() {
                match x.poll_keyboard() {
                    Some(r) => {
                        batch[n] = r;
                        n += 1;
                    }
                    None => break,
                }
            }
        }
        for r in &batch[..n] {
            console::feed_report(r);
        }
        sched::yield_now();
    }
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
