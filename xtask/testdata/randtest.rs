// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: getrandom must be a real CSPRNG. Checks 4096 bytes for the obvious
// statistical sanity (about half the bits set, no byte value wildly over/under-
// represented, two consecutive calls differ) and prints the first 16 bytes so the
// host can check that two *boots* produce different streams (the old generator was a
// fixed-seed xorshift: every boot identical). Raw syscalls, no libc.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         out("rcx") _, out("r11") _);
    r
}

fn main() {
    let mut a = [0u8; 4096];
    let mut b = [0u8; 4096];
    let ra = unsafe { sys(318, a.as_mut_ptr() as u64, a.len() as u64, 0) };
    let rb = unsafe { sys(318, b.as_mut_ptr() as u64, b.len() as u64, 0) };
    let mut bad: Vec<String> = Vec::new();
    if ra != 4096 || rb != 4096 {
        bad.push(format!("getrandom returned {ra}/{rb}"));
    }
    let ones: u32 = a.iter().map(|x| x.count_ones()).sum();
    // 32768 bits, p = 0.5: sigma ~ 90; allow ~5 sigma.
    if !(32768 / 2 - 450..=32768 / 2 + 450).contains(&(ones as i64 as usize)) {
        bad.push(format!("{ones} of 32768 bits set"));
    }
    let mut count = [0u32; 256];
    for &x in a.iter() {
        count[x as usize] += 1;
    }
    let (min, max) = (count.iter().min().unwrap(), count.iter().max().unwrap());
    // expected 16 per value; 4096 samples: min >= 1 and max <= 40 holds with overwhelming probability
    if *min < 1 || *max > 40 {
        bad.push(format!("byte counts min {min} max {max}"));
    }
    if a == b {
        bad.push("two calls returned identical data".to_string());
    }
    let hex: String = a[..16].iter().map(|x| format!("{:02x}", x)).collect();
    if bad.is_empty() {
        println!("rand ok first={hex} ones={ones}");
    } else {
        println!("rand FAIL: {} first={hex}", bad.join("; "));
    }
}
