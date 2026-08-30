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
#[cfg(feature = "interactive")]
mod cred;
mod elf;
mod ext2;
mod file;
mod gdt;
mod idt;
#[cfg(feature = "interactive")]
mod login;
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

    #[cfg(feature = "interactive")]
    {
        // Milestone 2: first-run setup / login, then launch the shell off ext2
        // and hand it the USB keyboard.
        let fs = ext2::open().expect("mount ext2 for the shell");
        let session = login::establish(&fs);
        process::set_session(&session.name, session.uid);
        kprintln!("THOS: session          {} (uid {})", session.name, session.uid);

        let sh = fs.read_path("/sh").expect("read /sh from ext2");
        kprintln!("THOS: shell            /sh = {} bytes", sh.len());
        process::spawn_init(&sh, &["/sh"], &["PATH=/", "HOME=/"]);

        kprintln!("THOS: interactive hold — type on the USB keyboard");
        loop {
            sched::yield_now();
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
        "THOS: smp stress ok    {spawned} churn + {PARKERS} parker threads clean; {runs}+{park_runs} runs, {} ctx switches",
        sched::ctx_switches()
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

    // AHCI write: round-trip a known pattern through a scratch sector past the
    // ext2 image (LBA 50000 = ~25 MiB; the fs is the first 16 MiB). The host
    // side of `cargo xtask ahci-test` re-checks this landed in the disk file.
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
