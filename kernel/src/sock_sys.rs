// SPDX-License-Identifier: GPL-2.0-or-later
//! The Linux socket syscalls (AF_INET only) and a real `poll`, on top of `net_sock`.

use alloc::sync::Arc;

use crate::file::{FileOps, POLLERR, POLLIN, POLLOUT};
use crate::net_sock::{Kind, SockFile};
use crate::{process, sched, signal, usercopy};

const EBADF: i64 = -9;
const EFAULT: i64 = usercopy::EFAULT;
const EINVAL: i64 = -22;
const ENOTSOCK: i64 = -88;
const EAFNOSUPPORT: i64 = -97;
const EPROTONOSUPPORT: i64 = -93;
const ESOCKTNOSUPPORT: i64 = -94;

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const SOCK_NONBLOCK: u64 = 0o4000;
const SOCK_CLOEXEC: u64 = 0o2000000;

fn sock_of(fd: u64) -> Result<Arc<dyn FileOps>, i64> {
    let f = sched::current().task().and_then(|t| t.fd_get(fd as i32)).ok_or(EBADF)?;
    if f.as_socket().is_none() {
        return Err(ENOTSOCK);
    }
    Ok(f)
}

/// `struct sockaddr_in` at `ptr`/`len` -> (ip, port).
fn read_sockaddr(ptr: u64, len: u64) -> Result<([u8; 4], u16), i64> {
    if len < 8 {
        return Err(EINVAL);
    }
    let b = usercopy::slice(ptr, 8)?;
    if u16::from_le_bytes([b[0], b[1]]) as u64 != AF_INET {
        return Err(EAFNOSUPPORT);
    }
    Ok(([b[4], b[5], b[6], b[7]], u16::from_be_bytes([b[2], b[3]])))
}

