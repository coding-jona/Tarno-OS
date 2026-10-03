// SPDX-License-Identifier: GPL-2.0-or-later
//! BSD sockets on top of the kernel network service (`net.rs`): AF_INET stream (TCP) and
//! datagram (UDP) sockets as file objects, so `read`/`write`/`close`/`poll` and the socket
//! syscalls all work on them. Blocking calls poll the stack every 2 ms while they wait
//! (interruptible by signals); the Winsock personality will bind the same objects (N3).

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use smoltcp::iface::SocketHandle;
use smoltcp::socket::{icmp, tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};
use spin::Mutex;

use crate::file::{FileOps, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::net::{Stack, NET};

pub const EAGAIN: i64 = -11;
pub const EINTR: i64 = -4;
pub const EBADF: i64 = -9;
pub const EINVAL: i64 = -22;
pub const EPIPE: i64 = -32;
pub const ENOTCONN: i64 = -107;
pub const ENETDOWN: i64 = -100;
pub const ETIMEDOUT: i64 = -110;
pub const ECONNREFUSED: i64 = -111;
pub const EADDRINUSE: i64 = -98;
pub const EISCONN: i64 = -106;
pub const EOPNOTSUPP: i64 = -95;
const S_IFSOCK: u32 = 0o140000;

/// Socket buffer sizes — small on purpose (the reference machine has little RAM): 16 KiB
/// per TCP direction, 4 packets of 2 KiB per UDP direction.
const TCP_BUF: usize = 16 * 1024;
const CONNECT_TIMEOUT_NS: u64 = 15_000_000_000;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Tcp,
    Udp,
    /// `SOCK_DGRAM` + `IPPROTO_ICMP`: Linux's unprivileged "ping socket". The kernel owns the
    /// echo identifier (the socket's "port"), so replies find their way back to the right socket.
    Icmp,
}

struct Sock {
    handle: SocketHandle,
    local_port: u16,
    peer: Option<IpEndpoint>,
    listening: bool,
}

pub struct SockFile {
    kind: Kind,
    /// A raw ICMP socket (`SOCK_RAW`): the program sets the echo identifier itself and receives
    /// replies with their IP header. (`false`: the Linux-style ping socket, kernel-owned id.)
    raw: bool,
    st: Mutex<Sock>,
    nonblock: AtomicBool,
}

static NEXT_PORT: AtomicU16 = AtomicU16::new(49152);

fn ephemeral() -> u16 {
    let p = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    if p < 49152 {
        NEXT_PORT.store(49153, Ordering::Relaxed);
        return 49152;
    }
    p
}

/// Sockets closed by their owner but still draining (FIN/ACK): the net thread removes them
/// once they are `Closed` or after a deadline.
static REAP: Mutex<Vec<(SocketHandle, u64)>> = Mutex::new(Vec::new());

pub fn reap(st: &mut Stack) {
    let now = crate::timer::monotonic_ns();
    let mut list = REAP.lock();
    list.retain(|&(h, deadline)| {
        let done = match st.sockets.get_mut::<tcp::Socket>(h).state() {
            tcp::State::Closed | tcp::State::TimeWait => true,
            _ => now > deadline,
        };
        if done {
            st.sockets.get_mut::<tcp::Socket>(h).abort();
            st.sockets.remove(h);
        }
        !done
    });
}

fn new_tcp() -> tcp::Socket<'static> {
    tcp::Socket::new(tcp::SocketBuffer::new(vec![0u8; TCP_BUF]), tcp::SocketBuffer::new(vec![0u8; TCP_BUF]))
}

fn new_icmp() -> icmp::Socket<'static> {
    icmp::Socket::new(
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0u8; 8 * 1024]),
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0u8; 8 * 1024]),
    )
}

/// The Internet checksum of an ICMP message (checksum field already zeroed).
fn icmp_checksum(msg: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < msg.len() {
        sum += u16::from_be_bytes([msg[i], msg[i + 1]]) as u32;
        i += 2;
    }
    if i < msg.len() {
        sum += (msg[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn new_udp() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 4 * 2048]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 4 * 2048]),
    )
}

