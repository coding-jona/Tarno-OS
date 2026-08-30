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
//! `queue depth` transfers run at once and the drive reorders them. Completion
//! is polled from `PxSACT` (updated by the drive's Set-Device-Bits FIS), and a
//! waiting thread `yield`s rather than spinning a core. Without NCQ it falls
//! back to a single-tag `READ`/`WRITE DMA EXT` + `FLUSH CACHE EXT`. Still
//! polled — MSI-X completion interrupts come later.

use core::sync::atomic::{fence, AtomicU32, Ordering};

use spin::Mutex;

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::{pci, sched};

pub const SECTOR: usize = 512;

// HBA_MEM
const GHC: usize = 0x04;
const CAP: usize = 0x00;
const PI: usize = 0x0C;
const GHC_AE: u32 = 1 << 31;
const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;

// HBA_PORT (relative to port base)
const P_CLB: usize = 0x00;
const P_CLBU: usize = 0x04;
const P_FB: usize = 0x08;
const P_FBU: usize = 0x0C;
const P_IS: usize = 0x10;
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
/// Cap on the poll-yield loop before a transfer is declared timed out.
const POLL_LIMIT: u32 = 2_000_000;

struct Hba {
    base: u64, // HHDM virtual address of the ABAR
}

impl Hba {
    fn r(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base as usize + off) as *const u32) }
    }
    fn w(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.base as usize + off) as *mut u32, v) }
    }
    fn pr(&self, port: usize, off: usize) -> u32 {
        self.r(PORT_BASE + port * PORT_STRIDE + off)
    }
    fn pw(&self, port: usize, off: usize, v: u32) {
        self.w(PORT_BASE + port * PORT_STRIDE + off, v);
    }
}

pub struct Disk {
    base: u64,
    port: usize,
    /// Frame holding the command list (`+0`) and received-FIS area (`+0x400`).
    clb_phys: u64,
    /// Two frames, 16 command tables each (`0x100` stride).
    ctab_phys: [u64; 2],
    /// Addressable 512-byte sectors, from `IDENTIFY DEVICE`.
    sectors: u64,
    /// Usable command queue depth: `1` = no NCQ (legacy DMA), else `2..=32`.
    depth: u8,
}

unsafe impl Send for Disk {}

impl Disk {
    fn ctab(&self, tag: u8) -> u64 {
        self.ctab_phys[(tag >> 4) as usize] + ((tag & 0xF) as u64) * 0x100
    }
}

static DISK: Mutex<Option<Disk>> = Mutex::new(None);
/// Held only while a command is being *built and issued* (microseconds); a
/// transfer's poll-wait happens with this released so other tags can be queued.
static SUBMIT: Mutex<()> = Mutex::new(());
/// One bit per free tag. Populated in `init` from the drive's queue depth.
static FREE_TAGS: AtomicU32 = AtomicU32::new(0);

