// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — xHCI + a USB HID boot-protocol keyboard (polled).
//!
//! Minimal path: reset the controller, stand up the command ring + one event
//! ring, enumerate the first connected port, Enable Slot → Address Device →
//! Set Configuration → Configure the interrupt-IN endpoint → Set Protocol(boot),
//! then keep a Normal TRB queued on the interrupt endpoint and hand the 8-byte
//! boot reports to the console.
//!
//! Descriptor parsing is skipped — QEMU's `usb-kbd` is config 1 / interface 0 /
//! EP `0x81` interrupt-IN, 8-byte reports. A general enumerator comes later.
//!
//! No real interrupts yet — `poll_keyboard` is called from a kernel thread.

use crate::mm::{phys_to_virt, FRAME_ALLOC};
use crate::pci;

// operational registers
const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_CRCR: usize = 0x18;
const OP_DCBAAP: usize = 0x30;
const OP_CONFIG: usize = 0x38;
const OP_PORTSC_BASE: usize = 0x400;

const USBCMD_RUN: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_CNR: u32 = 1 << 11;

const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PED: u32 = 1 << 1;
const PORTSC_PR: u32 = 1 << 4;
const PORTSC_PRC: u32 = 1 << 21;
const PORTSC_RW1CS: u32 = 0x00FE_0000; // the write-1-to-clear status bits

// TRB types
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EV_TRANSFER: u32 = 32;
const TRB_EV_CMD_COMPLETE: u32 = 33;

const RING_TRBS: usize = 256;
const EP0_DCI: u32 = 1;
const KBD_EP_DCI: u32 = 3; // EP1 IN

fn dma() -> (u64, *mut u8) {
    let frame = FRAME_ALLOC.lock().alloc().expect("xhci: no DMA frame");
    let virt = phys_to_virt(frame.start_address()).as_mut_ptr::<u8>();
    unsafe { core::ptr::write_bytes(virt, 0, 4096) };
    (frame.start_address().as_u64(), virt)
}

fn r32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
fn w32(addr: u64, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}
fn w64(addr: u64, v: u64) {
    w32(addr, v as u32);
    w32(addr + 4, (v >> 32) as u32);
}

/// A producer ring with a trailing Link TRB.
struct Ring {
    virt: *mut [u32; 4],
    phys: u64,
    idx: usize,
    cycle: u32,
}
impl Ring {
    fn new() -> Self {
        let (phys, virt) = dma();
        let virt = virt as *mut [u32; 4];
        unsafe {
            *virt.add(RING_TRBS - 1) =
                [phys as u32, (phys >> 32) as u32, 0, (TRB_LINK << 10) | (1 << 1) | 1];
        }
        Self { virt, phys, idx: 0, cycle: 1 }
    }

    fn push(&mut self, mut trb: [u32; 4]) {
        trb[3] = (trb[3] & !1) | self.cycle;
        unsafe { core::ptr::write_volatile(self.virt.add(self.idx), trb) };
        self.idx += 1;
        if self.idx == RING_TRBS - 1 {
            unsafe {
                let link = self.virt.add(RING_TRBS - 1);
                let mut l = core::ptr::read_volatile(link);
                l[3] = (l[3] & !1) | self.cycle;
                core::ptr::write_volatile(link, l);
            }
            self.idx = 0;
            self.cycle ^= 1;
        }
    }
}

/// The event ring consumer.
struct EventRing {
    virt: *mut [u32; 4],
    phys: u64,
    idx: usize,
    cycle: u32,
    erdp_reg: u64,
}
impl EventRing {
    fn poll(&mut self) -> Option<[u32; 4]> {
        let trb = unsafe { core::ptr::read_volatile(self.virt.add(self.idx)) };
        if (trb[3] & 1) != self.cycle {
            return None;
        }
        self.idx += 1;
        if self.idx == RING_TRBS {
            self.idx = 0;
            self.cycle ^= 1;
        }
        let erdp = self.phys + (self.idx * 16) as u64;
        w32(self.erdp_reg, (erdp as u32) | (1 << 3));
        w32(self.erdp_reg + 4, (erdp >> 32) as u32);
        Some(trb)
    }
}

#[allow(dead_code)] // op kept for later (port status, suspend)
pub struct Xhci {
    op: u64,
    db: u64,
    slot: u8,
    cmd: Ring,
    ep0: Ring,
    kbd: Ring,
    events: EventRing,
    kbd_buf_phys: u64,
    kbd_buf: *mut u8,
}
unsafe impl Send for Xhci {}

impl Xhci {
    fn ring_db(&self, slot: u64, target: u32) {
        w32(self.db + slot * 4, target);
    }

    fn wait_event(&mut self, want: u32) -> Option<[u32; 4]> {
        for _ in 0..80_000_000 {
            if let Some(t) = self.events.poll() {
                if (t[3] >> 10) & 0x3F == want {
                    return Some(t);
                }
            }
        }
        None
    }

