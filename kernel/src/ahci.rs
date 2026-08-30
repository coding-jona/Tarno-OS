// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — AHCI / SATA block driver.
//!
//! The target machine boots THOS from the Kingston A400 SATA SSD behind the
//! Intel Raptor Lake AHCI controller (`8086:7a62`); AHCI 1.3.1 is a standard
//! register interface, so this is portable.
//!
//! Layout: one frame holds the 32-entry command list + the received-FIS area;
//! two more frames hold 32 command tables (16 each, `0x100` stride). Each I/O
//! takes a free **tag**, builds that tag's table, and — when the drive supports
//! NCQ — issues `READ`/`WRITE FPDMA QUEUED` via `PxSACT` + `PxCI`, so up to
//! `queue depth` transfers run at once and the drive reorders them.
//!
//! Completion is **interrupt-driven** when the controller exposes MSI-X (or
//! MSI): the HBA's completion interrupt wakes the blocked submitter, which
//! re-checks `PxSACT`/`PxCI`. A timer-driven poll is kept as a safety net, and
//! without MSI at all it degrades to polite `yield` polling.

use core::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use spin::Mutex;

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::wait::WaitQueue;
use crate::{apic, pci, sched};

pub const SECTOR: usize = 512;

// HBA_MEM
const CAP: usize = 0x00;
const GHC: usize = 0x04;
const HBA_IS: usize = 0x08;
const PI: usize = 0x0C;
const GHC_AE: u32 = 1 << 31;
const GHC_IE: u32 = 1 << 1;
const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;

// HBA_PORT (relative to port base)
const P_CLB: usize = 0x00;
const P_CLBU: usize = 0x04;
const P_FB: usize = 0x08;
const P_FBU: usize = 0x0C;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SERR: usize = 0x30;
const P_SACT: usize = 0x34;
const P_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;

const TFD_BSY: u32 = 1 << 7;
const TFD_DRQ: u32 = 1 << 3;
const TFD_ERR: u32 = 1 << 0;
const IS_TFES: u32 = 1 << 30;

const SIG_SATA: u32 = 0x0000_0101;

const FIS_TYPE_H2D: u8 = 0x27;
const CMD_READ_DMA_EX: u8 = 0x25;
const CMD_WRITE_DMA_EX: u8 = 0x35;
const CMD_READ_FPDMA: u8 = 0x60;
const CMD_WRITE_FPDMA: u8 = 0x61;
const CMD_FLUSH_CACHE_EX: u8 = 0xEA;
const CMD_IDENTIFY: u8 = 0xEC;

const MAX_TAGS: u8 = 32;
const POLL_LIMIT: u32 = 4_000_000;

// --- controller state (set once in `init`, then lock-free so the IRQ handler
//     never contends on a mutex) ---
static HBA_BASE: AtomicU64 = AtomicU64::new(0);
static PORT: AtomicUsize = AtomicUsize::new(0);
static CLB_PHYS: AtomicU64 = AtomicU64::new(0);
static CTAB_PHYS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static SECTORS: AtomicU64 = AtomicU64::new(0);
static DEPTH: AtomicU32 = AtomicU32::new(1);
static IRQ_ON: AtomicBool = AtomicBool::new(false);

/// Held only for the ~µs command build+issue, never across a wait.
static SUBMIT: Mutex<()> = Mutex::new(());
/// One bit per free tag, populated from the drive's queue depth in `init`.
static FREE_TAGS: AtomicU32 = AtomicU32::new(0);
/// One bit per tag whose submitter is currently parked in `wait`.
static INFLIGHT: AtomicU32 = AtomicU32::new(0);
/// Completion interrupts actually taken (vs. the timer safety net doing the work).
static IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
/// A parked submitter blocks here; the IRQ (and the timer safety net) wake it.
static TAG_WAKE: [WaitQueue; 32] = [const { WaitQueue::new() }; 32];

