// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: streaming file I/O. Write a 1 MiB file in 4 KiB writes (write-back, not a
// whole-file rewrite per call), read it back through the streaming path (it is larger than
// the streaming threshold) with odd read sizes and random seeks, then delete it.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         out("rcx") _, out("r11") _);
    r
}

fn now_ms() -> u64 {
    let mut ts = [0i64; 2];
    unsafe { sys(228, 1, ts.as_mut_ptr() as u64, 0) };
    ts[0] as u64 * 1000 + ts[1] as u64 / 1_000_000
}

fn byte_at(off: u64) -> u8 {
    // a pattern that depends on the position, so a wrong offset shows up
    (off.wrapping_mul(2654435761) >> 7) as u8 ^ (off as u8)
}

const SIZE: u64 = 1024 * 1024; // 4x the streaming threshold; the test image is small
const PATH: &[u8] = b"/home/thos/stream.bin\0";

fn main() {
    let mut bad: Vec<String> = Vec::new();

    // write
    let t0 = now_ms();
    let fd = unsafe { sys(2, PATH.as_ptr() as u64, 0o1101, 0o644) }; // O_WRONLY|O_CREAT|O_TRUNC
    if fd < 0 {
        println!("stream FAIL: create = {fd}");
        return;
    }
    let mut buf = vec![0u8; 4096];
    let mut off = 0u64;
    while off < SIZE {
        for (i, b) in buf.iter_mut().enumerate() { *b = byte_at(off + i as u64); }
        let n = unsafe { sys(1, fd as u64, buf.as_ptr() as u64, 4096) };
        if n != 4096 { bad.push(format!("write at {off} = {n}")); break; }
        off += 4096;
    }
    let fs = unsafe { sys(74, fd as u64, 0, 0) }; // fsync
    if fs != 0 { bad.push(format!("fsync = {fs}")); }
    unsafe { sys(3, fd as u64, 0, 0) };
    let wr_ms = now_ms() - t0;

    // read back sequentially with an awkward size
    let t1 = now_ms();
    let fd = unsafe { sys(2, PATH.as_ptr() as u64, 0, 0) };
    if fd < 0 { println!("stream FAIL: reopen = {fd}"); return; }
    let mut st = [0u8; 144];
    unsafe { sys(5, fd as u64, st.as_mut_ptr() as u64, 0) };
    let mut size = 0u64;
    for i in 0..8 { size |= (st[48 + i] as u64) << (8 * i); }
    if size != SIZE { bad.push(format!("fstat size {size}")); }
    let mut rbuf = vec![0u8; 1000];
    let mut pos = 0u64;
    loop {
        let n = unsafe { sys(0, fd as u64, rbuf.as_mut_ptr() as u64, 1000) };
        if n < 0 { bad.push(format!("read = {n}")); break; }
        if n == 0 { break; }
        for i in 0..n as usize {
            if rbuf[i] != byte_at(pos + i as u64) { bad.push(format!("byte {} wrong", pos + i as u64)); pos = SIZE; break; }
        }
        pos += n as u64;
        if pos >= SIZE { break; }
    }
    if pos != SIZE && bad.is_empty() { bad.push(format!("sequential read ended at {pos}")); }

    // random seeks
    let mut x = 12345u64;
    for _ in 0..200 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let o = (x >> 33) % (SIZE - 300);
        let s = unsafe { sys(8, fd as u64, o, 0) };
        let mut b = [0u8; 300];
        let n = unsafe { sys(0, fd as u64, b.as_mut_ptr() as u64, 300) };
        if s != o as i64 || n != 300 { bad.push(format!("seek {o} -> {s}, read {n}")); break; }
        if (0..300).any(|i| b[i] != byte_at(o + i as u64)) { bad.push(format!("wrong data after seek to {o}")); break; }
    }
    // EOF
    unsafe { sys(8, fd as u64, SIZE, 0) };
    let mut one = [0u8; 8];
    let n = unsafe { sys(0, fd as u64, one.as_mut_ptr() as u64, 8) };
    if n != 0 { bad.push(format!("read at EOF = {n}")); }
    unsafe { sys(3, fd as u64, 0, 0) };
    let rd_ms = now_ms() - t1;

    // In-place update and append (the incremental ext2 path): overwrite 300 bytes in the middle
    // of the file, then grow it by 5000 bytes at the end.
    let mid = 0x40000u64 + 17;
    let fd = unsafe { sys(2, PATH.as_ptr() as u64, 1, 0) }; // O_WRONLY, no truncate
    if fd < 0 {
        bad.push(format!("open for update = {fd}"));
    } else {
        let patch = [0xA5u8; 300];
        unsafe { sys(8, fd as u64, mid, 0) };
        let n = unsafe { sys(1, fd as u64, patch.as_ptr() as u64, 300) };
        if n != 300 { bad.push(format!("overwrite = {n}")); }
        unsafe { sys(8, fd as u64, 0, 2) }; // SEEK_END
        let mut tail = vec![0u8; 5000];
        for (i, b) in tail.iter_mut().enumerate() { *b = byte_at(SIZE + i as u64); }
        let n = unsafe { sys(1, fd as u64, tail.as_ptr() as u64, 5000) };
        if n != 5000 { bad.push(format!("append = {n}")); }
        let fs = unsafe { sys(74, fd as u64, 0, 0) };
        if fs != 0 { bad.push(format!("fsync after update = {fs}")); }
        unsafe { sys(3, fd as u64, 0, 0) };
    }
    let fd = unsafe { sys(2, PATH.as_ptr() as u64, 0, 0) };
    if fd >= 0 {
        let mut st = [0u8; 144];
        unsafe { sys(5, fd as u64, st.as_mut_ptr() as u64, 0) };
        let mut size2 = 0u64;
        for i in 0..8 { size2 |= (st[48 + i] as u64) << (8 * i); }
        if size2 != SIZE + 5000 { bad.push(format!("size after append {size2}")); }
        let mut b = [0u8; 400];
        unsafe { sys(8, fd as u64, mid - 50, 0) };
        let n = unsafe { sys(0, fd as u64, b.as_mut_ptr() as u64, 400) };
        if n != 400 { bad.push(format!("read around the patch = {n}")); }
        for i in 0..400usize {
            let off = mid - 50 + i as u64;
            let want = if (mid..mid + 300).contains(&off) { 0xA5 } else { byte_at(off) };
            if b[i] != want { bad.push(format!("byte {off} after overwrite: {} want {}", b[i], want)); break; }
        }
        let mut tail = vec![0u8; 5000];
        unsafe { sys(8, fd as u64, SIZE, 0) };
        let n = unsafe { sys(0, fd as u64, tail.as_mut_ptr() as u64, 5000) };
        if n != 5000 || (0..5000usize).any(|i| tail[i] != byte_at(SIZE + i as u64)) { bad.push(format!("appended tail wrong (read {n})")); }
        unsafe { sys(3, fd as u64, 0, 0) };
    } else { bad.push(format!("reopen after update = {fd}")); }
    unsafe { sys(87, PATH.as_ptr() as u64, 0, 0) }; // unlink

    if bad.is_empty() {
        println!("stream ok: {SIZE} bytes, write {wr_ms} ms, read+seek {rd_ms} ms, in-place update + append");
    } else {
        for b in &bad { println!("stream FAIL: {b}"); }
    }
}