    fn command(&mut self, trb: [u32; 4]) -> Option<[u32; 4]> {
        self.cmd.push(trb);
        self.ring_db(0, 0);
        self.wait_event(TRB_EV_CMD_COMPLETE)
    }

    /// EP0 control transfer (optional IN data phase), then wait for completion.
    fn control(&mut self, setup: [u8; 8], data_phys: u64, data_len: u16) -> Option<[u32; 4]> {
        let s = u64::from_le_bytes(setup);
        let has_data = data_len > 0;
        let trt = if has_data { 3u32 } else { 0 };
        self.ep0.push([s as u32, (s >> 32) as u32, 8, (TRB_SETUP << 10) | (1 << 6) | (trt << 16)]);
        if has_data {
            self.ep0.push([
                data_phys as u32,
                (data_phys >> 32) as u32,
                data_len as u32,
                (TRB_DATA << 10) | (1 << 16),
            ]);
        }
        let dir = if has_data { 0u32 } else { 1 };
        self.ep0.push([0, 0, 0, (TRB_STATUS << 10) | (dir << 16) | (1 << 5)]);
        self.ring_db(self.slot as u64, EP0_DCI);
        self.wait_event(TRB_EV_TRANSFER)
    }

    fn arm_kbd(&mut self) {
        self.kbd.push([
            self.kbd_buf_phys as u32,
            (self.kbd_buf_phys >> 32) as u32,
            8,
            (TRB_NORMAL << 10) | (1 << 5),
        ]);
        self.ring_db(self.slot as u64, KBD_EP_DCI);
    }

    /// Return an 8-byte HID boot report if the keyboard endpoint produced one.
    pub fn poll_keyboard(&mut self) -> Option<[u8; 8]> {
        let ev = self.events.poll()?;
        if (ev[3] >> 10) & 0x3F != TRB_EV_TRANSFER {
            return None;
        }
        let mut rpt = [0u8; 8];
        unsafe { core::ptr::copy_nonoverlapping(self.kbd_buf, rpt.as_mut_ptr(), 8) };
        self.arm_kbd();
        Some(rpt)
    }
}

fn setup(bm: u8, breq: u8, wval: u16, widx: u16, wlen: u16) -> [u8; 8] {
    let mut s = [0u8; 8];
    s[0] = bm;
    s[1] = breq;
    s[2..4].copy_from_slice(&wval.to_le_bytes());
    s[4..6].copy_from_slice(&widx.to_le_bytes());
    s[6..8].copy_from_slice(&wlen.to_le_bytes());
    s
}