struct Hba {
    base: u64,
}
impl Hba {
    fn cur() -> Self {
        Self { base: HBA_BASE.load(Ordering::Relaxed) }
    }
    fn r(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base as usize + off) as *const u32) }
    }
    fn w(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.base as usize + off) as *mut u32, v) }
    }
    fn pr(&self, off: usize) -> u32 {
        self.r(PORT_BASE + PORT.load(Ordering::Relaxed) * PORT_STRIDE + off)
    }
    fn pw(&self, off: usize, v: u32) {
        self.w(PORT_BASE + PORT.load(Ordering::Relaxed) * PORT_STRIDE + off, v);
    }
}

fn ctab(tag: u8) -> u64 {
    CTAB_PHYS[(tag >> 4) as usize].load(Ordering::Relaxed) + ((tag & 0xF) as u64) * 0x100
}

fn alloc_tag() -> u8 {
    loop {
        let cur = FREE_TAGS.load(Ordering::Acquire);
        if cur != 0 {
            let t = cur.trailing_zeros();
            if FREE_TAGS
                .compare_exchange_weak(cur, cur & !(1u32 << t), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return t as u8;
            }
        } else {
            sched::yield_now(); // queue full — wait for a completion
        }
    }
}

fn free_tag(t: u8) {
    FREE_TAGS.fetch_or(1u32 << t, Ordering::Release);
}

/// Probe PCI, enable the HBA, bring up the first SATA port, `IDENTIFY` it, set
/// up the command list + tables and (if available) MSI-X / MSI completion
/// interrupts. `Ok` on a working port.
pub fn init() -> Result<(), &'static str> {
    let loc = pci::find_ahci().ok_or("no AHCI controller on bus 0")?;
    pci::enable_bus_master(loc);
    let abar = pci::bar(loc, 5);
    if abar == 0 {
        return Err("AHCI BAR5 is zero");
    }
    let base = crate::vmm::map_mmio(abar, 0x2000);
    HBA_BASE.store(base, Ordering::Relaxed);
    let hba = Hba { base };
    hba.w(GHC, hba.r(GHC) | GHC_AE);

    let pi = hba.r(PI);
    let port = (0..32)
        .find(|&p| {
            let ps = |o| hba.r(PORT_BASE + p * PORT_STRIDE + o);
            pi & (1 << p) != 0 && ps(P_SSTS) & 0x0F == 3 && ps(P_SIG) == SIG_SATA
        })
        .ok_or("no SATA device on any implemented port")?;
    PORT.store(port, Ordering::Relaxed);

    // Stop the port before repointing CLB/FB.
    let mut cmd = hba.pr(P_CMD) & !(CMD_ST | CMD_FRE);
    hba.pw(P_CMD, cmd);
    for _ in 0..1_000_000 {
        if hba.pr(P_CMD) & (CMD_CR | CMD_FR) == 0 {
            break;
        }
    }

    let alloc_zeroed = || -> Option<u64> {
        let f = FRAME_ALLOC.lock().alloc()?;
        unsafe {
            core::ptr::write_bytes(phys_to_virt(f.start_address()).as_mut_ptr::<u8>(), 0, 4096);
        }
        Some(f.start_address().as_u64())
    };
    let clb = alloc_zeroed().ok_or("no frame for AHCI cmd list")?;
    CLB_PHYS.store(clb, Ordering::Relaxed);
    CTAB_PHYS[0].store(alloc_zeroed().ok_or("no frame for AHCI cmd tables")?, Ordering::Relaxed);
    CTAB_PHYS[1].store(alloc_zeroed().ok_or("no frame for AHCI cmd tables")?, Ordering::Relaxed);

    hba.pw(P_CLB, clb as u32);
    hba.pw(P_CLBU, (clb >> 32) as u32);
    hba.pw(P_FB, (clb + 0x400) as u32);
    hba.pw(P_FBU, ((clb + 0x400) >> 32) as u32);
    hba.pw(P_SERR, 0xFFFF_FFFF);
    hba.pw(P_IS, 0xFFFF_FFFF);
    hba.w(HBA_IS, hba.r(HBA_IS));

    cmd = hba.pr(P_CMD) | CMD_FRE;
    hba.pw(P_CMD, cmd);
    hba.pw(P_CMD, cmd | CMD_ST);

    let _ = CAP;

    // IDENTIFY DEVICE — capacity, model, NCQ support (word 76 bit 8) + queue
    // depth (word 75 bits 0..4, zero-based). Non-queued on tag 0, nothing else
    // running yet.
    let id = identify()?;
    let word = |w: usize| u16::from_le_bytes([id[2 * w], id[2 * w + 1]]);
    let lba48 = word(83) & (1 << 10) != 0;
    let s48 = u64::from_le_bytes(id[200..208].try_into().unwrap());
    let s28 = u32::from_le_bytes(id[120..124].try_into().unwrap()) as u64;
    let sectors = if lba48 && s48 != 0 { s48 } else { s28 };
    if sectors == 0 {
        return Err("AHCI IDENTIFY: zero sector count");
    }
    let depth = if word(76) & (1 << 8) != 0 {
        (((word(75) & 0x1F) + 1) as u8).clamp(2, MAX_TAGS)
    } else {
        1
    };
    SECTORS.store(sectors, Ordering::Relaxed);
    DEPTH.store(depth as u32, Ordering::Relaxed);
    FREE_TAGS.store(((1u64 << depth) - 1) as u32, Ordering::Release);

    // MSI-X (preferred) / MSI completion interrupts.
    let irq = setup_irq(loc);
    if irq != "polled (no MSI)" {
        hba.pw(P_IE, 0xFFFF_FFFF); // enable every port interrupt source
        hba.w(GHC, hba.r(GHC) | GHC_IE);
        IRQ_ON.store(true, Ordering::Release);
    }

    let mut model = [b' '; 40]; // words 27..46, byte-swapped per word
    for i in 0..20 {
        model[i * 2] = id[54 + i * 2 + 1];
        model[i * 2 + 1] = id[54 + i * 2];
    }
    let model = core::str::from_utf8(&model).unwrap_or("?").trim();
    crate::kprintln!(
        "THOS: ahci ident       {} — {} sectors ({} MiB); {}",
        model,
        sectors,
        sectors * SECTOR as u64 / (1024 * 1024),
        if depth > 1 { "NCQ" } else { "no NCQ" },
    );
    crate::kprintln!("THOS: ahci irq         {}; queue depth {}", irq, depth);
    Ok(())
}