/// Write a `sockaddr_in` to `ptr` and the length to `lenp` (truncating like Linux does).
fn write_sockaddr(ptr: u64, lenp: u64, addr: ([u8; 4], u16)) -> i64 {
    if ptr == 0 || lenp == 0 {
        return 0;
    }
    let Ok(cap) = usercopy::read_u32(lenp) else { return EFAULT };
    let mut sa = [0u8; 16];
    sa[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    sa[2..4].copy_from_slice(&addr.1.to_be_bytes());
    sa[4..8].copy_from_slice(&addr.0);
    let n = (cap as usize).min(16);
    match usercopy::slice_mut(ptr, n) {
        Ok(b) => b.copy_from_slice(&sa[..n]),
        Err(e) => return e,
    }
    match usercopy::write_u32(lenp, 16) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

pub fn sys_socket(domain: u64, ty: u64, proto: u64) -> i64 {
    if domain != AF_INET {
        return EAFNOSUPPORT;
    }
    let kind = match ty & 0xF {
        SOCK_STREAM => Kind::Tcp,
        SOCK_DGRAM if proto == 1 => Kind::Icmp, // IPPROTO_ICMP: an unprivileged ping socket
        SOCK_DGRAM => Kind::Udp,
        3 if proto == 1 => Kind::Icmp, // SOCK_RAW + ICMP: what BusyBox `ping` opens
        3 => return -1,                // other raw sockets: EPERM
        _ => return ESOCKTNOSUPPORT,
    };
    let _ = EPROTONOSUPPORT;
    let sock = match SockFile::create_icmp_aware(kind, ty & 0xF == 3) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if ty & SOCK_NONBLOCK != 0 {
        sock.set_nonblock(true);
    }
    let Some(task) = sched::current().task() else { return EBADF };
    task.fd_alloc_flags(sock, ty & SOCK_CLOEXEC != 0) as i64
}

pub fn sys_bind(fd: u64, addr: u64, len: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    match read_sockaddr(addr, len) {
        Ok((_, port)) => f.as_socket().unwrap().bind(port),
        Err(e) => e,
    }
}

pub fn sys_listen(fd: u64) -> i64 {
    match sock_of(fd) {
        Ok(f) => f.as_socket().unwrap().listen(),
        Err(e) => e,
    }
}

pub fn sys_connect(fd: u64, addr: u64, len: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    match read_sockaddr(addr, len) {
        Ok((ip, port)) => f.as_socket().unwrap().connect(ip, port),
        Err(e) => e,
    }
}

pub fn sys_accept(fd: u64, addr: u64, lenp: u64, flags: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    match f.as_socket().unwrap().accept() {
        Ok((conn, peer)) => {
            let r = write_sockaddr(addr, lenp, peer);
            if r < 0 {
                return r;
            }
            if flags & SOCK_NONBLOCK != 0 {
                conn.set_nonblock(true);
            }
            let Some(task) = sched::current().task() else { return EBADF };
            task.fd_alloc_flags(conn, flags & SOCK_CLOEXEC != 0) as i64
        }
        Err(e) => e,
    }
}

pub fn sys_sendto(fd: u64, buf: u64, len: u64, addr: u64, alen: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    let to = if addr != 0 {
        match read_sockaddr(addr, alen) { Ok(a) => Some(a), Err(e) => return e }
    } else {
        None
    };
    match usercopy::slice(buf, len as usize) {
        Ok(data) => f.as_socket().unwrap().send_to(data, to),
        Err(e) => e,
    }
}

pub fn sys_recvfrom(fd: u64, buf: u64, len: u64, addr: u64, lenp: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    let out = match usercopy::slice_mut(buf, len as usize) { Ok(b) => b, Err(e) => return e };
    let (n, from) = f.as_socket().unwrap().recv_from(out);
    if n >= 0 {
        if let Some(a) = from {
            let r = write_sockaddr(addr, lenp, a);
            if r < 0 {
                return r;
            }
        }
    }
    n
}

pub fn sys_shutdown(fd: u64, how: u64) -> i64 {
    match sock_of(fd) {
        Ok(f) => f.as_socket().unwrap().shutdown(how as u32),
        Err(e) => e,
    }
}

pub fn sys_getsockname(fd: u64, addr: u64, lenp: u64) -> i64 {
    match sock_of(fd) {
        Ok(f) => write_sockaddr(addr, lenp, f.as_socket().unwrap().local_addr()),
        Err(e) => e,
    }
}

pub fn sys_getpeername(fd: u64, addr: u64, lenp: u64) -> i64 {
    match sock_of(fd) {
        Ok(f) => match f.as_socket().unwrap().peer_addr() {
            Some(p) => write_sockaddr(addr, lenp, p),
            None => -107, // ENOTCONN
        },
        Err(e) => e,
    }
}

/// `setsockopt`: every option is accepted and ignored (REUSEADDR, NODELAY, KEEPALIVE, ...).
pub fn sys_setsockopt(fd: u64) -> i64 {
    match sock_of(fd) {
        Ok(_) => 0,
        Err(e) => e,
    }
}

/// `getsockopt`: `SO_ERROR` / `SO_TYPE` are real; everything else reads as 0.
pub fn sys_getsockopt(fd: u64, level: u64, name: u64, val: u64, lenp: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    let v: u32 = if level == 1 && name == 4 {
        0 // SO_ERROR
    } else if level == 1 && name == 3 {
        if f.as_socket().unwrap().kind() == Kind::Tcp { 1 } else { 2 } // SO_TYPE
    } else {
        0
    };
    if val == 0 || lenp == 0 {
        return 0;
    }
    if let Err(e) = usercopy::write_u32(val, v) {
        return e;
    }
    match usercopy::write_u32(lenp, 4) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// `poll(fds, nfds, timeout_ms)`; `timeout_ns < 0` = forever. Real readiness via
/// `FileOps::poll_mask`; sleeps 2 ms between scans (a signal ends it with EINTR).
pub fn sys_poll(fds: u64, nfds: u64, timeout_ns: i64) -> i64 {
    let n = nfds as usize;
    if n > 4096 {
        return EINVAL;
    }
    if n > 0 && !usercopy::user_ok(fds, n * 8, true) {
        return EFAULT;
    }
    let deadline = (timeout_ns >= 0).then(|| crate::timer::monotonic_ns().saturating_add(timeout_ns as u64));
    let task = match sched::current().task() { Some(t) => t, None => return EBADF };
    loop {
        let mut ready = 0i64;
        for i in 0..n {
            let base = fds + (i * 8) as u64;
            let Ok(fd) = usercopy::read_u32(base) else { return EFAULT };
            let Ok(ev) = usercopy::read_u32(base + 4) else { return EFAULT };
            let (want, _) = (ev as u16, ());
            let rev: u16 = if (fd as i32) < 0 {
                0
            } else {
                match task.fd_get(fd as i32) {
                    Some(f) => f.poll_mask(want | POLLIN * 0) & (want | POLLERR | crate::file::POLLHUP),
                    None => 0x20, // POLLNVAL
                }
            };
            let _ = usercopy::slice_mut(base + 6, 2).map(|b| b.copy_from_slice(&rev.to_le_bytes()));
            if rev != 0 {
                ready += 1;
            }
        }
        if ready > 0 || n == 0 && deadline.is_none() && false {
            return ready;
        }
        if signal::interrupted() {
            return -4;
        }
        match deadline {
            Some(d) if crate::timer::monotonic_ns() >= d => return 0,
            _ => {}
        }
        crate::timer::sleep_ns(2_000_000);
    }
}

// ---------------------------------------------------------------------------
//  sendmsg / recvmsg / sendmmsg / recvmmsg (glibc's resolver sends its A and AAAA queries with
//  one sendmmsg). `struct msghdr`: name @0, namelen @8, iov @16, iovlen @24, control @32,
//  controllen @40, flags @48 (56 bytes); `struct mmsghdr` = msghdr + u32 msg_len (64 bytes).
// ---------------------------------------------------------------------------

const MSG_MAX: usize = 64 * 1024;

/// Gather the iovec array of a msghdr into one buffer.
fn gather_iov(msg: u64) -> Result<alloc::vec::Vec<u8>, i64> {
    let iov = usercopy::read_u64(msg + 16)?;
    let n = usercopy::read_u64(msg + 24)?;
    if n > 1024 {
        return Err(EINVAL);
    }
    let mut out = alloc::vec::Vec::new();
    for i in 0..n {
        let base = usercopy::read_u64(iov + 16 * i)?;
        let len = usercopy::read_u64(iov + 16 * i + 8)? as usize;
        if out.len() + len > MSG_MAX {
            return Err(-90); // EMSGSIZE
        }
        out.extend_from_slice(usercopy::slice(base, len)?);
    }
    Ok(out)
}

pub fn sys_sendmsg(fd: u64, msg: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    let name = usercopy::read_u64(msg).unwrap_or(0);
    let namelen = usercopy::read_u32(msg + 8).unwrap_or(0) as u64;
    let to = if name != 0 {
        match read_sockaddr(name, namelen) { Ok(a) => Some(a), Err(e) => return e }
    } else {
        None
    };
    match gather_iov(msg) {
        Ok(data) => f.as_socket().unwrap().send_to(&data, to),
        Err(e) => e,
    }
}

pub fn sys_recvmsg(fd: u64, msg: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    let iov = match usercopy::read_u64(msg + 16) { Ok(v) => v, Err(e) => return e };
    let n = match usercopy::read_u64(msg + 24) { Ok(v) => v, Err(e) => return e };
    if n > 1024 {
        return EINVAL;
    }
    let mut total = 0usize;
    for i in 0..n {
        match usercopy::read_u64(iov + 16 * i + 8) {
            Ok(l) => total += l as usize,
            Err(e) => return e,
        }
    }
    let mut buf = alloc::vec![0u8; total.min(MSG_MAX)];
    let (got, from) = f.as_socket().unwrap().recv_from(&mut buf);
    if got < 0 {
        return got;
    }
    // scatter
    let mut off = 0usize;
    for i in 0..n {
        if off >= got as usize {
            break;
        }
        let (Ok(base), Ok(len)) = (usercopy::read_u64(iov + 16 * i), usercopy::read_u64(iov + 16 * i + 8)) else { return EFAULT };
        let take = (len as usize).min(got as usize - off);
        match usercopy::slice_mut(base, take) {
            Ok(b) => b.copy_from_slice(&buf[off..off + take]),
            Err(e) => return e,
        }
        off += take;
    }
    // sender address, if the caller asked for one
    if let (Some(a), Ok(name)) = (from, usercopy::read_u64(msg)) {
        if name != 0 {
            let lenp = msg + 8; // msg_namelen is a u32 in place
            let r = write_sockaddr(name, lenp, a);
            if r < 0 {
                return r;
            }
        }
    }
    let _ = usercopy::write_u64(msg + 40, 0); // no control data
    let _ = usercopy::write_u32(msg + 48, 0); // msg_flags
    got
}

pub fn sys_sendmmsg(fd: u64, vec: u64, vlen: u64) -> i64 {
    let mut sent = 0i64;
    for i in 0..vlen.min(1024) {
        let m = vec + 64 * i;
        let r = sys_sendmsg(fd, m);
        if r < 0 {
            return if sent > 0 { sent } else { r };
        }
        if usercopy::write_u32(m + 56, r as u32).is_err() {
            return EFAULT;
        }
        sent += 1;
    }
    sent
}

pub fn sys_recvmmsg(fd: u64, vec: u64, vlen: u64) -> i64 {
    let f = match sock_of(fd) { Ok(f) => f, Err(e) => return e };
    let mut got = 0i64;
    for i in 0..vlen.min(1024) {
        // the first message may block; later ones only if something is already waiting
        if i > 0 && f.poll_mask(POLLIN) & POLLIN == 0 {
            break;
        }
        let m = vec + 64 * i;
        let r = sys_recvmsg(fd, m);
        if r < 0 {
            return if got > 0 { got } else { r };
        }
        if usercopy::write_u32(m + 56, r as u32).is_err() {
            return EFAULT;
        }
        got += 1;
    }
    got
}

/// `select` / `pselect6`: the `fd_set` flavour of [`sys_poll`]. `timeout_ns < 0` = forever.
pub fn sys_select(nfds: u64, rd: u64, wr: u64, ex: u64, timeout_ns: i64) -> i64 {
    if nfds > 1024 {
        return EINVAL;
    }
    let words = (nfds as usize).div_ceil(64);
    let bytes = words * 8;
    let read_set = |p: u64| -> Result<alloc::vec::Vec<u64>, i64> {
        if p == 0 {
            return Ok(alloc::vec![0u64; words]);
        }
        (0..words).map(|i| usercopy::read_u64(p + 8 * i as u64)).collect()
    };
    let (want_r, want_w, _want_e) = match (read_set(rd), read_set(wr), read_set(ex)) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    let task = match sched::current().task() { Some(t) => t, None => return EBADF };
    let deadline = (timeout_ns >= 0).then(|| crate::timer::monotonic_ns().saturating_add(timeout_ns as u64));
    loop {
        let mut got_r = alloc::vec![0u64; words];
        let mut got_w = alloc::vec![0u64; words];
        let mut ready = 0i64;
        for fd in 0..nfds as usize {
            let (w, b) = (fd / 64, 1u64 << (fd % 64));
            let (r_on, w_on) = (want_r[w] & b != 0, want_w[w] & b != 0);
            if !r_on && !w_on {
                continue;
            }
            let Some(f) = task.fd_get(fd as i32) else { return EBADF };
            let ev = f.poll_mask((if r_on { POLLIN } else { 0 }) | (if w_on { POLLOUT } else { 0 }));
            if r_on && ev & (POLLIN | crate::file::POLLHUP | POLLERR) != 0 {
                got_r[w] |= b;
                ready += 1;
            }
            if w_on && ev & (POLLOUT | POLLERR) != 0 {
                got_w[w] |= b;
                ready += 1;
            }
        }
        let timed_out = deadline.is_some_and(|d| crate::timer::monotonic_ns() >= d);
        if ready > 0 || timed_out {
            for (p, set) in [(rd, &got_r), (wr, &got_w), (ex, &alloc::vec![0u64; words])] {
                if p != 0 {
                    for (i, v) in set.iter().enumerate() {
                        if usercopy::write_u64(p + 8 * i as u64, *v).is_err() {
                            return EFAULT;
                        }
                    }
                }
            }
            let _ = bytes;
            return ready;
        }
        if signal::interrupted() {
            return -4;
        }
        crate::timer::sleep_ns(2_000_000);
    }
}

#[allow(dead_code)]
fn _unused() {
    let _ = (POLLOUT, process::current_pid());
}