/// Run `f` with the stack locked and freshly polled; keep retrying (sleeping 2 ms between
/// attempts, which a signal cuts short) until it yields `Some`.
fn wait_until<T>(
    nonblock: bool,
    timeout_ns: Option<u64>,
    mut f: impl FnMut(&mut Stack) -> Option<Result<T, i64>>,
) -> Result<T, i64> {
    let deadline = timeout_ns.map(|t| crate::timer::monotonic_ns().saturating_add(t));
    loop {
        {
            let mut g = NET.lock();
            let st = g.as_mut().ok_or(ENETDOWN)?;
            st.poll();
            if let Some(r) = f(st) {
                return r;
            }
        }
        if nonblock {
            return Err(EAGAIN);
        }
        if crate::signal::interrupted() {
            return Err(EINTR);
        }
        if deadline.is_some_and(|d| crate::timer::monotonic_ns() > d) {
            return Err(ETIMEDOUT);
        }
        crate::timer::sleep_ns(2_000_000);
    }
}

pub fn endpoint(ip: [u8; 4], port: u16) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3])), port)
}

fn v4(ep: &IpEndpoint) -> ([u8; 4], u16) {
    match ep.addr {
        IpAddress::Ipv4(a) => (a.octets(), ep.port),
    }
}

impl SockFile {
    pub fn create(kind: Kind) -> Result<Arc<SockFile>, i64> {
        Self::create_icmp_aware(kind, false)
    }

    pub fn create_icmp_aware(kind: Kind, raw: bool) -> Result<Arc<SockFile>, i64> {
        let mut g = NET.lock();
        let st = g.as_mut().ok_or(ENETDOWN)?;
        let mut local_port = 0;
        let handle = match kind {
            Kind::Tcp => st.sockets.add(new_tcp()),
            Kind::Udp => st.sockets.add(new_udp()),
            Kind::Icmp => {
                let mut s = new_icmp();
                if !raw {
                    local_port = ephemeral();
                    let _ = s.bind(icmp::Endpoint::Ident(local_port));
                } // a raw socket binds to the identifier of its first echo request
                st.sockets.add(s)
            }
        };
        Ok(Arc::new(SockFile {
            kind,
            raw,
            st: Mutex::new(Sock { handle, local_port, peer: None, listening: false }),
            nonblock: AtomicBool::new(false),
        }))
    }

    pub fn bind(&self, port: u16) -> i64 {
        let mut s = self.st.lock();
        let port = if port == 0 { ephemeral() } else { port };
        if self.kind == Kind::Icmp {
            return 0; // the identifier is fixed at creation
        }
        if self.kind == Kind::Udp {
            let mut g = NET.lock();
            let Some(st) = g.as_mut() else { return ENETDOWN };
            if st.sockets.get_mut::<udp::Socket>(s.handle).bind(port).is_err() {
                return EADDRINUSE;
            }
        }
        s.local_port = port;
        0
    }

    pub fn listen(&self) -> i64 {
        if self.kind != Kind::Tcp {
            return EOPNOTSUPP;
        }
        let mut s = self.st.lock();
        if s.local_port == 0 {
            s.local_port = ephemeral();
        }
        let mut g = NET.lock();
        let Some(st) = g.as_mut() else { return ENETDOWN };
        if st.sockets.get_mut::<tcp::Socket>(s.handle).listen(s.local_port).is_err() {
            return EADDRINUSE;
        }
        s.listening = true;
        0
    }