pub fn init() -> Result<Xhci, &'static str> {
    let loc = pci::find_xhci().ok_or("no xHCI controller")?;
    pci::enable_bus_master(loc);
    let bar = pci::bar(loc, 0);
    if bar == 0 {
        return Err("xHCI BAR0 is zero");
    }
    let base = crate::vmm::map_mmio(bar, 0x2_0000);

    let caplen = (r32(base) & 0xFF) as u64;
    let hcs1 = r32(base + 0x04);
    let max_slots = hcs1 & 0xFF;
    let max_ports = ((hcs1 >> 24) & 0xFF) as usize;
    let dboff = (r32(base + 0x14) & !0x3) as u64;
    let rtsoff = (r32(base + 0x18) & !0x1F) as u64;

    let op = base + caplen;
    let db = base + dboff;
    let ir0 = base + rtsoff + 0x20;

    // reset
    while r32(op + OP_USBSTS as u64) & USBSTS_CNR != 0 {}
    w32(op + OP_USBCMD as u64, r32(op + OP_USBCMD as u64) & !USBCMD_RUN);
    while r32(op + OP_USBSTS as u64) & USBSTS_HCH == 0 {}
    w32(op + OP_USBCMD as u64, USBCMD_HCRST);
    while r32(op + OP_USBCMD as u64) & USBCMD_HCRST != 0 {}
    while r32(op + OP_USBSTS as u64) & USBSTS_CNR != 0 {}

    w32(op + OP_CONFIG as u64, max_slots);

    let (dcbaa_phys, dcbaa) = dma();
    w64(op + OP_DCBAAP as u64, dcbaa_phys);

    let cmd = Ring::new();
    w64(op + OP_CRCR as u64, cmd.phys | 1);

    let (evt_phys, evt_virt) = dma();
    let (erst_phys, erst_virt) = dma();
    unsafe {
        (erst_virt as *mut u64).write(evt_phys);
        (erst_virt.add(8) as *mut u32).write(RING_TRBS as u32);
    }
    w32(ir0 + 0x08, 1); // ERSTSZ = 1 segment
    w64(ir0 + 0x10, erst_phys); // ERSTBA
    w64(ir0 + 0x18, evt_phys); // ERDP

    let events = EventRing {
        virt: evt_virt as *mut [u32; 4],
        phys: evt_phys,
        idx: 0,
        cycle: 1,
        erdp_reg: ir0 + 0x18,
    };

    w32(op + OP_USBCMD as u64, USBCMD_RUN);
    while r32(op + OP_USBSTS as u64) & USBSTS_HCH != 0 {}

    // first connected port
    let mut port = 0usize;
    for p in 1..=max_ports {
        if r32(op + (OP_PORTSC_BASE + (p - 1) * 0x10) as u64) & PORTSC_CCS != 0 {
            port = p;
            break;
        }
    }
    if port == 0 {
        return Err("no device on any xHCI port");
    }
    let psc = op + (OP_PORTSC_BASE + (port - 1) * 0x10) as u64;
    if r32(psc) & PORTSC_PED == 0 {
        w32(psc, (r32(psc) & !PORTSC_RW1CS) | PORTSC_PR);
        for _ in 0..20_000_000 {
            if r32(psc) & PORTSC_PRC != 0 {
                break;
            }
        }
        w32(psc, (r32(psc) & !PORTSC_RW1CS) | PORTSC_PRC);
    }
    let speed = (r32(psc) >> 10) & 0xF;
    let mps0: u32 = match speed {
        4 => 64,
        5 => 512,
        _ => 8,
    };

    let (kb_phys, kb_virt) = dma();
    let mut x = Xhci {
        op,
        db,
        slot: 0,
        cmd,
        ep0: Ring::new(),
        kbd: Ring::new(),
        events,
        kbd_buf_phys: kb_phys,
        kbd_buf: kb_virt,
    };

    let ev = x.command([0, 0, 0, TRB_ENABLE_SLOT << 10]).ok_or("Enable Slot timeout")?;
    x.slot = ((ev[3] >> 24) & 0xFF) as u8;
    if x.slot == 0 || (ev[2] >> 24) & 0xFF != 1 {
        return Err("Enable Slot failed");
    }

    // Input context: add slot + EP0; Address Device.
    let (input_phys, input_virt) = dma();
    let (devctx_phys, _dev) = dma();
    unsafe {
        (dcbaa as *mut u64).add(x.slot as usize).write(devctx_phys);
        let ic = input_virt as *mut u32;
        ic.add(1).write(0b11); // add slot + EP0
        ic.add(0x20 / 4).write((1 << 27) | (speed << 20)); // slot ctx: entries=1, speed
        ic.add(0x20 / 4 + 1).write((port as u32) << 16); // root hub port
        ic.add(0x40 / 4 + 1).write((4 << 3) | (mps0 << 16) | (3 << 1)); // EP0: control, MPS, CErr
        ic.add(0x40 / 4 + 2).write((x.ep0.phys as u32) | 1);
        ic.add(0x40 / 4 + 3).write((x.ep0.phys >> 32) as u32);
    }
    x.command([
        input_phys as u32,
        (input_phys >> 32) as u32,
        0,
        (TRB_ADDRESS_DEVICE << 10) | ((x.slot as u32) << 24),
    ])
    .ok_or("Address Device timeout")?;

    // SET_CONFIGURATION(1)
    x.control(setup(0x00, 0x09, 1, 0, 0), 0, 0).ok_or("Set Config failed")?;

    // Configure Endpoint: add EP1 IN (interrupt).
    unsafe {
        let ic = input_virt as *mut u32;
        core::ptr::write_bytes(ic, 0, 0x400 / 4 * 4 / 4); // clear whole input ctx region-ish
        core::ptr::write_bytes(input_virt, 0, 4096);
        ic.add(1).write(1 | (1 << KBD_EP_DCI)); // add slot ctx (required) + EP DCI 3
        ic.add(0x20 / 4).write(((KBD_EP_DCI + 0) << 27) | (speed << 20)); // ctx entries >= 3
        ic.add(0x20 / 4 + 1).write((port as u32) << 16);
        // EP context for DCI 3 at 0x20 + DCI*0x20 = 0x80
        let ep = 0x80 / 4;
        ic.add(ep + 1).write((7 << 3) | (8 << 16) | (3 << 1)); // interrupt IN, MPS 8, CErr 3
        ic.add(ep + 2).write((x.kbd.phys as u32) | 1);
        ic.add(ep + 3).write((x.kbd.phys >> 32) as u32);
        ic.add(ep + 4).write(8); // average TRB length / max ESIT-ish
    }
    x.command([
        input_phys as u32,
        (input_phys >> 32) as u32,
        0,
        (TRB_CONFIGURE_ENDPOINT << 10) | ((x.slot as u32) << 24),
    ])
    .ok_or("Configure Endpoint timeout")?;

    // HID SET_PROTOCOL(boot=0) on interface 0 — best effort.
    let _ = x.control(setup(0x21, 0x0B, 0, 0, 0), 0, 0);
    // SET_IDLE(0)
    let _ = x.control(setup(0x21, 0x0A, 0, 0, 0), 0, 0);

    x.arm_kbd();
    Ok(x)
}