pub fn capacity_sectors() -> u64 {
    SECTORS.load(Ordering::Relaxed)
}

pub fn queue_depth() -> u8 {
    DEPTH.load(Ordering::Relaxed) as u8
}

/// How many AHCI completion interrupts have been taken.
pub fn irq_count() -> u64 {
    IRQ_COUNT.load(Ordering::Relaxed)
}

// --- interrupt setup + handler ---

/// Program the device's MSI-X (else MSI) capability to raise `AHCI_VECTOR` on
/// the BSP and disable legacy INTx. Returns which mechanism was armed.
fn setup_irq(loc: pci::Location) -> &'static str {
    let vector = apic::AHCI_VECTOR as u32;
    let addr = 0xFEE0_0000u32 | ((apic::bsp_apic_id() as u32) << 12); // x86 MSI address

    let armed = if let Some(cap) = pci::find_cap(loc, 0x11) {
        // MSI-X: entry 0 lives in a BAR named by the table-offset word's BIR.
        let tbl = pci::read32(loc, cap + 4);
        let bar = pci::bar(loc, (tbl & 0b111) as u8);
        if bar == 0 {
            "polled (no MSI)"
        } else {
            let t = crate::vmm::map_mmio(bar + (tbl & !0b111) as u64, 0x1000) as *mut u32;
            unsafe {
                core::ptr::write_volatile(t.add(0), addr);
                core::ptr::write_volatile(t.add(1), 0);
                core::ptr::write_volatile(t.add(2), vector);
                core::ptr::write_volatile(t.add(3), 0); // vector control: unmasked
            }
            let ctrl = pci::read16(loc, cap + 2);
            pci::write16(loc, cap + 2, (ctrl | (1 << 15)) & !(1 << 14)); // enable, unmask fn
            "MSI-X completion"
        }
    } else if let Some(cap) = pci::find_cap(loc, 0x05) {
        // MSI: address/data live in config space directly.
        let ctrl = pci::read16(loc, cap + 2);
        if ctrl & (1 << 7) != 0 {
            pci::write32(loc, cap + 4, addr);
            pci::write32(loc, cap + 8, 0);
            pci::write16(loc, cap + 0x0C, vector as u16);
        } else {
            pci::write32(loc, cap + 4, addr);
            pci::write16(loc, cap + 8, vector as u16);
        }
        pci::write16(loc, cap + 2, (ctrl & !(0b111 << 4)) | 1); // 1 vector, enable
        "MSI completion"
    } else {
        "polled (no MSI)"
    };

    if armed != "polled (no MSI)" {
        let cmd = pci::read16(loc, 0x04);
        pci::write16(loc, 0x04, cmd | (1 << 10)); // disable legacy INTx
    }
    armed
}