    pub fn connect(&self, ip: [u8; 4], port: u16) -> i64 {
        let (handle, local) = {
            let mut s = self.st.lock();
            if s.local_port == 0 {
                s.local_port = ephemeral();
            }
            s.peer = Some(endpoint(ip, port));
            (s.handle, s.local_port)
        };
        let nb = self.is_nonblock();
        match self.kind {
            Kind::Icmp => 0, // remember the peer (done above); sending needs nothing else
            Kind::Udp => {
                // "Connected" UDP: just remember the peer, and make sure we are bound.
                let mut g = NET.lock();
                let Some(st) = g.as_mut() else { return ENETDOWN };
                let u = st.sockets.get_mut::<udp::Socket>(handle);
                if !u.is_open() {
                    let _ = u.bind(local);
                }
                0
            }
            Kind::Tcp => {
                {
                    let mut g = NET.lock();
                    let Some(st) = g.as_mut() else { return ENETDOWN };
                    let Stack { iface, sockets, .. } = st;
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    if sock.is_active() {
                        return EISCONN;
                    }
                    if sock.connect(iface.context(), (IpAddress::Ipv4(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3])), port), local).is_err() {
                        return EINVAL;
                    }
                }
                if nb {
                    return -115; // EINPROGRESS
                }
                wait_until(false, Some(CONNECT_TIMEOUT_NS), |st| {
                    match st.sockets.get_mut::<tcp::Socket>(handle).state() {
                        tcp::State::Established => Some(Ok(0)),
                        tcp::State::SynSent | tcp::State::SynReceived => None,
                        _ => Some(Err(ECONNREFUSED)),
                    }
                })
                .unwrap_or_else(|e| e)
            }
        }
    }

    /// `accept`: wait for an established connection, hand it out as a new socket and put a
    /// fresh listener behind the old one.
    pub fn accept(&self) -> Result<(Arc<SockFile>, ([u8; 4], u16)), i64> {
        let (handle, port, listening) = {
            let s = self.st.lock();
            (s.handle, s.local_port, s.listening)
        };
        if !listening {
            return Err(EINVAL);
        }
        let remote = wait_until(self.is_nonblock(), None, |st| {
            let s = st.sockets.get_mut::<tcp::Socket>(handle);
            match s.state() {
                tcp::State::Established => Some(Ok(s.remote_endpoint())),
                tcp::State::Listen | tcp::State::SynReceived => None,
                _ => Some(Err(ECONNREFUSED)),
            }
        })?;
        let fresh = {
            let mut g = NET.lock();
            let st = g.as_mut().ok_or(ENETDOWN)?;
            let fresh = st.sockets.add(new_tcp());
            let _ = st.sockets.get_mut::<tcp::Socket>(fresh).listen(port);
            fresh
        };
        let conn = core::mem::replace(&mut self.st.lock().handle, fresh);
        let peer = remote.map(|e| v4(&e)).unwrap_or(([0; 4], 0));
        Ok((
            Arc::new(SockFile {
                kind: Kind::Tcp,
                st: Mutex::new(Sock { handle: conn, local_port: port, peer: remote, listening: false }),
                nonblock: AtomicBool::new(false),
                raw: false,
            }),
            peer,
        ))
    }

    pub fn send_to(&self, data: &[u8], to: Option<([u8; 4], u16)>) -> i64 {
        let (handle, peer, lport) = {
            let s = self.st.lock();
            (s.handle, s.peer, s.local_port)
        };
        let nb = self.is_nonblock();
        match self.kind {
            Kind::Tcp => wait_until(nb, None, |st| {
                let s = st.sockets.get_mut::<tcp::Socket>(handle);
                match s.state() {
                    tcp::State::Closed | tcp::State::Listen | tcp::State::SynSent | tcp::State::SynReceived => {
                        if s.state() == tcp::State::Closed || s.state() == tcp::State::Listen {
                            return Some(Err(ENOTCONN));
                        }
                        return None; // still connecting
                    }
                    _ => {}
                }
                if !s.may_send() {
                    return Some(Err(EPIPE));
                }
                if s.can_send() {
                    return Some(Ok(s.send_slice(data).unwrap_or(0) as i64));
                }
                None
            })
            .unwrap_or_else(|e| e),
            Kind::Icmp => {
                let Some(dst) = to.map(|(ip, p)| endpoint(ip, p)).or(peer) else { return -89 };
                if data.len() < 8 {
                    return EINVAL;
                }
                let mut msg = data.to_vec();
                if self.raw {
                    // Raw: the program's identifier stays; bind the socket to it on first use so
                    // the matching replies come back here.
                    let id = u16::from_be_bytes([msg[4], msg[5]]);
                    let mut g = NET.lock();
                    if let Some(st) = g.as_mut() {
                        let s = st.sockets.get_mut::<icmp::Socket>(handle);
                        if !s.is_open() {
                            let _ = s.bind(icmp::Endpoint::Ident(id));
                        }
                    }
                } else {
                    // Ping socket: stamp our identifier into the echo header, fix the checksum.
                    msg[4..6].copy_from_slice(&lport.to_be_bytes());
                    msg[2] = 0;
                    msg[3] = 0;
                    let ck = icmp_checksum(&msg);
                    msg[2..4].copy_from_slice(&ck.to_be_bytes());
                }
                wait_until(nb, None, |st| {
                    let s = st.sockets.get_mut::<icmp::Socket>(handle);
                    match s.send(msg.len(), dst.addr) {
                        Ok(buf) => {
                            buf.copy_from_slice(&msg);
                            Some(Ok(data.len() as i64))
                        }
                        Err(_) => None,
                    }
                })
                .unwrap_or_else(|e| e)
            }
            Kind::Udp => {
                let Some(dst) = to.map(|(ip, p)| endpoint(ip, p)).or(peer) else { return -89 /* EDESTADDRREQ */ };
                wait_until(nb, None, |st| {
                    let u = st.sockets.get_mut::<udp::Socket>(handle);
                    if !u.is_open() {
                        let _ = u.bind(if lport == 0 { ephemeral() } else { lport });
                    }
                    match u.send_slice(data, dst) {
                        Ok(()) => Some(Ok(data.len() as i64)),
                        Err(udp::SendError::BufferFull) => None,
                        Err(_) => Some(Err(EINVAL)),
                    }
                })
                .unwrap_or_else(|e| e)
            }
        }
    }

    /// Receive into `buf`; for UDP also the sender.
    pub fn recv_from(&self, buf: &mut [u8]) -> (i64, Option<([u8; 4], u16)>) {
        let (handle, kind) = (self.st.lock().handle, self.kind);
        let raw = self.raw;
        let nb = self.is_nonblock();
        let r = wait_until(nb, None, |st| match kind {
            Kind::Tcp => {
                let s = st.sockets.get_mut::<tcp::Socket>(handle);
                if matches!(s.state(), tcp::State::Closed | tcp::State::Listen) && !s.can_recv() {
                    return Some(Err(ENOTCONN));
                }
                if s.can_recv() {
                    Some(Ok((s.recv_slice(buf).unwrap_or(0) as i64, None)))
                } else if !s.may_recv() {
                    Some(Ok((0, None))) // peer closed: EOF
                } else {
                    None
                }
            }
            Kind::Icmp => {
                let my_ip = crate::net::local_ip_of(st);
                let s = st.sockets.get_mut::<icmp::Socket>(handle);
                match s.recv() {
                    Ok((payload, addr)) => {
                        let ip = match addr {
                            IpAddress::Ipv4(a) => a.octets(),
                        };
                        if raw {
                            // Raw sockets hand over the IP header too: build one for the reply.
                            let total = 20 + payload.len();
                            let mut pkt = vec![0u8; total];
                            pkt[0] = 0x45;
                            pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
                            pkt[8] = 64; // ttl
                            pkt[9] = 1; // ICMP
                            pkt[12..16].copy_from_slice(&ip);
                            pkt[16..20].copy_from_slice(&my_ip);
                            let mut sum = 0u32;
                            for i in (0..20).step_by(2) {
                                sum += u16::from_be_bytes([pkt[i], pkt[i + 1]]) as u32;
                            }
                            while sum >> 16 != 0 {
                                sum = (sum & 0xFFFF) + (sum >> 16);
                            }
                            pkt[10..12].copy_from_slice(&(!(sum as u16)).to_be_bytes());
                            pkt[20..].copy_from_slice(payload);
                            let n = pkt.len().min(buf.len());
                            buf[..n].copy_from_slice(&pkt[..n]);
                            Some(Ok((n as i64, Some((ip, 0)))))
                        } else {
                            let n = payload.len().min(buf.len());
                            buf[..n].copy_from_slice(&payload[..n]);
                            Some(Ok((n as i64, Some((ip, 0)))))
                        }
                    }
                    Err(_) => None,
                }
            }
            Kind::Udp => {
                let u = st.sockets.get_mut::<udp::Socket>(handle);
                match u.recv_slice(buf) {
                    Ok((n, meta)) => Some(Ok((n as i64, Some(v4(&meta.endpoint))))),
                    Err(_) => None,
                }
            }
        });
        match r {
            Ok(v) => v,
            Err(e) => (e, None),
        }
    }

    pub fn shutdown(&self, how: u32) -> i64 {
        if self.kind == Kind::Tcp && how != 0 {
            let h = self.st.lock().handle;
            if let Some(st) = NET.lock().as_mut() {
                st.sockets.get_mut::<tcp::Socket>(h).close(); // FIN: no more data from us
            }
        }
        0
    }

    pub fn local_addr(&self) -> ([u8; 4], u16) {
        let port = self.st.lock().local_port;
        let ip = crate::net::local_ip();
        (ip, port)
    }

    pub fn peer_addr(&self) -> Option<([u8; 4], u16)> {
        self.st.lock().peer.map(|e| v4(&e))
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// `SO_ERROR`: a connection that failed leaves the socket closed.
    pub fn pending_error(&self) -> i64 {
        0
    }
}

