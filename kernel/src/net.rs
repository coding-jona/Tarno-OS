// SPDX-License-Identifier: GPL-2.0-or-later
//! The kernel network service (stage N0 of `docs/thos/network-plan.md`): a NIC driver
//! behind `smoltcp`'s `Device` trait, an interface with a static address, and a kernel
//! thread that polls it. The first user is a one-shot ICMP ping of the gateway at boot.
//! Sockets for the two personalities come on top of this (N1+).

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::socket::icmp;
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, Icmpv4Packet, Icmpv4Repr, IpAddress, IpCidr, Ipv4Address,
};
use spin::Mutex;

use crate::kprintln;
use crate::virtio_net::{VirtioNet, MTU};

/// QEMU user-mode networking (`-nic user`): guest 10.0.2.15/24, gateway 10.0.2.2.
const STATIC_IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];

/// The stack's `Device` — the driver plus the smoltcp token plumbing.
struct Nic(VirtioNet);

struct RxTok(Vec<u8>);
struct TxTok<'a>(&'a mut VirtioNet);

impl phy::RxToken for RxTok {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl<'a> phy::TxToken for TxTok<'a> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut out = None;
        let mut f = Some(f);
        // The driver copies straight into its DMA buffer; if no buffer is free the frame is
        // dropped (smoltcp retransmits what matters).
        self.0.transmit(len, |buf| out = Some((f.take().unwrap())(buf)));
        match out {
            Some(r) => r,
            None => (f.take().unwrap())(&mut vec![0u8; len]),
        }
    }
}

impl Device for Nic {
    type RxToken<'a> = RxTok;
    type TxToken<'a> = TxTok<'a>;

    fn receive(&mut self, _t: Instant) -> Option<(RxTok, TxTok<'_>)> {
        self.0.poll_rx();
        let frame = self.0.rx_queue.pop_front()?;
        Some((RxTok(frame), TxTok(&mut self.0)))
    }
    fn transmit(&mut self, _t: Instant) -> Option<TxTok<'_>> {
        if self.0.tx_ready() {
            Some(TxTok(&mut self.0))
        } else {
            None
        }
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = MTU;
        c.checksum = ChecksumCapabilities::default();
        c
    }
}

pub struct Stack {
    nic: Nic,
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
}

pub static NET: Mutex<Option<Stack>> = Mutex::new(None);

fn now() -> Instant {
    Instant::from_micros((crate::timer::monotonic_ns() / 1000) as i64)
}

impl Stack {
    pub fn poll(&mut self) {
        self.iface.poll(now(), &mut self.nic, &mut self.sockets);
    }
}

/// The interface's IPv4 address (0.0.0.0 before `init`).
pub fn local_ip() -> [u8; 4] {
    NET.lock()
        .as_ref()
        .and_then(|st| st.iface.ipv4_addr())
        .map(|a| a.octets())
        .unwrap_or([0; 4])
}

/// Bring the NIC and the stack up. `Err` (no virtio-net device) is normal on real hardware
/// until its driver exists.
pub fn init() -> Result<(), &'static str> {
    let mut dev = Nic(VirtioNet::probe()?);
    let mac = dev.0.mac;
    let cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    let mut iface = Interface::new(cfg, &mut dev, now());
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::new(IpAddress::v4(STATIC_IP[0], STATIC_IP[1], STATIC_IP[2], STATIC_IP[3]), 24));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(GATEWAY[0], GATEWAY[1], GATEWAY[2], GATEWAY[3]));
    kprintln!(
        "THOS: net nic          virtio-net {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, {}.{}.{}.{}/24 via {}.{}.{}.{}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        STATIC_IP[0], STATIC_IP[1], STATIC_IP[2], STATIC_IP[3],
        GATEWAY[0], GATEWAY[1], GATEWAY[2], GATEWAY[3]
    );
    *NET.lock() = Some(Stack { nic: dev, iface, sockets: SocketSet::new(Vec::new()) });
    Ok(())
}

/// Ping `dst` once per second, up to `tries` times; the round-trip time in ms of the first
/// reply.
fn ping(dst: [u8; 4], tries: u32) -> Option<u64> {
    let handle: SocketHandle = {
        let mut g = NET.lock();
        let st = g.as_mut()?;
        let rx = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 4], vec![0u8; 512]);
        let tx = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 4], vec![0u8; 512]);
        let mut s = icmp::Socket::new(rx, tx);
        s.bind(icmp::Endpoint::Ident(0x7405)).ok()?;
        st.sockets.add(s)
    };
    let dst_ip = IpAddress::v4(dst[0], dst[1], dst[2], dst[3]);
    let mut result = None;
    'tries: for seq in 0..tries as u16 {
        let sent = crate::timer::monotonic_ns();
        {
            let mut g = NET.lock();
            let st = g.as_mut()?;
            let s = st.sockets.get_mut::<icmp::Socket>(handle);
            let repr = Icmpv4Repr::EchoRequest { ident: 0x7405, seq_no: seq, data: b"THOS-ping" };
            if let Ok(buf) = s.send(repr.buffer_len(), dst_ip) {
                repr.emit(&mut Icmpv4Packet::new_unchecked(buf), &ChecksumCapabilities::default());
            }
        }
        // Poll for up to a second (ARP resolution makes the first echo take two round trips).
        for _ in 0..100 {
            {
                let mut g = NET.lock();
                let st = g.as_mut()?;
                st.poll();
                let s = st.sockets.get_mut::<icmp::Socket>(handle);
                while let Ok((payload, _addr)) = s.recv() {
                    if let Ok(p) = Icmpv4Packet::new_checked(payload) {
                        if let Ok(Icmpv4Repr::EchoReply { ident: 0x7405, .. }) =
                            Icmpv4Repr::parse(&p, &ChecksumCapabilities::default())
                        {
                            result = Some((crate::timer::monotonic_ns() - sent) / 1_000_000);
                            break 'tries;
                        }
                    }
                }
            }
            crate::timer::sleep_ns(10_000_000);
        }
    }
    if let Some(st) = NET.lock().as_mut() {
        st.sockets.remove(handle);
    }
    result
}

/// The network thread: boot-time gateway ping, then keep the stack serviced.
pub extern "C" fn net_thread(_: usize) -> ! {
    match ping(GATEWAY, 3) {
        Some(ms) => kprintln!(
            "THOS: net ok           ping {}.{}.{}.{} answered in {} ms (ARP + ICMP over virtio-net + smoltcp)",
            GATEWAY[0], GATEWAY[1], GATEWAY[2], GATEWAY[3], ms
        ),
        None => kprintln!("THOS: net FAIL         gateway did not answer ping"),
    }
    loop {
        if let Some(st) = NET.lock().as_mut() {
            st.poll();
            crate::net_sock::reap(st);
        }
        crate::timer::sleep_ns(10_000_000);
    }
}
