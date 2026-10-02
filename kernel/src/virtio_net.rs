// SPDX-License-Identifier: GPL-2.0-or-later
//! virtio-net (legacy / transitional PCI device, I/O-port BAR0) — the NIC QEMU offers, so
//! the network stack is testable in CI. Real NICs (the Acer's) get their own drivers behind
//! the same `smoltcp` `Device` interface later (plan: `docs/thos/network-plan.md`).
//!
//! Deliberately small: two queues, 16 buffers each (2 KiB: 10-byte virtio header + a full
//! Ethernet frame), no offloads, no interrupts yet (the net thread polls) — a few tens of
//! KiB in total, which matters on the 2010 reference machine.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

use x86_64::instructions::port::Port;

use crate::pci;

const VENDOR_VIRTIO: u16 = 0x1AF4;
const DEVICE_NET_LEGACY: u16 = 0x1000;

// Legacy register offsets (I/O space).
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SEL: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_CONFIG: u16 = 0x14; // MAC address (no MSI-X)

const ST_ACK: u8 = 1;
const ST_DRIVER: u8 = 2;
const ST_DRIVER_OK: u8 = 4;
const F_MAC: u32 = 1 << 5;

const HDR: usize = 10; // struct virtio_net_hdr without MRG_RXBUF
const BUFS: usize = 16;
const BUF_SZ: usize = 2048;
pub const MTU: usize = 1514; // Ethernet frame without FCS

const DESC_WRITE: u16 = 2;

struct Queue {
    size: u16,
    virt: *mut u8, // start of the ring memory
    used_off: usize,
    avail_idx: u16,
    last_used: u16,
}

unsafe impl Send for Queue {}

impl Queue {
    fn ring_bytes(size: u16) -> usize {
        let q = size as usize;
        let first = (16 * q + 6 + 2 * q + 4095) & !4095;
        (first + 6 + 8 * q + 4095) & !4095
    }
    fn desc(&self, i: usize) -> *mut u8 {
        unsafe { self.virt.add(16 * i) }
    }
    fn avail(&self) -> *mut u16 {
        unsafe { self.virt.add(16 * self.size as usize) as *mut u16 }
    }
    fn used(&self) -> *mut u8 {
        unsafe { self.virt.add(self.used_off) }
    }
    fn set_desc(&self, i: usize, phys: u64, len: u32, flags: u16) {
        unsafe {
            let d = self.desc(i);
            (d as *mut u64).write_volatile(phys);
            (d.add(8) as *mut u32).write_volatile(len);
            (d.add(12) as *mut u16).write_volatile(flags);
            (d.add(14) as *mut u16).write_volatile(0);
        }
    }
    /// Make descriptor `i` available to the device.
    fn push_avail(&mut self, i: u16) {
        unsafe {
            let slot = self.avail_idx as usize % self.size as usize;
            self.avail().add(2 + slot).write_volatile(i);
            fence(Ordering::SeqCst);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            self.avail().add(1).write_volatile(self.avail_idx);
        }
        fence(Ordering::SeqCst);
    }
    /// Next completed `(descriptor id, bytes written)`.
    fn pop_used(&mut self) -> Option<(u16, u32)> {
        unsafe {
            let idx = (self.used().add(2) as *const u16).read_volatile();
            if idx == self.last_used {
                return None;
            }
            fence(Ordering::SeqCst);
            let slot = self.last_used as usize % self.size as usize;
            let e = self.used().add(4 + 8 * slot);
            let id = (e as *const u32).read_volatile() as u16;
            let len = (e.add(4) as *const u32).read_volatile();
            self.last_used = self.last_used.wrapping_add(1);
            Some((id, len))
        }
    }
}

pub struct VirtioNet {
    io: u16,
    pub mac: [u8; 6],
    rx: Queue,
    tx: Queue,
    rx_phys: u64,
    rx_virt: *mut u8,
    tx_phys: u64,
    tx_virt: *mut u8,
    tx_busy: [bool; BUFS],
    /// Received frames not yet handed to the stack.
    pub rx_queue: VecDeque<Vec<u8>>,
}

unsafe impl Send for VirtioNet {}

fn out8(p: u16, v: u8) {
    unsafe { Port::<u8>::new(p).write(v) }
}
fn out16(p: u16, v: u16) {
    unsafe { Port::<u16>::new(p).write(v) }
}
fn out32(p: u16, v: u32) {
    unsafe { Port::<u32>::new(p).write(v) }
}
fn in16(p: u16) -> u16 {
    unsafe { Port::<u16>::new(p).read() }
}
fn in8(p: u16) -> u8 {
    unsafe { Port::<u8>::new(p).read() }
}

impl VirtioNet {
    pub fn probe() -> Result<Self, &'static str> {
        let loc = pci::find_id(VENDOR_VIRTIO, DEVICE_NET_LEGACY).ok_or("no virtio-net device")?;
        let bar0 = pci::read32(loc, 0x10);
        if bar0 & 1 == 0 {
            return Err("virtio-net BAR0 is not an I/O BAR");
        }
        let io = (bar0 & !3) as u16;
        pci::enable_bus_master(loc);