/// AHCI completion interrupt: clear the status latches and nudge every parked
/// submitter to re-check its tag.
pub fn on_irq() {
    IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    let hba = Hba::cur();
    let g = hba.r(HBA_IS);
    if g != 0 {
        hba.w(HBA_IS, g);
    }
    let p = hba.pr(P_IS);
    if p != 0 {
        hba.pw(P_IS, p);
    }
    wake_parked();
}

/// Timer safety net (called from the APIC tick): covers a dropped interrupt.
pub fn poll_wake() {
    if INFLIGHT.load(Ordering::Relaxed) != 0 {
        wake_parked();
    }
}

fn wake_parked() {
    let mut m = INFLIGHT.load(Ordering::Acquire);
    while m != 0 {
        let t = m.trailing_zeros();
        TAG_WAKE[t as usize].wake_all();
        m &= m - 1;
    }
}

// --- command path ---

fn wait_ready(hba: &Hba) {
    for _ in 0..1_000_000 {
        if hba.pr(P_TFD) & (TFD_BSY | TFD_DRQ) == 0 {
            return;
        }
        sched::yield_now();
    }
}

/// Build tag `tag`'s command header + table and issue it. Caller holds `SUBMIT`.
fn issue(tag: u8, ncq: bool, is_write: bool, buf: Option<(u64, u32)>, fill_fis: impl FnOnce(*mut u8)) {
    let hba = Hba::cur();
    let hdr = phys_to_virt(x86_64::PhysAddr::new(
        CLB_PHYS.load(Ordering::Relaxed) + tag as u64 * 32,
    ))
    .as_mut_ptr::<u32>();
    let ctab_phys = ctab(tag);
    let ct = phys_to_virt(x86_64::PhysAddr::new(ctab_phys)).as_mut_ptr::<u8>();

    unsafe {
        let prdtl = u32::from(buf.is_some());
        let w = if is_write { 1u32 << 6 } else { 0 };
        core::ptr::write_volatile(hdr.add(0), 5 | w | (prdtl << 16)); // cfl = 5 dwords
        core::ptr::write_volatile(hdr.add(1), 0);
        core::ptr::write_volatile(hdr.add(2), ctab_phys as u32);
        core::ptr::write_volatile(hdr.add(3), (ctab_phys >> 32) as u32);

        core::ptr::write_bytes(ct, 0, 0x80 + 16);
        if let Some((phys, bytes)) = buf {
            let prdt = ct.add(0x80) as *mut u32;
            core::ptr::write_volatile(prdt.add(0), phys as u32);
            core::ptr::write_volatile(prdt.add(1), (phys >> 32) as u32);
            core::ptr::write_volatile(prdt.add(2), 0);
            core::ptr::write_volatile(prdt.add(3), bytes - 1);
        }
        fill_fis(ct);
    }

    fence(Ordering::SeqCst);
    if ncq {
        hba.pw(P_SACT, 1u32 << tag);
    }
    hba.pw(P_CI, 1u32 << tag);
}