fn alloc_tag() -> u8 {
    loop {
        let cur = FREE_TAGS.load(Ordering::Acquire);
        if cur != 0 {
            let t = cur.trailing_zeros();
            let bit = 1u32 << t;
            if FREE_TAGS
                .compare_exchange_weak(cur, cur & !bit, Ordering::AcqRel, Ordering::Acquire)
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

/// Probe PCI, enable the HBA, bring up the first SATA port, `IDENTIFY` it, and
/// set up the command list + tables. `Ok` on a working port.
pub fn init() -> Result<(), &'static str> {
    let loc = pci::find_ahci().ok_or("no AHCI controller on bus 0")?;
    pci::enable_bus_master(loc);
    let abar = pci::bar(loc, 5);
    if abar == 0 {
        return Err("AHCI BAR5 is zero");
    }
    let hba = Hba { base: crate::vmm::map_mmio(abar, 0x2000) };
    hba.w(GHC, hba.r(GHC) | GHC_AE);

    let pi = hba.r(PI);
    let port = (0..32)
        .find(|&p| {
            pi & (1 << p) != 0
                && (hba.pr(p, P_SSTS) & 0x0F) == 3
                && hba.pr(p, P_SIG) == SIG_SATA
        })
        .ok_or("no SATA device on any implemented port")?;

    // Stop the port before repointing CLB/FB.
    let mut cmd = hba.pr(port, P_CMD) & !(CMD_ST | CMD_FRE);
    hba.pw(port, P_CMD, cmd);
    for _ in 0..1_000_000 {
        if hba.pr(port, P_CMD) & (CMD_CR | CMD_FR) == 0 {
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
    let clb_phys = alloc_zeroed().ok_or("no frame for AHCI cmd list")?;
    let ctab_phys = [
        alloc_zeroed().ok_or("no frame for AHCI cmd tables")?,
        alloc_zeroed().ok_or("no frame for AHCI cmd tables")?,
    ];

    hba.pw(port, P_CLB, clb_phys as u32);
    hba.pw(port, P_CLBU, (clb_phys >> 32) as u32);
    hba.pw(port, P_FB, (clb_phys + 0x400) as u32);
    hba.pw(port, P_FBU, ((clb_phys + 0x400) >> 32) as u32);
    hba.pw(port, P_SERR, 0xFFFF_FFFF);
    hba.pw(port, P_IS, 0xFFFF_FFFF);

    cmd = hba.pr(port, P_CMD) | CMD_FRE;
    hba.pw(port, P_CMD, cmd);
    hba.pw(port, P_CMD, cmd | CMD_ST);

    let _ = CAP;
    *DISK.lock() = Some(Disk { base: hba.base, port, clb_phys, ctab_phys, sectors: 0, depth: 1 });

    // IDENTIFY DEVICE — capacity, model, NCQ support (word 76 bit 8) and queue
    // depth (word 75 bits 0..4, zero-based). Issued on tag 0, non-queued, while
    // nothing else can be running.
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

    {
        let mut g = DISK.lock();
        let d = g.as_mut().unwrap();
        d.sectors = sectors;
        d.depth = depth;
    }
    FREE_TAGS.store(((1u64 << depth) - 1) as u32, Ordering::Release);

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
    if depth > 1 {
        crate::kprintln!("THOS: ahci ncq         queue depth {}", depth);
    }
    Ok(())
}

/// Total addressable 512-byte sectors on the active disk.
pub fn capacity_sectors() -> u64 {
    DISK.lock().as_ref().map(|d| d.sectors).unwrap_or(0)
}

/// Usable command queue depth (1 = no NCQ).
pub fn queue_depth() -> u8 {
    DISK.lock().as_ref().map(|d| d.depth).unwrap_or(1)
}

/// Poll (yielding) until the device is ready to accept a new command.
fn wait_ready(hba: &Hba, port: usize) {
    for _ in 0..1_000_000 {
        if hba.pr(port, P_TFD) & (TFD_BSY | TFD_DRQ) == 0 {
            return;
        }
        sched::yield_now();
    }
}

/// Build tag `tag`'s command header + table (FIS filled by `fill_fis`, PRDT set
/// from `buf`) and issue it — via `PxSACT`+`PxCI` when `ncq`, else `PxCI`.
/// The caller must hold `SUBMIT`.
fn issue(tag: u8, ncq: bool, is_write: bool, buf: Option<(u64, u32)>, fill_fis: impl FnOnce(*mut u8)) {
    let (base, port, hdr_phys, ctab_phys) = {
        let g = DISK.lock();
        let d = g.as_ref().unwrap();
        (d.base, d.port, d.clb_phys + tag as u64 * 32, d.ctab(tag))
    };
    let hba = Hba { base };
    let hdr = phys_to_virt(x86_64::PhysAddr::new(hdr_phys)).as_mut_ptr::<u32>();
    let ctab = phys_to_virt(x86_64::PhysAddr::new(ctab_phys)).as_mut_ptr::<u8>();

    unsafe {
        let prdtl = u32::from(buf.is_some());
        let w = if is_write { 1u32 << 6 } else { 0 };
        core::ptr::write_volatile(hdr.add(0), 5 | w | (prdtl << 16)); // cfl=5 dwords
        core::ptr::write_volatile(hdr.add(1), 0); // PRDBC
        core::ptr::write_volatile(hdr.add(2), ctab_phys as u32);
        core::ptr::write_volatile(hdr.add(3), (ctab_phys >> 32) as u32);

        core::ptr::write_bytes(ctab, 0, 0x80 + 16);
        if let Some((phys, bytes)) = buf {
            let prdt = ctab.add(0x80) as *mut u32;
            core::ptr::write_volatile(prdt.add(0), phys as u32);
            core::ptr::write_volatile(prdt.add(1), (phys >> 32) as u32);
            core::ptr::write_volatile(prdt.add(2), 0);
            core::ptr::write_volatile(prdt.add(3), bytes - 1);
        }
        fill_fis(ctab);
    }

    fence(Ordering::SeqCst);
    if ncq {
        hba.pw(port, P_SACT, 1u32 << tag);
    }
    hba.pw(port, P_CI, 1u32 << tag);
}

/// Poll (yielding) until tag `tag` completes; NCQ tags clear from `PxSACT`,
/// legacy slots from `PxCI`.
fn wait(tag: u8, ncq: bool) -> Result<(), &'static str> {
    let (base, port) = {
        let g = DISK.lock();
        let d = g.as_ref().unwrap();
        (d.base, d.port)
    };
    let hba = Hba { base };
    let mask = 1u32 << tag;
    for _ in 0..POLL_LIMIT {
        let is = hba.pr(port, P_IS);
        if is & IS_TFES != 0 || hba.pr(port, P_TFD) & TFD_ERR != 0 {
            hba.pw(port, P_IS, is);
            return Err("AHCI task-file error");
        }
        let done = if ncq {
            hba.pr(port, P_SACT) & mask == 0
        } else {
            hba.pr(port, P_CI) & mask == 0
        };
        if done {
            fence(Ordering::SeqCst);
            return Ok(());
        }
        sched::yield_now();
    }
    Err("AHCI command timed out")
}

/// One data transfer of `sectors` sectors at `lba` to/from `buf_phys`.
fn transfer(is_write: bool, lba: u64, sectors: u16, buf_phys: u64) -> Result<(), &'static str> {
    let (cap, depth) = {
        let g = DISK.lock();
        let d = g.as_ref().ok_or("AHCI not initialised")?;
        (d.sectors, d.depth)
    };
    if cap != 0 && lba + sectors as u64 > cap {
        return Err("AHCI: LBA past end of disk");
    }
    let bytes = sectors as u32 * SECTOR as u32;
    let ncq = depth > 1;
    let tag = alloc_tag();

    {
        let (base, port) = {
            let g = DISK.lock();
            let d = g.as_ref().unwrap();
            (d.base, d.port)
        };
        wait_ready(&Hba { base }, port);
        let _s = SUBMIT.lock();
        issue(tag, ncq, is_write, Some((buf_phys, bytes)), fis(is_write, ncq, lba, sectors, tag));
    }

    let r = wait(tag, ncq);
    free_tag(tag);
    r
}

/// The FIS builder for a `READ`/`WRITE` (`FPDMA QUEUED` when `ncq`, else
/// `DMA EXT`). Returned as a closure so `transfer` stays flat.
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
            // FIS LBA bytes: [4,5,6] then [8,9,10]
            let off = if i < 3 { 4 + i } else { 5 + i };
            f.add(off).write_volatile(b);
        }
        if ncq {
            f.add(2).write_volatile(if is_write { CMD_WRITE_FPDMA } else { CMD_READ_FPDMA });
            f.add(3).write_volatile(sectors as u8); // features 7:0 = count low
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

/// Write `buf.len() / 512` sectors starting at `lba` from `buf`, durably: the
/// NCQ path sets FUA, the legacy path issues `FLUSH CACHE EXT` afterwards.
pub fn write(lba: u64, buf: &[u8]) -> Result<(), &'static str> {
    assert!(buf.len() % SECTOR == 0 && !buf.is_empty() && buf.len() <= 4096);
    let sectors = (buf.len() / SECTOR) as u16;

    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let phys = frame.start_address();
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), phys_to_virt(phys).as_mut_ptr::<u8>(), buf.len());
    }
    let mut r = transfer(true, lba, sectors, phys.as_u64());
    if r.is_ok() && queue_depth() == 1 {
        r = flush();
    }
    FRAME_ALLOC.lock().dealloc(frame);
    r
}