impl Drop for SockFile {
    fn drop(&mut self) {
        let h = self.st.lock().handle;
        let mut g = NET.lock();
        let Some(st) = g.as_mut() else { return };
        match self.kind {
            Kind::Udp => {
                st.sockets.remove(h);
            }
            Kind::Icmp => {
                st.sockets.remove(h);
            }
            Kind::Tcp => {
                let s = st.sockets.get_mut::<tcp::Socket>(h);
                if matches!(s.state(), tcp::State::Listen | tcp::State::SynSent | tcp::State::Closed) {
                    s.abort();
                    st.sockets.remove(h);
                } else {
                    s.close(); // graceful: let the FIN and queued data go out
                    REAP.lock().push((h, crate::timer::monotonic_ns() + 10_000_000_000));
                }
            }
        }
    }
}

impl FileOps for SockFile {
    fn read(&self, buf: &mut [u8]) -> i64 {
        self.recv_from(buf).0
    }
    fn write(&self, buf: &[u8]) -> i64 {
        self.send_to(buf, None)
    }
    fn seek(&self, _o: i64, _w: u32) -> i64 {
        -29 // ESPIPE
    }
    fn stat(&self) -> (u32, u64) {
        (S_IFSOCK | 0o777, 0)
    }
    fn as_socket(&self) -> Option<&SockFile> {
        Some(self)
    }
    fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Relaxed);
    }
    fn is_nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }
    fn poll_mask(&self, want: u16) -> u16 {
        let (h, kind, listening) = {
            let s = self.st.lock();
            (s.handle, self.kind, s.listening)
        };
        let mut g = NET.lock();
        let Some(st) = g.as_mut() else { return POLLERR };
        st.poll();
        let mut r = 0;
        match kind {
            Kind::Tcp => {
                let s = st.sockets.get_mut::<tcp::Socket>(h);
                if listening {
                    if s.state() == tcp::State::Established {
                        r |= POLLIN;
                    }
                } else {
                    if s.can_recv() || (!s.may_recv() && s.state() != tcp::State::SynSent && s.state() != tcp::State::SynReceived) {
                        r |= POLLIN; // data, or EOF/closed (read won't block)
                    }
                    if s.state() == tcp::State::Established && s.can_send() {
                        r |= POLLOUT;
                    }
                    if matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait) {
                        r |= POLLHUP;
                    }
                }
            }
            Kind::Icmp => {
                let s = st.sockets.get_mut::<icmp::Socket>(h);
                if s.can_recv() {
                    r |= POLLIN;
                }
                if s.can_send() {
                    r |= POLLOUT;
                }
            }
            Kind::Udp => {
                let u = st.sockets.get_mut::<udp::Socket>(h);
                if u.can_recv() {
                    r |= POLLIN;
                }
                if u.can_send() {
                    r |= POLLOUT;
                }
            }
        }
        r & (want | POLLERR | POLLHUP)
    }
}
