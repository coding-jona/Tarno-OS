// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — AHCI / SATA block driver (polled).
//!
//! The target machine boots THOS from the Kingston A400 SATA SSD behind the
//! Intel Raptor Lake AHCI controller (`8086:7a62`); AHCI 1.3.1 is a standard
//! register interface, so this is portable. Polled for now — MSI-X completion
//! interrupts and NCQ come later.
//!
//! One command frame per port (command list + received FIS + one command
//! table packed into a single 4 KiB physical frame), single command slot,
//! `READ DMA EXT` / `WRITE DMA EXT`.

use core::sync::atomic::{fence, Ordering};

use spin::Mutex;

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::pci;

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
const P_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;

const TFD_BSY: u32 = 1 << 7;
const TFD_DRQ: u32 = 1 << 3;
const IS_TFES: u32 = 1 << 30;

const SIG_SATA: u32 = 0x0000_0101;

const FIS_TYPE_H2D: u8 = 0x27;
const CMD_READ_DMA_EX: u8 = 0x25;
const CMD_WRITE_DMA_EX: u8 = 0x35;
const CMD_FLUSH_CACHE_EX: u8 = 0xEA;

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
    hba: Hba,
    port: usize,
    /// Physical base of the per-port command area (cmd list @ +0, FIS @ +0x400,
    /// command table @ +0x500).
    cmd_phys: u64,
}

unsafe impl Send for Disk {}

static DISK: Mutex<Option<Disk>> = Mutex::new(None);

/// Probe PCI, enable the HBA, bring up the first SATA port. Returns the number
/// of 512-byte sectors is not known without IDENTIFY yet — returns Ok on a
/// working port.
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

    // Stop the port before touching CLB/FB.
    let mut cmd = hba.pr(port, P_CMD);
    cmd &= !(CMD_ST | CMD_FRE);
    hba.pw(port, P_CMD, cmd);
    for _ in 0..1_000_000 {
        if hba.pr(port, P_CMD) & (CMD_CR | CMD_FR) == 0 {
            break;
        }
    }

    // One frame: [0..0x400] cmd list, [0x400..0x500] received FIS,
    // [0x500..] command table 0.
    let frame = FRAME_ALLOC.lock().alloc().ok_or("no frame for AHCI")?;
    let cmd_phys = frame.start_address().as_u64();
    unsafe {
        core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
    }

    hba.pw(port, P_CLB, cmd_phys as u32);
    hba.pw(port, P_CLBU, (cmd_phys >> 32) as u32);
    hba.pw(port, P_FB, (cmd_phys + 0x400) as u32);
    hba.pw(port, P_FBU, ((cmd_phys + 0x400) >> 32) as u32);

    hba.pw(port, P_SERR, 0xFFFF_FFFF); // clear
    hba.pw(port, P_IS, 0xFFFF_FFFF);

    // Start: FRE then ST.
    cmd = hba.pr(port, P_CMD) | CMD_FRE;
    hba.pw(port, P_CMD, cmd);
    cmd |= CMD_ST;
    hba.pw(port, P_CMD, cmd);

    let _ = CAP;
    *DISK.lock() = Some(Disk { hba, port, cmd_phys });
    Ok(())
}

fn transfer(write: bool, lba: u64, sectors: u16, buf_phys: u64) -> Result<(), &'static str> {
    let guard = DISK.lock();
    let d = guard.as_ref().ok_or("AHCI not initialised")?;
    let hba = &d.hba;
    let port = d.port;

    // Wait for the port to be idle.
    for _ in 0..1_000_000 {
        if hba.pr(port, P_TFD) & (TFD_BSY | TFD_DRQ) == 0 {
            break;
        }
    }

    let clist = phys_to_virt(x86_64::PhysAddr::new(d.cmd_phys)).as_mut_ptr::<u32>();
    let ctba = d.cmd_phys + 0x500;
    let ctab = phys_to_virt(x86_64::PhysAddr::new(ctba)).as_mut_ptr::<u8>();

    unsafe {
        // Command header, slot 0.
        let cfl = 5u32; // H2D register FIS = 5 dwords
        let w = if write { 1u32 << 6 } else { 0 };
        core::ptr::write_volatile(clist.add(0), cfl | w | (1u32 << 16)); // PRDTL = 1
        core::ptr::write_volatile(clist.add(1), 0); // PRDBC
        core::ptr::write_volatile(clist.add(2), ctba as u32);
        core::ptr::write_volatile(clist.add(3), (ctba >> 32) as u32);

        core::ptr::write_bytes(ctab, 0, 0x80 + 16);

        // PRDT entry 0 at offset 0x80.
        let prdt = ctab.add(0x80) as *mut u32;
        core::ptr::write_volatile(prdt.add(0), buf_phys as u32);
        core::ptr::write_volatile(prdt.add(1), (buf_phys >> 32) as u32);
        core::ptr::write_volatile(prdt.add(2), 0);
        let bytes = sectors as u32 * SECTOR as u32;
        core::ptr::write_volatile(prdt.add(3), bytes - 1); // byte count - 1

        // Command FIS (H2D register).
        let f = ctab;
        f.add(0).write_volatile(FIS_TYPE_H2D);
        f.add(1).write_volatile(1 << 7); // C = 1
        f.add(2).write_volatile(if write { CMD_WRITE_DMA_EX } else { CMD_READ_DMA_EX });
        f.add(3).write_volatile(0);
        f.add(4).write_volatile(lba as u8);
        f.add(5).write_volatile((lba >> 8) as u8);
        f.add(6).write_volatile((lba >> 16) as u8);
        f.add(7).write_volatile(1 << 6); // LBA mode
        f.add(8).write_volatile((lba >> 24) as u8);
        f.add(9).write_volatile((lba >> 32) as u8);
        f.add(10).write_volatile((lba >> 40) as u8);
        f.add(11).write_volatile(0);
        f.add(12).write_volatile(sectors as u8);
        f.add(13).write_volatile((sectors >> 8) as u8);
    }

    fence(Ordering::SeqCst);
    hba.pw(port, P_IS, 0xFFFF_FFFF);
    hba.pw(port, P_CI, 1); // issue slot 0

    for _ in 0..10_000_000 {
        if hba.pr(port, P_CI) & 1 == 0 {
            if hba.pr(port, P_IS) & IS_TFES != 0 {
                return Err("AHCI task-file error");
            }
            fence(Ordering::SeqCst);
            return Ok(());
        }
    }
    Err("AHCI command timed out")
}