/// Explicit `FLUSH CACHE EXT` barrier. A non-queued command must not overlap
/// queued ones, so this claims a tag, waits for every *other* tag to drain, and
/// holds `SUBMIT` across the whole flush so nothing new is queued meanwhile.
pub fn flush() -> Result<(), &'static str> {
    let (base, port) = {
        let g = DISK.lock();
        let d = g.as_ref().ok_or("AHCI not initialised")?;
        (d.base, d.port)
    };
    let hba = Hba { base };
    let tag = alloc_tag();
    let others = !(1u32 << tag);

    let _s = SUBMIT.lock();
    for _ in 0..POLL_LIMIT {
        if hba.pr(port, P_SACT) & others == 0 && hba.pr(port, P_CI) & others == 0 {
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
    let r = wait(tag, false);
    drop(_s);
    free_tag(tag);
    r
}

/// `IDENTIFY DEVICE` on tag 0 (non-queued); returns the raw 512-byte block.
/// Only called from `init`, before any other command can run or `FREE_TAGS` is
/// populated.
fn identify() -> Result<[u8; 512], &'static str> {
    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let buf_phys = frame.start_address().as_u64();

    {
        let (base, port) = {
            let g = DISK.lock();
            let d = g.as_ref().unwrap();
            (d.base, d.port)
        };
        wait_ready(&Hba { base }, port);
        let _s = SUBMIT.lock();
        issue(0, false, false, Some((buf_phys, 512)), |f| unsafe {
            f.add(0).write_volatile(FIS_TYPE_H2D);
            f.add(1).write_volatile(1 << 7);
            f.add(2).write_volatile(CMD_IDENTIFY);
            f.add(7).write_volatile(0); // IDENTIFY does not use LBA mode
        });
    }
    let r = wait(0, false);

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
