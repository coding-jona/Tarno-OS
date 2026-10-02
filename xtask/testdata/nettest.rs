// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: BSD sockets over the virtio-net + smoltcp stack. `nettest <tcp-port> <udp-port>`
// talks to servers on the host (QEMU user networking: the host is 10.0.2.2): a TCP request/
// response to EOF, and a UDP echo that is waited for with poll(). Raw syscalls, no libc.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         in("r10") d, in("r8") e, in("r9") f, out("rcx") _, out("r11") _);
    r
}

fn sockaddr(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut a = [0u8; 16];
    a[0] = 2; // AF_INET
    a[2..4].copy_from_slice(&port.to_be_bytes());
    a[4..8].copy_from_slice(&ip);
    a
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tcp_port: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let udp_port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let host = [10, 0, 2, 2];
    let mut bad: Vec<String> = Vec::new();

    // --- TCP ---
    unsafe {
        let s = sys(41, 2, 1, 0, 0, 0, 0);
        if s < 0 { bad.push(format!("socket(TCP) = {s}")); } else {
            let sa = sockaddr(host, tcp_port);
            let r = sys(42, s as u64, sa.as_ptr() as u64, 16, 0, 0, 0);
            if r != 0 { bad.push(format!("connect(TCP) = {r}")); } else {
                let req = b"GET / HTTP/1.0\r\n\r\n";
                let w = sys(1, s as u64, req.as_ptr() as u64, req.len() as u64, 0, 0, 0);
                if w != req.len() as i64 { bad.push(format!("write(TCP) = {w}")); }
                let mut resp = Vec::new();
                let mut buf = [0u8; 512];
                loop {
                    let n = sys(0, s as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0);
                    if n <= 0 { if n < 0 { bad.push(format!("read(TCP) = {n}")); } break; }
                    resp.extend_from_slice(&buf[..n as usize]);
                }
                if !String::from_utf8_lossy(&resp).contains("THOS-NET-OK-TCP") {
                    bad.push(format!("TCP response was {:?}", String::from_utf8_lossy(&resp)));
                }
                // getpeername must report the host.
                let mut pa = [0u8; 16];
                let mut pl = 16u32;
                let r = sys(52, s as u64, pa.as_mut_ptr() as u64, &mut pl as *mut u32 as u64, 0, 0, 0);
                if r != 0 || pa[4..8] != host || u16::from_be_bytes([pa[2], pa[3]]) != tcp_port {
                    bad.push(format!("getpeername = {r} {:?}", pa));
                }
            }
            sys(3, s as u64, 0, 0, 0, 0, 0);
        }
    }

    // --- UDP + poll ---
    unsafe {
        let s = sys(41, 2, 2, 0, 0, 0, 0);
        if s < 0 { bad.push(format!("socket(UDP) = {s}")); } else {
            let sa = sockaddr(host, udp_port);
            let msg = b"ping-udp";
            let w = sys(44, s as u64, msg.as_ptr() as u64, msg.len() as u64, 0, sa.as_ptr() as u64, 16);
            if w != msg.len() as i64 { bad.push(format!("sendto(UDP) = {w}")); }
            // pollfd { fd: i32, events: i16 (POLLIN), revents: i16 }
            let mut pfd = [0u8; 8];
            pfd[0..4].copy_from_slice(&(s as i32).to_le_bytes());
            pfd[4..6].copy_from_slice(&1i16.to_le_bytes());
            let r = sys(7, pfd.as_mut_ptr() as u64, 1, 5000, 0, 0, 0);
            if r != 1 || i16::from_le_bytes([pfd[6], pfd[7]]) & 1 == 0 { bad.push(format!("poll(UDP) = {r}")); } else {
                let mut buf = [0u8; 64];
                let mut from = [0u8; 16];
                let mut fl = 16u32;
                let n = sys(45, s as u64, buf.as_mut_ptr() as u64, 64, 0, from.as_mut_ptr() as u64, &mut fl as *mut u32 as u64);
                if n < 0 || &buf[..n as usize] != b"echo:ping-udp" {
                    bad.push(format!("recvfrom(UDP) = {n} {:?}", &buf[..n.max(0) as usize]));
                }
                if from[4..8] != host { bad.push(format!("UDP sender was {:?}", &from[4..8])); }
            }
            // poll with nothing to read and a short timeout must time out with 0.
            let mut pfd = [0u8; 8];
            pfd[0..4].copy_from_slice(&(s as i32).to_le_bytes());
            pfd[4..6].copy_from_slice(&1i16.to_le_bytes());
            let r = sys(7, pfd.as_mut_ptr() as u64, 1, 100, 0, 0, 0);
            if r != 0 { bad.push(format!("idle poll timeout = {r} (want 0)")); }
            sys(3, s as u64, 0, 0, 0, 0, 0);
        }
    }

    if bad.is_empty() { println!("net-sock ok: tcp+udp+poll"); } else { for b in &bad { println!("net-sock FAIL: {b}"); } }
}