/// Read `buf.len() / 512` sectors starting at `lba` into `buf`.
pub fn read(lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
    assert!(buf.len() % SECTOR == 0 && !buf.is_empty() && buf.len() <= 4096);
    let sectors = (buf.len() / SECTOR) as u16;

    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let phys = frame.start_address();
    transfer(false, lba, sectors, phys.as_u64())?;
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(phys).as_ptr::<u8>(),
            buf.as_mut_ptr(),
            buf.len(),
        );
    }
    FRAME_ALLOC.lock().dealloc(frame);
    Ok(())
}

/// Write `buf.len() / 512` sectors starting at `lba` from `buf`, then flush the
/// drive's write cache so the data is durable before this returns.
pub fn write(lba: u64, buf: &[u8]) -> Result<(), &'static str> {
    assert!(buf.len() % SECTOR == 0 && !buf.is_empty() && buf.len() <= 4096);
    let sectors = (buf.len() / SECTOR) as u16;

    let frame = FRAME_ALLOC.lock().alloc().ok_or("no bounce frame")?;
    let phys = frame.start_address();
    unsafe {
        core::ptr::copy_nonoverlapping(
            buf.as_ptr(),
            phys_to_virt(phys).as_mut_ptr::<u8>(),
            buf.len(),
        );
    }
    let r = transfer(true, lba, sectors, phys.as_u64()).and_then(|()| flush());
    FRAME_ALLOC.lock().dealloc(frame);
    r
}

/// Issue `FLUSH CACHE EXT` (no data transfer) on the active port.
fn flush() -> Result<(), &'static str> {
    let guard = DISK.lock();
    let d = guard.as_ref().ok_or("AHCI not initialised")?;
    let hba = &d.hba;
    let port = d.port;

    for _ in 0..1_000_000 {
        if hba.pr(port, P_TFD) & (TFD_BSY | TFD_DRQ) == 0 {
            break;
        }
    }

    let clist = phys_to_virt(x86_64::PhysAddr::new(d.cmd_phys)).as_mut_ptr::<u32>();
    let ctba = d.cmd_phys + 0x500;
    let ctab = phys_to_virt(x86_64::PhysAddr::new(ctba)).as_mut_ptr::<u8>();

    unsafe {
        core::ptr::write_volatile(clist.add(0), 5u32); // cfl = 5 dwords, PRDTL = 0, no W
        core::ptr::write_volatile(clist.add(1), 0);
        core::ptr::write_volatile(clist.add(2), ctba as u32);
        core::ptr::write_volatile(clist.add(3), (ctba >> 32) as u32);

        core::ptr::write_bytes(ctab, 0, 0x80);
        let f = ctab;
        f.add(0).write_volatile(FIS_TYPE_H2D);
        f.add(1).write_volatile(1 << 7); // C = 1
        f.add(2).write_volatile(CMD_FLUSH_CACHE_EX);
        f.add(7).write_volatile(1 << 6); // LBA mode
    }

    fence(Ordering::SeqCst);
    hba.pw(port, P_IS, 0xFFFF_FFFF);
    hba.pw(port, P_CI, 1);

    for _ in 0..10_000_000 {
        if hba.pr(port, P_CI) & 1 == 0 {
            if hba.pr(port, P_IS) & IS_TFES != 0 {
                return Err("AHCI flush task-file error");
            }
            return Ok(());
        }
    }
    Err("AHCI flush timed out")
}