/// Wait for tag `tag` to complete. `blocking` parks on the tag's wait queue
/// (woken by the completion IRQ); otherwise it polls with `yield`.
fn wait(tag: u8, ncq: bool, blocking: bool) -> Result<(), &'static str> {
    let hba = Hba::cur();
    let mask = 1u32 << tag;
    if blocking {
        INFLIGHT.fetch_or(mask, Ordering::AcqRel);
    }
    let r = (|| {
        for _ in 0..POLL_LIMIT {
            let is = hba.pr(P_IS);
            if is & IS_TFES != 0 || hba.pr(P_TFD) & TFD_ERR != 0 {
                hba.pw(P_IS, is);
                return Err("AHCI task-file error");
            }
            let done = if ncq {
                hba.pr(P_SACT) & mask == 0
            } else {
                hba.pr(P_CI) & mask == 0
            };
            if done {
                fence(Ordering::SeqCst);
                return Ok(());
            }
            if blocking {
                TAG_WAKE[tag as usize].wait();
            } else {
                sched::yield_now();
            }
        }
        Err("AHCI command timed out")
    })();
    if blocking {
        INFLIGHT.fetch_and(!mask, Ordering::Release);
    }
    r
}

fn transfer(is_write: bool, lba: u64, sectors: u16, buf_phys: u64) -> Result<(), &'static str> {
    if HBA_BASE.load(Ordering::Relaxed) == 0 {
        return Err("AHCI not initialised");
    }
    let cap = SECTORS.load(Ordering::Relaxed);
    if cap != 0 && lba + sectors as u64 > cap {
        return Err("AHCI: LBA past end of disk");
    }
    let ncq = DEPTH.load(Ordering::Relaxed) > 1;
    let bytes = sectors as u32 * SECTOR as u32;
    let tag = alloc_tag();

    {
        wait_ready(&Hba::cur());
        let _s = SUBMIT.lock();
        issue(tag, ncq, is_write, Some((buf_phys, bytes)), fis(is_write, ncq, lba, sectors, tag));
    }
    let r = wait(tag, ncq, IRQ_ON.load(Ordering::Relaxed));
    free_tag(tag);
    r
}

/// FIS builder for `READ`/`WRITE` (`FPDMA QUEUED` when `ncq`, else `DMA EXT`).
fn fis(is_write: bool, ncq: bool, lba: u64, sectors: u16, tag: u8) -> impl FnOnce(*mut u8) {
    move |f| unsafe {
        f.add(0).write_volatile(FIS_TYPE_H2D);
        f.add(1).write_volatile(1 << 7); // C
        for (i, b) in [
            lba as u8,
            (lba >> 8) as u8,
            (lba >> 16) as u8,
            (lba >> 24) as u8,
            (lba >> 32) as u8,
            (lba >> 40) as u8,
        ]
        .into_iter()
        .enumerate()
        {
            f.add(if i < 3 { 4 + i } else { 5 + i }).write_volatile(b); // LBA bytes [4,5,6][8,9,10]
        }
        if ncq {
            f.add(2).write_volatile(if is_write { CMD_WRITE_FPDMA } else { CMD_READ_FPDMA });
            f.add(3).write_volatile(sectors as u8); // features 7:0  = count low
            f.add(11).write_volatile((sectors >> 8) as u8); // features 15:8 = count high
            f.add(7).write_volatile(if is_write { 0xC0 } else { 0x40 }); // LBA (+FUA on write)
            f.add(12).write_volatile(tag << 3); // sector count 7:3 = tag
        } else {
            f.add(2).write_volatile(if is_write { CMD_WRITE_DMA_EX } else { CMD_READ_DMA_EX });
            f.add(7).write_volatile(1 << 6); // LBA mode
            f.add(12).write_volatile(sectors as u8);
            f.add(13).write_volatile((sectors >> 8) as u8);
        }
    }
}