        out8(io + REG_STATUS, 0); // reset
        out8(io + REG_STATUS, ST_ACK);
        out8(io + REG_STATUS, ST_ACK | ST_DRIVER);
        let host = unsafe { Port::<u32>::new(io).read() };
        if host & F_MAC == 0 {
            return Err("virtio-net offers no MAC address");
        }
        out32(io + REG_GUEST_FEATURES, F_MAC); // nothing else: no offloads, 10-byte header

        let (dma_phys, dma_len) = crate::mm::net_dma();
        let dma_virt = crate::mm::phys_to_virt(x86_64::PhysAddr::new(dma_phys)).as_mut_ptr::<u8>();
        unsafe { core::ptr::write_bytes(dma_virt, 0, dma_len as usize) };

        let mut off = 0usize;
        let mut queues: [Option<Queue>; 2] = [None, None];
        for qi in 0..2u16 {
            out16(io + REG_QUEUE_SEL, qi);
            let size = in16(io + REG_QUEUE_SIZE);
            if size == 0 || !size.is_power_of_two() {
                return Err("virtio-net queue missing");
            }
            let bytes = Queue::ring_bytes(size);
            if off + bytes + 2 * BUFS * BUF_SZ > dma_len as usize {
                return Err("virtio-net rings do not fit the DMA region");
            }
            let q = Queue {
                size,
                virt: unsafe { dma_virt.add(off) },
                used_off: (16 * size as usize + 6 + 2 * size as usize + 4095) & !4095,
                avail_idx: 0,
                last_used: 0,
            };
            out32(io + REG_QUEUE_PFN, ((dma_phys + off as u64) >> 12) as u32);
            off += bytes;
            queues[qi as usize] = Some(q);
        }
        let [rx, tx] = queues;
        let (rx, tx) = (rx.unwrap(), tx.unwrap());
        let rx_phys = dma_phys + off as u64;
        let tx_phys = rx_phys + (BUFS * BUF_SZ) as u64;
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = in8(io + REG_CONFIG + i as u16);
        }
        let mut dev = Self {
            io,
            mac,
            rx,
            tx,
            rx_phys,
            rx_virt: unsafe { dma_virt.add(off) },
            tx_phys,
            tx_virt: unsafe { dma_virt.add(off + BUFS * BUF_SZ) },
            tx_busy: [false; BUFS],
            rx_queue: VecDeque::new(),
        };
        for i in 0..BUFS {
            dev.rx.set_desc(i, rx_phys + (i * BUF_SZ) as u64, BUF_SZ as u32, DESC_WRITE);
            dev.rx.push_avail(i as u16);
        }
        out8(io + REG_STATUS, ST_ACK | ST_DRIVER | ST_DRIVER_OK);
        out16(io + REG_QUEUE_NOTIFY, 0);
        Ok(dev)
    }

    /// Move completed receive buffers into `rx_queue` and give them back to the device.
    pub fn poll_rx(&mut self) {
        let mut any = false;
        while let Some((id, len)) = self.rx.pop_used() {
            let len = len as usize;
            if len > HDR && len <= BUF_SZ && (id as usize) < BUFS {
                let src = unsafe { self.rx_virt.add(id as usize * BUF_SZ + HDR) };
                let frame = unsafe { core::slice::from_raw_parts(src, len - HDR) }.to_vec();
                self.rx_queue.push_back(frame);
            }
            self.rx.push_avail(id);
            any = true;
        }
        if any {
            out16(self.io + REG_QUEUE_NOTIFY, 0);
        }
    }

    fn reclaim_tx(&mut self) {
        while let Some((id, _)) = self.tx.pop_used() {
            if (id as usize) < BUFS {
                self.tx_busy[id as usize] = false;
            }
        }
    }

    /// Is there a free transmit buffer?
    pub fn tx_ready(&mut self) -> bool {
        self.reclaim_tx();
        self.tx_busy.iter().any(|b| !b)
    }

    /// Send one Ethernet frame built by `fill` (`len` bytes). `false` if no buffer is free.
    pub fn transmit<F: FnOnce(&mut [u8])>(&mut self, len: usize, fill: F) -> bool {
        self.reclaim_tx();
        if len > MTU {
            return false;
        }
        let Some(i) = self.tx_busy.iter().position(|b| !b) else { return false };
        let buf = unsafe { core::slice::from_raw_parts_mut(self.tx_virt.add(i * BUF_SZ), HDR + len) };
        buf[..HDR].fill(0);
        fill(&mut buf[HDR..]);
        self.tx.set_desc(i, self.tx_phys + (i * BUF_SZ) as u64, (HDR + len) as u32, 0);
        self.tx_busy[i] = true;
        self.tx.push_avail(i as u16);
        out16(self.io + REG_QUEUE_NOTIFY, 1);
        true
    }
}
