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
// The self-test suite (cargo feature `selftest`) is most of this file. A normal
// boot compiles it out entirely, leaving the helpers it used unreferenced.
#![cfg_attr(not(feature = "selftest"), allow(dead_code, unused_imports, unused_variables))]

extern crate alloc;

mod acpi;
mod ahci;
mod apc;
mod apic;
mod console;
mod cpu;
#[cfg(feature = "interactive")]
mod cred;
mod device;
mod elf;
mod execgate;
mod ext2;
mod fat;
mod file;
mod gdi;
mod gdt;
mod gpt;
mod idt;
mod integrity;
#[cfg(feature = "interactive")]
mod login;
mod mm;
mod nt;
mod object;
mod pci;
mod pe;
mod process;
mod registry;
mod sched;
mod secsvc;
mod seh;
mod signal;
mod serial;
mod smp;
mod syscall;
mod timer;
mod usercopy;
mod vfs;
mod vmm;
mod window;
mod fbcon;
mod power;
mod mbr;
mod ps2;
mod random;
mod rtc;
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
use sha2::{Digest, Sha256};

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
                fbcon::init(fb); // from here on, kprintln! also lands on screen
            }
            None => kprintln!("THOS: no framebuffer in response"),
        },
        None => kprintln!("THOS: framebuffer request unanswered"),
    }

    cpu::enable_sse();
    let smep = cpu::enable_smep();
    gdt::init(0);
    idt::init();
    kprintln!("THOS: GDT + IDT loaded");
    kprintln!("THOS: SMEP             {}", if smep { "enabled" } else { "not available on this CPU" });
    x86_64::instructions::interrupts::int3();
    kprintln!("THOS: traps ok (returned from #BP)");

    memory_bringup();
    acpi_apic_bringup();
    fbcon::suspend(); // the FB mapping changes under vmm_bringup
    vmm_bringup();
    gdi_bringup();
    fbcon::resume();
    #[cfg(feature = "selftest")]
    gdi_paint_check();

    let mp = MP_REQUEST.response().expect("Limine MP request unanswered");
    smp::init(mp);

    syscall::init_cpu(0);

    // `selftest` = the full in-kernel verification suite `cargo xtask *-test`
    // drives (it needs the test binaries on the disk, writes scratch files,
    // spawns dozens of processes). A normal boot is just: bring up the
    // scheduler, find the disk + root filesystem, start the input devices.
    #[cfg(feature = "selftest")]
    {
        scheduler_milestone();
        multi_wait_milestone();
        storage_milestone();
    }
    #[cfg(not(feature = "selftest"))]
    {
        sched::init_bsp();
        boot_system();
    }

    #[cfg(feature = "interactive")]
    {
        // Milestone 2: first-run setup / login, then launch the shell off ext2
        // and hand it the keyboard. When the shell ends (`exit`, Ctrl+D) the
        // session is over: back to the login prompt, never a dead console.
        loop {
            let fs = ext2::open().unwrap_or_else(|e| fatal_boot("cannot mount the root filesystem", e));
            let session = login::establish(&fs);
            process::set_session(&session.name, session.uid);
            kprintln!("THOS: session          {} (uid {})", session.name, session.uid);

            // The interactive shell is stock BusyBox `sh` (ash).
            let sh = fs
                .read_path("/busybox")
                .unwrap_or_else(|| fatal_boot("no login shell", "/busybox is missing from the root filesystem"));
            kprintln!("THOS: shell            /busybox sh = {} bytes", sh.len());
            let pid = process::spawn_init(
                &sh,
                &["sh"],
                &["PATH=/bin:/", "HOME=/", "PWD=/", "TERM=dumb", "PS1=thos$ "],
            );

            signal::FG_PGRP.store(pid, core::sync::atomic::Ordering::Relaxed);
            kprintln!("THOS: interactive hold — type on the keyboard");
            while !process::pid_exited(pid) {
                sched::yield_now();
            }
            kprintln!("\nTHOS: session ended");
        }
    }

    #[cfg(not(feature = "interactive"))]
    {
        kprintln!("THOS: halting.");
        exit_qemu(ExitCode::Success);
        hcf();
    }
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
    unsafe { power::init(rsdp) };
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

    // Start the wall clock: RTC reading + the TSC calibrated against the PIT.
    let rtc = rtc::read_unix();
    timer::start_clock(apic::tsc_per_ms(), rtc);
    kprintln!(
        "THOS: clock            TSC {} MHz; RTC {}",
        apic::tsc_per_ms() / 1000,
        match rtc {
            Some(t) => alloc::format!("{} (unix)", t),
            None => alloc::string::String::from("unreadable — wall clock starts at 0"),
        }
    );

    random::init(); // after the clock: the RTC and TSC are inputs to the pool

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

/// Map the boot framebuffer into THOS's own tables — the GDI32/User32
/// skeleton's one and only "device context". Must run after `vmm_bringup`
/// (needs `vmm::map_mmio`, which needs the kernel PML4).
fn gdi_bringup() {
    let hhdm = HHDM_REQUEST.response().expect("HHDM request unanswered").offset;
    match FRAMEBUFFER_REQUEST.response().and_then(|r| r.framebuffers().first()) {
        Some(fb) => gdi::init(fb, hhdm),
        None => kprintln!("THOS: gdi FAIL         no framebuffer in response"),
    }
}