/// Read `buf.len() / 512` sectors starting at `lba` into `buf`.
pub fn read(lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
    assert!(buf.len() % SECTOR == 0 && !buf.is_empty() && buf.len() <= 4096);
    let sectors = (buf.len() / SECTOR) as u16;
    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let phys = frame.start_address();
    let r = transfer(false, lba, sectors, phys.as_u64());
    if r.is_ok() {
        unsafe {
            core::ptr::copy_nonoverlapping(phys_to_virt(phys).as_ptr::<u8>(), buf.as_mut_ptr(), buf.len());
        }
    }
    FRAME_ALLOC.lock().dealloc(frame);
    r
}

/// Write `buf.len() / 512` sectors at `lba`, durably: the NCQ path sets FUA,
/// the legacy path issues `FLUSH CACHE EXT` afterwards.
pub fn write(lba: u64, buf: &[u8]) -> Result<(), &'static str> {
    assert!(buf.len() % SECTOR == 0 && !buf.is_empty() && buf.len() <= 4096);
    let sectors = (buf.len() / SECTOR) as u16;
    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let phys = frame.start_address();
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), phys_to_virt(phys).as_mut_ptr::<u8>(), buf.len());
    }
    let mut r = transfer(true, lba, sectors, phys.as_u64());
    if r.is_ok() && DEPTH.load(Ordering::Relaxed) == 1 {
        r = flush();
    }
    FRAME_ALLOC.lock().dealloc(frame);
    r
}

/// Explicit `FLUSH CACHE EXT`. Claims a tag, drains every *other* tag, and holds
/// `SUBMIT` across the flush (a non-queued command must not overlap queued
/// ones). Rare — the NCQ write path uses FUA instead. Polled, not blocking.
pub fn flush() -> Result<(), &'static str> {
    if HBA_BASE.load(Ordering::Relaxed) == 0 {
        return Err("AHCI not initialised");
    }
    let hba = Hba::cur();
    let tag = alloc_tag();
    let others = !(1u32 << tag);

    let _s = SUBMIT.lock();
    for _ in 0..POLL_LIMIT {
        if hba.pr(P_SACT) & others == 0 && hba.pr(P_CI) & others == 0 {
            break;
        }
        sched::yield_now();
    }
    issue(tag, false, false, None, |f| unsafe {
        f.add(0).write_volatile(FIS_TYPE_H2D);
        f.add(1).write_volatile(1 << 7);
        f.add(2).write_volatile(CMD_FLUSH_CACHE_EX);
        f.add(7).write_volatile(1 << 6);
    });
    let r = wait(tag, false, false);
    drop(_s);
    free_tag(tag);
    r
}

/// `IDENTIFY DEVICE` on tag 0 (non-queued); raw 512-byte block. Init only.
fn identify() -> Result<[u8; 512], &'static str> {
    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let buf_phys = frame.start_address().as_u64();
    {
        wait_ready(&Hba::cur());
        let _s = SUBMIT.lock();
        issue(0, false, false, Some((buf_phys, 512)), |f| unsafe {
            f.add(0).write_volatile(FIS_TYPE_H2D);
            f.add(1).write_volatile(1 << 7);
            f.add(2).write_volatile(CMD_IDENTIFY);
            f.add(7).write_volatile(0); // IDENTIFY does not use LBA mode
        });
    }
    let r = wait(0, false, false);

    let mut block = [0u8; 512];
    if r.is_ok() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(x86_64::PhysAddr::new(buf_phys)).as_ptr::<u8>(),
                block.as_mut_ptr(),
                512,
            );
        }
    }
    FRAME_ALLOC.lock().dealloc(frame);
    r.map(|()| block)
}