/// `gdi::` functions exercised directly — no PE process, no hand-assembled
/// syscall trampolines needed, same rationale as `registry_enum_check` /
/// `section_sharing_check`. Proves the pixel plumbing actually reaches the
/// real framebuffer: a fill lands at the right offsets and nowhere else, a
/// set/get round-trips exactly, an off-screen access is rejected rather than
/// walking off the mapped region, and the brush/select-object colour model
/// behaves like real GDI (old colour handed back, stock objects are right).
fn gdi_paint_check() {
    let (w, h) = gdi::screen_size();
    if w == 0 {
        kprintln!("THOS: gdi skip check   no framebuffer, nothing to verify");
        return;
    }
    const SCREEN: u64 = 1; // GetDC(0)
    assert_eq!(gdi::set_pixel(SCREEN, 0, 0, 0x00AB_CDEF), 0x00AB_CDEF, "SetPixel: bad return");
    assert_eq!(gdi::get_pixel(SCREEN, 0, 0), 0x00AB_CDEF, "SetPixel/GetPixel round-trip lost the colour");
    assert_eq!(gdi::get_pixel(SCREEN, -1, 0), u32::MAX, "GetPixel(-1, _) should be CLR_INVALID");
    assert_eq!(gdi::get_pixel(SCREEN, w as i64, 0), u32::MAX, "GetPixel(width, _) should be CLR_INVALID (off-screen)");

    let white = gdi::get_stock_object(0); // WHITE_BRUSH
    let black = gdi::get_stock_object(4); // BLACK_BRUSH
    let prev = gdi::select_object(SCREEN, white);
    assert_eq!(prev, white, "SelectObject should hand back the DC's previous brush (default: white)");
    // A known white background around the black rect, so the edge checks
    // below aren't at the mercy of whatever the boot gradient left there.
    assert!(gdi::fill_rect(SCREEN, 5, 5, 25, 25), "fill_rect should report success for an on-screen rect");
    gdi::select_object(SCREEN, black);
    assert!(gdi::fill_rect(SCREEN, 10, 10, 20, 20), "fill_rect should report success for an on-screen rect");
    assert_eq!(gdi::get_pixel(SCREEN, 15, 15), 0x0000_0000, "Rectangle didn't actually paint black inside the rect");
    assert_eq!(gdi::get_pixel(SCREEN, 9, 15), 0x00FF_FFFF, "Rectangle painted outside its left edge");
    assert_eq!(gdi::get_pixel(SCREEN, 20, 15), 0x00FF_FFFF, "Rectangle painted outside its right edge (exclusive bound)");

    // A rectangle that only partially overlaps the screen still fills the
    // part that's on it, and doesn't walk off the mapped framebuffer.
    assert!(gdi::fill_rect(SCREEN, -5, -5, 5, 5), "a partially off-screen rect should still fill its on-screen part");
    assert_eq!(gdi::get_pixel(SCREEN, 0, 0), 0x0000_0000, "partially off-screen fill didn't reach the on-screen corner");
    assert!(!gdi::fill_rect(SCREEN, -10, -10, -1, -1), "a fully off-screen rect should report no fill");

    // --- window-relative DC: the actual point of this check ---
    window::register_class(alloc::string::String::from("GdiCheckClass"), 0);
    let hwnd = window::create_window("GdiCheckClass", 50, 50, 20, 20, 0);
    assert_ne!(hwnd, 0, "create_window should succeed against a registered class");
    let wdc = gdi::WINDOW_DC_TAG | hwnd as u64;

    // (0,0) in the window's own DC is screen (50,50) — separate from the
    // screen DC's own (0,0), which the checks above already painted black.
    gdi::select_object(wdc, gdi::create_solid_brush(0x0000_FF00)); // green
    assert_eq!(gdi::set_pixel(wdc, 0, 0, 0x0000_00FF), 0x0000_00FF, "SetPixel on a window DC: bad return");
    assert_eq!(gdi::get_pixel(SCREEN, 50, 50), 0x0000_00FF, "window DC (0,0) didn't land at the window's screen origin");
    assert_eq!(gdi::get_pixel(SCREEN, 15, 15), 0x0000_0000, "drawing through the window DC leaked into the screen DC's rect");

    // A rectangle drawn through the window DC clips to the window's own
    // 20x20 rect, not the whole screen: [10,10)..[30,30) client-relative
    // clips to [0,0)..[20,20) client == [50,50)..[70,70) screen.
    assert!(gdi::fill_rect(wdc, -10, -10, 30, 30), "fill_rect on a window DC should still report success");
    assert_eq!(gdi::get_pixel(SCREEN, 50, 50), 0x0000_FF00, "window-DC fill_rect didn't reach its own client origin");
    assert_eq!(gdi::get_pixel(SCREEN, 69, 69), 0x0000_FF00, "window-DC fill_rect didn't reach its own client corner");
    assert_ne!(gdi::get_pixel(SCREEN, 70, 70), 0x0000_FF00, "window-DC fill_rect wasn't clipped to the window's own rect");

    kprintln!("THOS: gdi paint ok     {}x{}; SetPixel/GetPixel + brush + Rectangle + window DC verified", w, h);
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

// --- Milestone 1 addition: the real multi-object wait-block
// (`wait::wait_any_until`, what `NtWaitForMultipleObjects` uses) — two
// threads each parked on the *same pair* of events at once, one WaitAny-style
// (wakes on the first) and one WaitAll-style (wakes only once both are set),
// proving the multi-queue block actually blocks and wakes correctly, and that
// two independent multi-waits over an overlapping object set don't deadlock
// each other via the fixed-lock-order dedup in `wait_any_until`.
static MULTI_EV_A: wait::Event = wait::Event::new();
static MULTI_EV_B: wait::Event = wait::Event::new();
static MULTI_WOKE_ANY: AtomicBool = AtomicBool::new(false);
static MULTI_WOKE_ALL: AtomicBool = AtomicBool::new(false);

extern "C" fn multi_any_waiter(_: usize) -> ! {
    while !MULTI_EV_A.is_signaled() && !MULTI_EV_B.is_signaled() {
        wait::wait_any_until(&[MULTI_EV_A.queue(), MULTI_EV_B.queue()], None, || {
            !MULTI_EV_A.is_signaled() && !MULTI_EV_B.is_signaled()
        });
    }
    MULTI_WOKE_ANY.store(true, Ordering::Release);
    sched::exit()
}

extern "C" fn multi_all_waiter(_: usize) -> ! {
    while !(MULTI_EV_A.is_signaled() && MULTI_EV_B.is_signaled()) {
        wait::wait_any_until(&[MULTI_EV_A.queue(), MULTI_EV_B.queue()], None, || {
            !(MULTI_EV_A.is_signaled() && MULTI_EV_B.is_signaled())
        });
    }
    MULTI_WOKE_ALL.store(true, Ordering::Release);
    sched::exit()
}

extern "C" fn multi_setter(_: usize) -> ! {
    for _ in 0..20 {
        sched::yield_now();
    }
    MULTI_EV_A.signal(); // the WaitAny waiter must wake now — WaitAll must not yet
    for _ in 0..20 {
        sched::yield_now();
    }
    MULTI_EV_B.signal(); // now the WaitAll waiter must wake too
    sched::exit()
}

fn multi_wait_milestone() {
    sched::spawn("multi-any", multi_any_waiter, 0);
    sched::spawn("multi-all", multi_all_waiter, 0);
    sched::spawn("multi-set", multi_setter, 0);
    while !MULTI_WOKE_ANY.load(Ordering::Acquire) || !MULTI_WOKE_ALL.load(Ordering::Acquire) {
        sched::yield_now();
    }
    kprintln!("THOS: multi wait ok    WaitAny + WaitAll both blocked and woke correctly");
}

// --- SMP scheduler stress (feature = "stress", driven by `cargo xtask smp-test`) ---

#[cfg(feature = "stress")]
mod stress {
    use super::*;

    pub static SPAWNED: AtomicU64 = AtomicU64::new(0);
    pub static EXITED: AtomicU64 = AtomicU64::new(0);
    pub static RUNS: AtomicU64 = AtomicU64::new(0);
    pub static BAD_CANARY: AtomicU64 = AtomicU64::new(0);

    pub static PARK_Q: wait::WaitQueue = wait::WaitQueue::new();
    pub static PARK_RUNS: AtomicU64 = AtomicU64::new(0);
    pub static PARK_EXITED: AtomicU64 = AtomicU64::new(0);

    pub const WAVES: u64 = 8;
    pub const PER_WAVE: usize = 64;
    pub const YIELDS: u64 = 60;
    pub const PARKERS: usize = 48;
    pub const WAKERS: usize = 4;
    pub const PARK_CYCLES: u64 = 40;
    pub const USER_INITS: u64 = 4;

    /// Fill a stack buffer with a per-thread pattern, yield many times, then
    /// check it survived. If the scheduler ever ran this thread on two CPUs at
    /// once (kernel stack reused mid-flight) the yields would smash it.
    fn canary_check(id: u64) -> bool {
        let mut buf = [0u64; 96];
        let seed = 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(id.wrapping_add(1));
        for (i, c) in buf.iter_mut().enumerate() {
            *c = seed ^ i as u64;
        }
        for _ in 0..YIELDS {
            RUNS.fetch_add(1, Ordering::Relaxed);
            sched::yield_now();
        }
        buf.iter().enumerate().all(|(i, &c)| c == seed ^ i as u64)
    }

    pub extern "C" fn churn_worker(id: usize) -> ! {
        if !canary_check(id as u64) {
            BAD_CANARY.fetch_add(1, Ordering::Relaxed);
        }
        EXITED.fetch_add(1, Ordering::Relaxed);
        sched::exit()
    }

    pub extern "C" fn parker(id: usize) -> ! {
        let mut buf = [0u64; 64];
        let seed = 0xA5A5_5A5Au64 ^ id as u64;
        for (i, c) in buf.iter_mut().enumerate() {
            *c = seed ^ i as u64;
        }
        for _ in 0..PARK_CYCLES {
            PARK_Q.wait();
            PARK_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        if !buf.iter().enumerate().all(|(i, &c)| c == seed ^ i as u64) {
            BAD_CANARY.fetch_add(1, Ordering::Relaxed);
        }
        PARK_EXITED.fetch_add(1, Ordering::Relaxed);
        sched::exit()
    }

    pub extern "C" fn waker(_id: usize) -> ! {
        while PARK_EXITED.load(Ordering::Relaxed) < PARKERS as u64 {
            PARK_Q.wake_all();
            sched::yield_now();
        }
        sched::exit()
    }
}

/// Gate B: hammer the scheduler on every CPU — hundreds of threads churning
/// `yield` / `exit`, a pool blocking and being mass-woken on the wait queue,
/// and a few real user `fork`/`wait4` processes — then assert nothing was
/// lost, double-run, or ran on two CPUs at once.
#[cfg(feature = "stress")]
fn smp_stress_milestone(init_bytes: &[u8]) {
    use stress::*;

    kprintln!(
        "THOS: smp stress start {} CPUs; {} churn + {} parkers + {} user forks",
        smp::cpu_count(),
        WAVES as usize * PER_WAVE,
        PARKERS,
        USER_INITS,
    );

    let user_base = syscall::user_exits();
    for i in 0..PARKERS {
        sched::spawn("stress-park", parker, i);
    }
    for i in 0..WAKERS {
        sched::spawn("stress-wake", waker, i);
    }
    for _ in 0..USER_INITS {
        process::spawn_init(init_bytes, &["/init"], &["THOS=1"]);
    }

    for _ in 0..WAVES {
        for i in 0..PER_WAVE {
            SPAWNED.fetch_add(1, Ordering::Relaxed);
            sched::spawn("stress-churn", churn_worker, i);
        }
        // Only let a wave half-drain before piling on the next, so create and
        // destroy overlap across all CPUs the whole time.
        let mark = EXITED.load(Ordering::Relaxed) + (PER_WAVE as u64 / 2);
        while EXITED.load(Ordering::Relaxed) < mark {
            sched::yield_now();
        }
        sched::reap(); // free exited stacks — the bootstrap heap is small
    }

    while EXITED.load(Ordering::Relaxed) < SPAWNED.load(Ordering::Relaxed)
        || PARK_EXITED.load(Ordering::Relaxed) < PARKERS as u64
        || syscall::user_exits() < user_base + USER_INITS * 2
    {
        sched::yield_now();
        sched::reap();
    }
    sched::reap();

    let spawned = SPAWNED.load(Ordering::Relaxed);
    let runs = RUNS.load(Ordering::Relaxed);
    let park_runs = PARK_RUNS.load(Ordering::Relaxed);
    let bad = BAD_CANARY.load(Ordering::Relaxed);

    assert_eq!(bad, 0, "SMP stress: {bad} threads saw a smashed stack canary");
    assert_eq!(
        runs,
        spawned * YIELDS,
        "SMP stress: churn run count {runs} != {spawned}*{YIELDS} (lost or double-run)"
    );
    assert_eq!(
        park_runs,
        PARKERS as u64 * PARK_CYCLES,
        "SMP stress: parker run count {park_runs} != {PARKERS}*{PARK_CYCLES}"
    );

    kprintln!(
        "THOS: smp stress ok    {spawned} churn + {PARKERS} parker threads clean; {runs}+{park_runs} runs, {} ctx switches, {} phantoms dropped",
        sched::ctx_switches(),
        sched::phantoms_dropped(),
    );
}

/// `NtEnumerateKey` / `NtEnumerateValueKey`'s backing logic
/// (`registry::enumerate_key`/`enumerate_value`), exercised directly — no PE
/// process needed, unlike the hand-assembled `pe-test` round-trip. Uses a
/// throwaway key so it can't collide with — or leak into — a real hive.
fn registry_enum_check() {
    let base = r"\Registry\Machine\Software\ThosEnumCheck";
    assert!(registry::create(base), "registry_enum_check: create base");
    for (sub, val, data) in [("Alpha", "A", b"1".as_slice()), ("Beta", "B", b"22".as_slice())] {
        assert!(registry::create(&alloc::format!("{base}\\{sub}")), "create subkey");
        assert!(registry::set_value(base, val, 4, data), "set value");
    }
    // Subkeys and values enumerate in (sorted) order, and stop past the end.
    assert_eq!(registry::enumerate_key(base, 0).as_deref(), Some("alpha"));
    assert_eq!(registry::enumerate_key(base, 1).as_deref(), Some("beta"));
    assert_eq!(registry::enumerate_key(base, 2), None);
    let (name0, ty0, len0) = registry::enumerate_value(base, 0).expect("value 0");
    assert_eq!((name0.as_str(), ty0, len0), ("a", 4, 1));
    let (name1, ty1, len1) = registry::enumerate_value(base, 1).expect("value 1");
    assert_eq!((name1.as_str(), ty1, len1), ("b", 4, 2));
    assert!(registry::enumerate_value(base, 2).is_none());
    // Clean up: don't leave a stray key sitting in a real hive on disk.
    for sub in ["alpha", "beta"] {
        registry::delete_key(&alloc::format!("{base}\\{sub}"));
    }
    assert!(registry::delete_key(base), "registry_enum_check: cleanup");
    kprintln!("THOS: registry enum ok NtEnumerateKey/Value order + STATUS_NO_MORE_ENTRIES");
}

/// Per-key registry security — `registry::create_write_ok`/`write_key_ok`,
/// the predicates `nt.rs`'s `NtCreateKey`/`NtSetValueKey`/`NtDeleteKey`
/// actually enforce. A key owned by a non-system uid accepts its own
/// owner's write and root's, rejects a stranger's; *creating* a new subkey
/// is checked against the nearest existing ancestor (the new key doesn't
/// exist yet to have an owner of its own) — same "write to the parent"
/// shape the filesystem's DAC already established, generalized for the
/// registry's own auto-vivified ancestors.
fn registry_security_check(fs: &ext2::Ext2) {
    let base = r"\Registry\Machine\Software\ThosSecCheck";
    assert!(registry::create_owned(base, 1000), "create_owned as uid 1000");
    assert_eq!(registry::owner_of(base), Some(1000));

    assert!(registry::write_key_ok(base, 1000), "the owner must be able to write their own key");
    assert!(registry::write_key_ok(base, 0), "root must always be able to write");
    assert!(!registry::write_key_ok(base, 2000), "a different uid must be denied");

    // Creating a *new* subkey: checked against the nearest existing
    // ancestor (`base`, owned 1000), not the not-yet-existing subkey.
    let child = alloc::format!("{base}\\Sub");
    assert!(registry::create_write_ok(&child, 1000), "the owner may create a subkey under their own key");
    assert!(
        !registry::create_write_ok(&child, 2000),
        "a stranger may not create a subkey under someone else's key"
    );
    assert!(registry::create_owned(&child, 1000), "actually create the subkey");
    assert_eq!(registry::owner_of(&child), Some(1000));

    // A system-owned key (uid 0, `create`'s default — every pre-existing
    // hive key from before this increment) is writable by root, same as
    // always, but now genuinely denied to a normal uid — the DAC gap this
    // slice closes.
    let sys_child = alloc::format!("{base}\\SysSub");
    assert!(registry::create(&sys_child), "system-owned create");
    assert_eq!(registry::owner_of(&sys_child), Some(0));
    assert!(registry::write_key_ok(&sys_child, 0), "root may always write a system-owned key");
    assert!(!registry::write_key_ok(&sys_child, 4000), "a normal uid may not write a system-owned key");

    // Persistence: `create_owned` under a hive path (`Machine\Software`)
    // already went through `persist()` above, same path every other
    // registry write takes — confirm the owner genuinely made it into the
    // on-disk hive bytes, not just in-memory state.
    let hive = fs.read_path("/etc/thos/registry/software.hiv").expect("read software.hiv");
    let text = core::str::from_utf8(&hive).expect("hive is valid utf8");
    assert!(
        text.lines().any(|l| l.starts_with("O ") && l.ends_with(" 1000")),
        "no 'O <relpath> 1000' owner record found in the persisted hive"
    );

    for sub in ["sub", "syssub"] {
        registry::delete_key(&alloc::format!("{base}\\{sub}"));
    }
    registry::delete_key(base);
    kprintln!("THOS: registry sec ok  per-key owner, write DAC, ancestor-create check, persisted to hive");
}

/// Change-notify (`registry::watch`/`nt.rs`'s `NtNotifyChangeKey`) — the
/// registry side directly (no PE process needed: `watch` takes a plain
/// `Arc<wait::Event>`, the same object `NtNotifyChangeKey` resolves from a
/// handle). `nt.rs`'s own dispatch glue (`current_regkey`/`current_event`/
/// the `WatchTree` stack arg) isn't exercised by a dedicated live PE test
/// yet — no hand-assembled PE test scenario calls it today, the same
/// honest scoping already used for `chmod`/`chown` (no BusyBox applet) and
/// `execve`'s exec-gate wiring.
fn registry_notify_check() {
    let base = r"\Registry\Machine\Software\ThosNotifyCheck";
    assert!(registry::create(base), "create base");

    // A watch on `base` itself fires on a value change on `base`.
    let ev = alloc::sync::Arc::new(wait::Event::new());
    assert!(registry::watch(base, false, ev.clone()), "watch an existing key");
    assert!(!ev.is_signaled(), "must not be signalled before any change");
    assert!(registry::set_value(base, "Foo", 4, b"1"), "set a value on base");
    assert!(ev.is_signaled(), "watch didn't fire on a value change");

    // One-shot: the same change again must NOT re-fire a watch that has
    // already fired and wasn't re-armed — prove it with a *fresh* event
    // that was never (re-)registered after the first fire.
    let ev2 = alloc::sync::Arc::new(wait::Event::new());
    assert!(registry::set_value(base, "Foo", 4, b"2"), "set base's value again");
    assert!(!ev2.is_signaled(), "an unrelated, never-registered event must never be signalled");

    // Without WatchTree, a direct child firing base is expected (depth 1);
    // a grandchild must NOT fire it.
    let child = alloc::format!("{base}\\Child");
    let grandchild = alloc::format!("{child}\\Grandchild");
    assert!(registry::create(&child), "create child");
    let ev3 = alloc::sync::Arc::new(wait::Event::new());
    assert!(registry::watch(base, false, ev3.clone()), "re-arm the watch on base");
    assert!(registry::create(&grandchild), "create grandchild");
    assert!(!ev3.is_signaled(), "a non-WatchTree watch must not fire on a grandchild-depth change");
    assert!(registry::set_value(&child, "Bar", 4, b"1"), "set a value on the direct child");
    assert!(ev3.is_signaled(), "a non-WatchTree watch must still fire on a direct child's own change");

    // WatchTree DOES cover the grandchild.
    let ev4 = alloc::sync::Arc::new(wait::Event::new());
    assert!(registry::watch(base, true, ev4.clone()), "watch base with WatchTree");
    assert!(registry::set_value(&grandchild, "Baz", 4, b"1"), "set a value deep in the subtree");
    assert!(ev4.is_signaled(), "a WatchTree watch must fire on a grandchild-depth change");

    assert!(!registry::watch(r"\Registry\Machine\Software\ThosNoSuchKey", false, ev.clone()), "watching a missing key must fail");

    registry::delete_key(&grandchild);
    registry::delete_key(&child);
    registry::delete_key(base);
    kprintln!("THOS: registry notify ok one-shot fire, WatchTree depth, no-fire-before-change");
}

/// `cargo xtask registry-crash-test`'s kernel-side half — the `regcrashtest`
/// feature only. Idempotent across the test's two boots via the target
/// file's own content, the same pattern `integrity_check` uses to tell
/// "first boot" from "later boot": missing → seed it, then overwrite it
/// (the overwrite is what hits `ext2::write_path_owned`'s injected crash
/// point and halts QEMU right after the inode commit, before the old
/// blocks are freed — simulating a real crash at exactly that instant);
/// present → the second boot, after xtask has run `e2fsck` on the
/// "crashed" disk image, just reads it back and reports what's there,
/// proving the commit itself survived.
#[cfg(feature = "regcrashtest")]
fn registry_crash_check(fs: &ext2::Ext2) {
    const PATH: &str = "/regcrash-test.bin";
    const OLD: &[u8] = b"OLD-CONTENT-1";
    const NEW: &[u8] = b"NEW-CONTENT-LONGER-THAN-OLD-ONE-DELIBERATELY";
    match fs.read_path(PATH) {
        None => {
            fs.write_path(PATH, OLD).expect("seed regcrash-test.bin");
            fs.write_path(PATH, NEW).expect("overwrite regcrash-test.bin (should crash mid-way)");
            // Only reached if the injected crash point in ext2.rs didn't
            // fire — a real test-harness bug, worth a loud, distinct marker
            // rather than silently falling through to a normal boot.
            kprintln!("THOS: regcrash FAIL    the injected crash point never fired");
        }
        Some(bytes) => {
            kprintln!("THOS: regcrash ok      {:?} after a simulated crash mid-overwrite", core::str::from_utf8(&bytes));
        }
    }
}

/// `execgate::check`'s detection logic, exercised directly — the algorithm
/// shared by both `spawn_pe` (`PE reject`/exec-gate check below, a real
/// `pe::load` round trip) and `execve` (wired the same way, not yet
/// exercised by a dedicated live test — no test binary execve's malicious
/// content today; this at least proves the shared detection logic itself is
/// right). Ordinary bytes with no signature must pass; the EICAR string
/// anywhere in an otherwise arbitrary buffer must not.
fn execgate_check() {
    assert_eq!(execgate::check(b"just an ordinary file, nothing to see here"), execgate::Verdict::Allow);
    assert_eq!(execgate::check(&[]), execgate::Verdict::Allow);
    let mut buf = alloc::vec![0xAAu8; 64];
    buf.extend_from_slice(b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*");
    buf.extend_from_slice(&[0xBBu8; 64]);
    assert!(
        matches!(execgate::check(&buf), execgate::Verdict::Quarantine(_)),
        "EICAR string buried in the middle of a buffer wasn't caught"
    );

    // `MARKER_STRING`'s own hash-list entry (`BLOCKED_HASHES`, the *local*
    // fallback) is exercised end-to-end by `secsvc_check` right after this
    // — it controls the Security Service's lifecycle, and the verdict for
    // this hash now genuinely depends on whether the service is up (it
    // isn't on the service's own list — Allow) or down (the local fallback
    // catches it — Quarantine). Here, just confirm the hand-transcribed hex
    // constant in execgate.rs really is this string's SHA-256, independent
    // of that lifecycle — a real check, not trusting the bytes blindly.
    let mut h = Sha256::new();
    h.update(execgate::MARKER_STRING);
    let digest: [u8; 32] = h.finalize().into();
    assert_eq!(
        digest,
        execgate::marker_hash(),
        "MARKER_STRING's real SHA-256 doesn't match execgate.rs's BLOCKED_HASHES entry"
    );

    kprintln!("THOS: exec gate check ok EICAR signature detected, clean content passes");
}

/// The Security Service round trip — real process isolation, a real
/// kernel↔service channel, and a real crash-degrade fallback, not assumed.
/// `secsvc::spawn` already ran in `kmain`; this exercises `execgate::check`
/// both while the service is alive and after it (deliberately) exits.
fn secsvc_check(fs: &ext2::Ext2) {
    // While alive: a hash *only the service's own list* knows about —
    // `SECSVC_ONLY_MARKER` is deliberately absent from the kernel's local
    // `BLOCKED_HASHES` — must be quarantined, and the reason must name the
    // service, not the local fallback.
    assert_eq!(
        execgate::check(execgate::SECSVC_ONLY_MARKER),
        execgate::Verdict::Quarantine("Security Service: known-bad hash"),
        "the service-alive verdict must come from the service, not the local list"
    );

    // Quarantine store: the service (a real process with its own ext2
    // access, independent of the IPC pipes) is expected to have appended a
    // record of that decision to `/etc/thos/quarantine.log` — checked here
    // by reading the actual on-disk bytes straight off ext2, not trusting
    // the service's in-memory state or the verdict alone. No RTC yet (a
    // real, separate gap — `syscall.rs`'s own `SYS_TIME` stub), so this
    // just confirms *a* record naming the right hash exists, not a
    // timestamp.
    let mut h = Sha256::new();
    h.update(execgate::SECSVC_ONLY_MARKER);
    let digest: [u8; 32] = h.finalize().into();
    let hex: alloc::string::String = digest.iter().map(|b| alloc::format!("{b:02x}")).collect();
    let log = fs.read_path("/etc/thos/quarantine.log").expect("read quarantine.log");
    let text = core::str::from_utf8(&log).expect("quarantine.log is valid utf8");
    assert!(
        text.lines().any(|l| l.contains(&hex)),
        "no quarantine.log record found for SECSVC_ONLY_MARKER's hash"
    );

    // Still alive: ordinary content the service has never heard of either
    // — a real Allow verdict from the round trip, not a rejection-by-default.
    assert_eq!(execgate::check(b"nothing interesting, service should allow this"), execgate::Verdict::Allow);

    // The test poison pill: makes the service process exit immediately,
    // simulating a crash. `set_exit_status` (process.rs) clears its fd
    // table the instant it does, so the kernel's next `check_hash` sees a
    // real EOF, not a guess or a timeout.
    assert_eq!(secsvc::check_hash(&[0xFFu8; 32]), None, "the poison pill itself has no verdict");
    for _ in 0..64 {
        sched::yield_now(); // let the service's exit actually run
    }

    // Now degraded: the kernel's own local fallback must still catch what
    // it always could (`MARKER_STRING`, in `BLOCKED_HASHES`) — the actual
    // "crash there degrades to a policy default" property, not a crash.
    assert_eq!(
        execgate::check(execgate::MARKER_STRING),
        execgate::Verdict::Quarantine("known-bad hash (local fallback)"),
        "the local fallback must still work once the service is gone"
    );
    // The local fallback is still a whole-file hash match, not a substring
    // scan — a buffer that merely *contains* the marker must not fire.
    let mut wrapped = alloc::vec![0xCCu8; 8];
    wrapped.extend_from_slice(execgate::MARKER_STRING);
    wrapped.extend_from_slice(&[0xDDu8; 8]);
    assert_eq!(
        execgate::check(&wrapped),
        execgate::Verdict::Allow,
        "the local fallback must require an exact whole-file match, not fire on a substring"
    );
    // And the service-only hash — genuinely unknown to the local list — is
    // now allowed, proving the earlier quarantine really was the service's
    // own verdict, not a coincidental local hit.
    assert_eq!(execgate::check(execgate::SECSVC_ONLY_MARKER), execgate::Verdict::Allow);

    // The quarantine record itself is on ext2, not in the (now-gone)
    // service's memory — it must still be there, untouched, after the
    // simulated crash.
    let log_after = fs.read_path("/etc/thos/quarantine.log").expect("quarantine.log must survive the service exiting");
    assert!(
        core::str::from_utf8(&log_after).is_ok_and(|t| t.lines().any(|l| l.contains(&hex))),
        "the quarantine record didn't survive the service's exit"
    );

    kprintln!("THOS: secsvc check ok  service-backed verdict, quarantine log persisted, crash + local fallback");
}

/// `NtCreateSection`/`NtMapViewOfSection`/`NtUnmapViewOfSection`/
/// `NtFlushVirtualMemory`'s backing logic (`process::Section` /
/// `Process::map_section_view` etc.), exercised directly against real page
/// tables — no PE process needed. Proves the actual new capability over the
/// old copy-based section: two views share the *same* physical frames (a
/// write through one view's VA is visible reading through the other's,
/// checked via `Process::translate`, not just "the `Vec<PhysFrame>` lists
/// match"), `unmap_view` tears down and is not idempotent, and a file-backed
/// section's `flush()` genuinely rewrites the underlying ext2 file's bytes
/// on disk.
fn section_sharing_check(fs: &ext2::Ext2) {
    let proc = process::Process::new();
    let hhdm = mm::hhdm_offset();

    // --- anonymous section: two views, one process, shared frames ---
    let sec = Arc::new(process::Section::new(b"AAAA", None));
    let v1 = proc.map_section_view(&sec, 0, 4096);
    let v2 = proc.map_section_view(&sec, 0, 4096);
    assert_ne!(v1, v2, "section_sharing_check: two views got the same VA");

    proc.write_user(v1, b"HELLO");
    let p2 = proc.translate(v2).expect("view2 mapped");
    let seen = unsafe { core::slice::from_raw_parts((p2 + hhdm) as *const u8, 5) };
    assert_eq!(seen, b"HELLO", "write through view1 not visible through view2 — sections aren't sharing frames");

    proc.write_user(v2, b"WORLD");
    let p1 = proc.translate(v1).expect("view1 mapped");
    let seen_back = unsafe { core::slice::from_raw_parts((p1 + hhdm) as *const u8, 5) };
    assert_eq!(seen_back, b"WORLD", "write through view2 not visible through view1");

    assert!(proc.unmap_view(v2), "unmap_view(v2) should succeed the first time");
    assert!(!proc.unmap_view(v2), "unmap_view(v2) should fail the second time (already gone)");
    assert!(proc.translate(v2).is_none(), "view2's PTEs should be torn down after unmap");
    assert!(proc.translate(v1).is_some(), "view1 must survive v2's unmap — frames are shared, not owned by one view");
    assert!(proc.unmap_view(v1), "unmap_view(v1) should still succeed");

    // --- file-backed section: flush() writes real bytes back to ext2 ---
    let path = "/section_check.tmp";
    fs.write_path(path, b"before").expect("seed /section_check.tmp");
    let file: Arc<dyn file::FileOps> =
        file::Ext2File::new(path.into(), fs.read_path(path).expect("read seeded file"));
    let sec2 = Arc::new(process::Section::new(b"before", Some(file)));
    let v3 = proc.map_section_view(&sec2, 0, sec2.size);
    proc.write_user(v3, b"after!");
    assert!(proc.flush_view(v3), "flush_view should succeed");
    let on_disk = fs.read_path(path).expect("re-read /section_check.tmp");
    assert_eq!(&on_disk[..6], b"after!", "Section::flush didn't actually rewrite the ext2 file");
    assert!(proc.unmap_view(v3));
    fs.unlink_path(path).ok();

    kprintln!("THOS: sections ok      shared frames across views; file-backed flush writes through to ext2");
}

/// File-integrity baselines: first boot with none stored records SHA-256
/// hashes of `integrity::BASELINE_FILES`; every later boot recomputes and
/// compares against that record. Detection only — nothing here stops a
/// write to a baselined file, it just notices one happened.
fn integrity_check(fs: &ext2::Ext2) {
    if !integrity::exists(fs) {
        let checks = integrity::record(fs, integrity::BASELINE_FILES);
        let missing = checks.iter().filter(|c| c.outcome == integrity::Outcome::Missing).count();
        kprintln!(
            "THOS: integrity ok     baseline recorded for {}/{} files (first boot){}",
            checks.len() - missing,
            checks.len(),
            if missing == 0 { "" } else { " — some missing" }
        );
        return;
    }
    let checks = integrity::verify(fs);
    let tampered: alloc::vec::Vec<_> =
        checks.iter().filter(|c| c.outcome == integrity::Outcome::Tampered).collect();
    if tampered.is_empty() {
        kprintln!("THOS: integrity ok     {} files verified against baseline, no tampering", checks.len());
    } else {
        for c in &tampered {
            kprintln!("THOS: integrity FAIL   {} does not match its baseline hash", c.path);
        }
    }
}

/// Real POSIX file creation (`open_resolved`'s `O_CREAT` slice) and
/// `chmod`/`chown` (`ext2::chmod_path`/`chown_path` plus the permission
/// policy `syscall::sys_chmod`/`sys_chown` apply around them).
/// `open_resolved` itself needs a real task context (it reads the calling
/// task's uid via `sched::current().task()`, not available this early at
/// boot) so it's proven end-to-end through a live shell instead — kbd-test's
/// `touch`/`mkdir`. BusyBox carries no `chmod`/`chown` applet, so those two
/// are proven here at the ext2-layer + policy-logic level: real, just not
/// yet exercised through the live syscall ABI by a dedicated test.
fn posix_owner_check(fs: &ext2::Ext2) {
    let path = "/owner_check.tmp";
    fs.write_path_owned(path, b"x", 1000, 1000).expect("create owner_check.tmp as uid 1000");
    let ino = fs.path_lookup(path).expect("find owner_check.tmp");
    let node = fs.read_inode(ino);
    assert_eq!((node.uid, node.gid), (1000, 1000), "write_path_owned didn't set the real owner");
    assert_eq!(node.mode & 0xF000, 0x8000, "a newly created file should be a regular file");

    // chmod policy (owner-or-root), mirrored from syscall::sys_chmod.
    let chmod_allowed = |caller_uid: u32| caller_uid == 0 || caller_uid == node.uid;
    assert!(chmod_allowed(1000), "the owner must be allowed to chmod their own file");
    assert!(chmod_allowed(0), "root must be allowed to chmod any file");
    assert!(!chmod_allowed(2000), "a non-owning, non-root uid must NOT be allowed to chmod");

    fs.chmod_path(path, 0o600).expect("chmod owner_check.tmp");
    let after_chmod = fs.read_inode(ino);
    assert_eq!(
        after_chmod.mode,
        0x8000 | 0o600,
        "chmod_path didn't set the new low bits (or clobbered the file-type nibble)"
    );

    // chown policy (root-only, stricter than chmod), mirrored from
    // syscall::sys_chown — even the owner can't give a file away.
    let chown_allowed = |caller_uid: u32| caller_uid == 0;
    assert!(chown_allowed(0), "root must be allowed to chown");
    assert!(!chown_allowed(1000), "even the owner must NOT be allowed to chown");

    fs.chown_path(path, 2000, 2000).expect("chown owner_check.tmp");
    let after_chown = fs.read_inode(ino);
    assert_eq!((after_chown.uid, after_chown.gid), (2000, 2000), "chown_path didn't change the owner");

    fs.unlink_path(path).ok();
    kprintln!("THOS: posix owner ok   O_CREAT ownership + chmod/chown policy (O_CREAT/mkdir also proven live in kbd-test)");
}

/// The group tier `Inode::access_ok` gained this increment — real Unix
/// owner/group/other, not just owner/other. A file owned `(1000, 1000)` at
/// mode `0640` (owner rw, group r, other none): the owner gets read+write;
/// a *different* uid that shares the file's gid gets the group bits
/// (read, not write); a uid matching neither the owning uid nor the owning
/// gid falls through to "other" and gets nothing (0640 has no other bits at
/// all). uid 0 always passes, any tier.
fn group_tier_check(fs: &ext2::Ext2) {
    let path = "/group_check.tmp";
    fs.write_path_owned(path, b"x", 1000, 1000).expect("create group_check.tmp");
    fs.chmod_path(path, 0o640).expect("chmod group_check.tmp to 0640");
    let ino = fs.path_lookup(path).expect("find group_check.tmp");
    let node = fs.read_inode(ino);

    assert!(node.access_ok(1000, 1000, true), "the owner must get the owner (rw) bits");
    assert!(node.access_ok(1000, 1000, false), "the owner must get read too");

    assert!(
        node.access_ok(2000, 1000, false),
        "a different uid sharing the file's gid must get the group (read) bits"
    );
    assert!(
        !node.access_ok(2000, 1000, true),
        "the group tier is read-only at 0640 — write must still be denied"
    );

    assert!(
        !node.access_ok(3000, 4000, false),
        "a uid matching neither the owning uid nor the owning gid must not fall through to access anyway"
    );

    assert!(node.access_ok(0, 0, true), "uid 0 must always pass, regardless of tier");

    fs.unlink_path(path).ok();
    kprintln!("THOS: group tier ok    owner/group/other DAC tiers distinguished (0640: owner rw, group r-only, other none)");
}

/// Phase 2 milestone: a VFS with an in-memory file opened through the handle
/// table, and the AHCI driver reading real sectors off the SATA disk.
/// Process isolation's other half, closing a gap `process.rs`'s own module
/// doc used to flag ("no address-space teardown (a reaper frees the frames
/// later)"): spawn and exit a batch of real user processes, force the
/// reaper (`sched::reap` — normally driven by an idle CPU's loop, called
/// here directly for a deterministic check) to run, and confirm the frame
/// count actually comes back down — not "compiles", a genuine check that a
/// terminated process's page tables and image/stack/heap frames are
/// reclaimed rather than left dangling (a resource leak, and a lingering
/// trace of a dead process's memory the isolation boundary is supposed to
/// have closed) and, in the shared-section case, that only the process's
/// *own* frames go back — a section still held elsewhere must survive.
fn process_teardown_check(bin: &[u8]) {
    // Warm up once first — the loader's own one-time lazy setup shouldn't be
    // mistaken for a per-process leak.
    let before_warmup = syscall::user_exits();
    process::spawn_init(bin, &["/rusthello"], &["THOS=1"]);
    while syscall::user_exits() < before_warmup + 1 {
        sched::yield_now();
    }
    for _ in 0..40 {
        sched::yield_now(); // let a still-switching-away thread finish (`finish_switch`
        sched::reap(); // clears `running`) before reap() re-checks it — same pattern
    } // the stress milestone's own reap-loop already uses.
    let baseline = mm::FRAME_ALLOC.lock().free_frames();

    const ROUNDS: u64 = 8;
    let start_exits = syscall::user_exits();
    for _ in 0..ROUNDS {
        process::spawn_init(bin, &["/rusthello"], &["THOS=1"]);
    }
    while syscall::user_exits() < start_exits + ROUNDS {
        sched::yield_now();
    }
    // `reap()` isn't driven automatically anywhere yet (a real reaper thread
    // is future work — see `sched::reap`'s doc comment); call it directly,
    // interleaved with `yield_now` so an exited thread's own CPU gets to run
    // `finish_switch` (clearing `running`) before reap() re-checks it.
    for _ in 0..40 {
        sched::yield_now();
        sched::reap();
    }
    let after = mm::FRAME_ALLOC.lock().free_frames();
    assert_eq!(
        after, baseline,
        "process teardown leaked frames: {ROUNDS} processes spawned+exited, {baseline} -> {after} free frames"
    );
    kprintln!("THOS: proc teardown ok {ROUNDS} processes spawned+exited, {baseline} free frames unchanged");
}

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

    ahci::init().expect("AHCI init"); // logs "THOS: ahci ident ..."
    let mut sb = [0u8; ahci::SECTOR];
    ahci::read(2, &mut sb).expect("AHCI read LBA 2");
    let magic = u16::from_le_bytes([sb[56], sb[57]]);
    kprintln!("THOS: ahci ok          LBA 2 read; ext2 magic {:#06x}", magic);

    // Capacity from IDENTIFY: the disk must hold the fs, and a read one sector
    // past the end must be rejected by the bounds check (not the drive).
    let cap = ahci::capacity_sectors();
    assert!(cap >= 32_768, "disk reports only {cap} sectors — smaller than the fs");
    assert!(ahci::read(cap, &mut [0u8; ahci::SECTOR]).is_err(), "read past EOD not rejected");
    kprintln!("THOS: ahci cap ok      {} sectors; out-of-range read rejected", cap);

    // GPT + FAT: a self-contained GPT image (protective MBR, one ESP holding a
    // FAT32 volume) is spliced in at LBA 51000, past the ext2 image (see xtask
    // `disk_image`). Find the ESP by type GUID, mount it, read a known file —
    // the full "read the ESP" path: GPT → partition → BPB → FAT chain → 8.3.
    match gpt::find_esp(51_000) {
        Some(esp_lba) => {
            kprintln!("THOS: gpt ok           ESP at LBA {}", esp_lba);
            match fat::Fat::open(esp_lba).and_then(|v| {
                v.read_path("/EFI/THOS/HELLO.TXT").ok_or("file not found")
            }) {
                Ok(b) => {
                    kprintln!("THOS: fat ok           /EFI/THOS/HELLO.TXT = {} bytes", b.len());
                    serial::write_bytes(&b);
                }
                Err(e) => kprintln!("THOS: fat FAIL         {}", e),
            }
        }
        None => kprintln!("THOS: gpt FAIL         no ESP found at LBA 51000"),
    }

    let fs = ext2::open().expect("mount ext2");

    #[cfg(feature = "regcrashtest")]
    registry_crash_check(&fs);

    // Before anything can touch the registry (PE syscalls included).
    let loaded_hives = registry::load_hives(&fs);
    kprintln!(
        "THOS: registry ok      {}/3 hives loaded from disk{}",
        loaded_hives,
        if loaded_hives == 0 { " (first boot — defaults seeded)" } else { "" }
    );
    registry_enum_check();
    registry_security_check(&fs);
    registry_notify_check();
    // Before the exec gate's own check — a real, isolated userspace
    // process, not kernel code (`secsvc.rs`'s own module doc). Its exact
    // shutdown/crash behavior is what `execgate_check` below exercises.
    secsvc::spawn(&fs);
    execgate_check();
    secsvc_check(&fs);
    section_sharing_check(&fs);
    integrity_check(&fs);
    posix_owner_check(&fs);
    group_tier_check(&fs);
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
    process_teardown_check(&rs);

    // Milestone 2: an unmodified stock static BusyBox. Feature-gated — reading
    // the 2 MiB binary a block at a time makes every boot noticeably slower.
    #[cfg(feature = "bbtest")]
    {
        let bb = fs.read_path("/busybox").expect("read /busybox from ext2");
        kprintln!("THOS: ext2 ok          /busybox = {} bytes", bb.len());
        let before = syscall::user_exits();
        process::spawn_init(
            &bb,
            &["busybox", "echo", "THOS: busybox says hello"],
            &["PATH=/", "HOME=/"],
        );
        while syscall::user_exits() <= before {
            sched::yield_now();
        }
        kprintln!("THOS: busybox ok       stock static BusyBox ran unmodified");
    }

    // Pipes + command substitution: BusyBox `sh -c` with a `|` and two `$(…)`.
    // `pipe2` + O_CLOEXEC + pipe EOF all have to work for this exact line.
    #[cfg(feature = "pipetest")]
    {
        let bb = fs.read_path("/busybox").expect("read /busybox from ext2");
        let pid = process::spawn_init(
            &bb,
            &[
                "sh",
                "-c",
                "echo THOS-PIPE $(ls /bin | grep -c sleep) sub-$(echo works)",
            ],
            &["PATH=/bin:/", "HOME=/"],
        );
        while !process::pid_exited(pid) {
            sched::yield_now();
        }
        kprintln!("THOS: pipe ok          `|` and `$(…)` through BusyBox sh");
    }

    // Phase 3: a statically linked Win64 `.exe` loaded by the native PE loader
    // (no Wine, no ntdll yet — it only makes syscalls). Milestone 3 headline in
    // miniature: ELF and PE containers, same ring-3 kernel.
    #[cfg(feature = "petest")]
    {
        let exe = fs.read_path("/pe-hello.exe").expect("read /pe-hello.exe from ext2");
        kprintln!("THOS: pe ok            /pe-hello.exe = {} bytes", exe.len());
        let pid = process::spawn_pe(&exe).expect("spawn_pe: /pe-hello.exe rejected");
        while !process::pid_exited(pid) {
            sched::yield_now();
        }
        kprintln!("THOS: pe exited        native PE64 ran to exit");

        // A DLL whose DllMain(DLL_PROCESS_ATTACH) returns FALSE must abort
        // process init — the exe entry (which prints "PE DLLFAIL REACHED
        // ENTRY") never runs. `cargo xtask pe-test` asserts that line is absent.
        let df = fs.read_path("/pe-dllfail.exe").expect("read /pe-dllfail.exe from ext2");
        let dpid = process::spawn_pe(&df).expect("spawn_pe: /pe-dllfail.exe rejected");
        while !process::pid_exited(dpid) {
            sched::yield_now();
        }
        kprintln!("THOS: pe dllfail ok    DllMain FALSE aborted init");

        // Hostile input: a truncated / garbage PE must be rejected cleanly, not
        // panic the kernel.
        let mut junk = Vec::from(&exe[..exe.len().min(200)]);
        junk[0x3C] = 0xFF; // point e_lfanew off the end
        assert!(process::spawn_pe(&junk).is_err(), "malformed PE was not rejected");
        assert!(process::spawn_pe(b"MZ\x90\x00not really a pe").is_err());
        kprintln!("THOS: pe reject ok     malformed PEs rejected, kernel alive");

        // The native-exec gate: an otherwise perfectly valid, already-tested
        // PE (this same /pe-hello.exe) carrying the EICAR test string
        // anywhere in it must be quarantined before `pe::load` even parses
        // a header — the well-formedness of the container is irrelevant to
        // the gate, only its content. `exe` on its own already proved this
        // exact byte sequence runs fine (`THOS: pe exited` above), so a
        // rejection here can only be the gate, not a malformed-file fluke.
        let mut eicar_pe = exe.clone();
        eicar_pe.extend_from_slice(b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*");
        assert!(process::spawn_pe(&eicar_pe).is_err(), "EICAR-laced PE was not quarantined");
        kprintln!("THOS: exec gate ok     EICAR-signature PE quarantined, kernel alive");

        // Milestone 3: a real mingw-w64 compiler-produced Win32 console `.exe`
        // (own entry, only KERNEL32 imports) runs through the NT path.
        let wc = fs.read_path("/wincon.exe").expect("read /wincon.exe from ext2");
        kprintln!("THOS: wincon ok        /wincon.exe = {} bytes (mingw-w64)", wc.len());
        let wpid = process::spawn_pe(&wc).expect("spawn_pe: /wincon.exe rejected");
        while !process::pid_exited(wpid) {
            sched::yield_now();
        }
        kprintln!("THOS: wincon exited    real toolchain PE ran to exit");

        // Milestone 3+: a full mingw CRT `int main` .exe — imports msvcrt.dll,
        // runs against THOS's synthetic C runtime (printf / __getmainargs / ...).
        let ce = fs.read_path("/crt.exe").expect("read /crt.exe from ext2");
        kprintln!("THOS: crt ok           /crt.exe = {} bytes (mingw CRT)", ce.len());
        let cpid = process::spawn_pe(&ce).expect("spawn_pe: /crt.exe rejected");
        while !process::pid_exited(cpid) {
            sched::yield_now();
        }
        kprintln!("THOS: crt exited       mingw C runtime ran to exit");

        // ELF and PE processes in one `ps` view.
        process::ps_dump();
    }

    // AHCI write: round-trip a known pattern through a scratch sector past the
    // ext2 image (LBA 50000 = ~25 MiB; the fs is the first 16 MiB). The host
    // side of `cargo xtask ahci-test` re-checks this landed in the disk file.
    // Fixed-LBA scratch writes are test-image-only: on a real MBR disk those
    // sectors lie inside the user's root partition.
    if ext2::on_partition() {
        kprintln!("THOS: ahci scratch skip real disk — destructive LBA tests not run");
    } else {
        const SCRATCH_LBA: u64 = 50_000;
        let mut wbuf = [0u8; ahci::SECTOR];
        for (i, b) in wbuf.iter_mut().enumerate() {
            *b = (i as u8) ^ 0xA5;
        }
        ahci::write(SCRATCH_LBA, &wbuf).expect("AHCI write");
        let mut rbuf = [0u8; ahci::SECTOR];
        ahci::read(SCRATCH_LBA, &mut rbuf).expect("AHCI read-back");
        assert!(rbuf == wbuf, "AHCI write / read-back mismatch");
        kprintln!("THOS: ahci write ok    LBA {} round-tripped (durable)", SCRATCH_LBA);

        // Concurrent NCQ: 8 threads hammer distinct scratch regions at once, so
        // several tags are outstanding and the drive reorders them.
        for i in 0..8 {
            sched::spawn("ncq-io", ncq_io_worker, i);
        }
        while NCQ_DONE.load(Ordering::Relaxed) < 8 {
            sched::yield_now();
        }
        assert_eq!(NCQ_BAD.load(Ordering::Relaxed), 0, "concurrent NCQ I/O corrupted data");
        kprintln!(
            "THOS: ahci ncq ok      8 concurrent readers/writers verified (depth {}, {} completion IRQs)",
            ahci::queue_depth(),
            ahci::irq_count(),
        );

    }

    // ext2 write: create a file + a dir + a nested file, read them back through
    // our own read path. `cargo xtask ext2-test` then e2fsck's the image and
    // cat's the files from the host to prove it is a valid on-disk ext2.
    {
        let fs = ext2::open().expect("remount ext2");
        let payload = b"ext2 write works on THOS\n";
        fs.write_path("/thos-created.txt", payload).expect("ext2 write_path");
        match fs.mkdir_path("/thosdir") {
            Ok(()) | Err("already exists") => {} // idempotent: disk.img is reused
            Err(e) => panic!("ext2 mkdir_path: {e}"),
        }
        fs.write_path("/thosdir/nested.txt", b"nested ok\n").expect("ext2 nested write");
        assert!(fs.read_path("/thos-created.txt").as_deref() == Some(payload.as_slice()));
        assert!(fs.read_path("/thosdir/nested.txt").as_deref() == Some(b"nested ok\n".as_slice()));
        kprintln!("THOS: ext2 write ok    /thos-created.txt + /thosdir/nested.txt");

        // unlink / rmdir: make a throwaway file + dir, delete them, prove gone.
        fs.write_path("/thos-temp.txt", b"delete me\n").expect("ext2 temp write");
        let _ = fs.mkdir_path("/thos-tmpdir");
        assert!(fs.rmdir_path("/thosdir") == Err("directory not empty"));
        fs.unlink_path("/thosdir/nested.txt").expect("ext2 unlink nested");
        fs.rmdir_path("/thosdir").expect("ext2 rmdir");
        fs.unlink_path("/thos-temp.txt").expect("ext2 unlink");
        fs.rmdir_path("/thos-tmpdir").expect("ext2 rmdir tmpdir");
        assert!(fs.read_path("/thos-temp.txt").is_none());
        assert!(fs.path_lookup("/thosdir").is_none());
        assert!(fs.unlink_path("/thos-temp.txt") == Err("no such file"));
        kprintln!("THOS: ext2 unlink ok   removed files + dirs, backups re-synced");
    }

    #[cfg(feature = "stress")]
    smp_stress_milestone(&init);

    #[cfg(feature = "faulttest")]
    {
        // `cargo xtask ncq-error-test` runs QEMU with blkdebug poisoning one
        // read of this LBA. The read must fail *cleanly* (no hang, no panic),
        // recovery must run, and a retry must succeed on the restarted port.
        const BAD_LBA: u64 = 41_000;
        let mut b = [0u8; ahci::SECTOR];
        let r = ahci::read(BAD_LBA, &mut b);
        assert!(r.is_err(), "poisoned read did not surface an error: {r:?}");
        assert!(ahci::recover_count() >= 1, "error was not run through recovery");
        kprintln!(
            "THOS: ncq error ok     poisoned read -> {:?}; {} recovery pass(es), no hang",
            r.unwrap_err(),
            ahci::recover_count(),
        );
    }

    start_input_devices();
}

/// Bring up every input device that can feed the console: USB keyboard via
/// xHCI (if the machine has one) and the i8042 PS/2 keyboard (the laptop
/// path). Each runs a polling thread that feeds `console::feed_report`.
fn start_input_devices() {
    match xhci::init() {
        Ok(x) => {
            *XHCI.lock() = Some(x);
            sched::spawn("xhci-poll", xhci_poll_thread, 0);
            kprintln!("THOS: xhci ok          USB keyboard attached (poll thread up)");
        }
        Err(e) => kprintln!("THOS: xhci             {}", e),
    }

    // PS/2 keyboard via the i8042 — the laptop target has no xHCI at all.
    match ps2::init() {
        Ok(()) => {
            sched::spawn("ps2-poll", ps2_poll_thread, 0);
            kprintln!("THOS: ps2 ok           i8042 keyboard attached (poll thread up)");
        }
        Err(e) => kprintln!("THOS: ps2              {}", e),
    }
}

/// Normal (non-`selftest`) boot after the scheduler is up: disk, root
/// filesystem, registry hives, the Security Service, input devices.
#[cfg(not(feature = "selftest"))]
fn boot_system() {
    if let Err(e) = ahci::init() {
        fatal_boot("no usable SATA disk (BIOS SATA mode must be AHCI)", e);
    }
    let fs = ext2::open().unwrap_or_else(|e| fatal_boot("cannot mount the root filesystem", e));
    let hives = registry::load_hives(&fs);
    kprintln!("THOS: registry ok      {}/3 hives loaded from disk", hives);
    secsvc::spawn(&fs); // logs and degrades gracefully if /secsvc is absent
    start_input_devices();
}

/// A boot failure the user can act on: say what is wrong, then stop. (This is
/// the screen the user sees on a real machine now that the framebuffer console
/// mirrors the kernel log — so it must not be a bare `expect` panic.)
#[allow(dead_code)] // only the normal boot path and the interactive login use it
fn fatal_boot(what: &str, why: &str) -> ! {
    kprintln!("");
    kprintln!("THOS: cannot continue — {what}");
    kprintln!("THOS:   {why}");
    exit_qemu(ExitCode::Failed);
    hcf();
}

// --- concurrent NCQ I/O check (storage_milestone) ---
static NCQ_DONE: AtomicU64 = AtomicU64::new(0);
static NCQ_BAD: AtomicU64 = AtomicU64::new(0);

extern "C" fn ncq_io_worker(i: usize) -> ! {
    let lba = 40_000 + i as u64; // a distinct scratch sector per worker, past the fs
    let mut w = [0u8; ahci::SECTOR];
    for (j, b) in w.iter_mut().enumerate() {
        *b = (j as u8).wrapping_add((i as u8).wrapping_mul(17));
    }
    for _ in 0..16 {
        let mut r = [0u8; ahci::SECTOR];
        if ahci::write(lba, &w).is_err() || ahci::read(lba, &mut r).is_err() || r != w {
            NCQ_BAD.fetch_add(1, Ordering::Relaxed);
            break;
        }
    }
    NCQ_DONE.fetch_add(1, Ordering::Relaxed);
    sched::exit()
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

extern "C" fn ps2_poll_thread(_: usize) -> ! {
    let mut dec = ps2::Decoder::new();
    loop {
        while let Some(b) = ps2::read_scancode() {
            if let Some(r) = dec.feed(b) {
                console::feed_report(&r);
            }
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
