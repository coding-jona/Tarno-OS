// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: a real clock. Wall-clock time must come from the RTC (the host compares the
// printed value with its own clock), the monotonic clock must never go backwards and
// must agree with how long nanosleep really blocked, gettimeofday/time must match
// clock_gettime, and a file written now must carry a modification time of "now".
// Raw syscalls, no libc.
use std::arch::asm;

unsafe fn sys(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> i64 {
    let r: i64;
    asm!("syscall", inlateout("rax") n => r, in("rdi") a, in("rsi") b, in("rdx") c,
         in("r10") d, in("r8") e, out("rcx") _, out("r11") _);
    r
}

fn clock(id: u64) -> u64 {
    let mut ts = [0i64; 2];
    unsafe { sys(228, id, ts.as_mut_ptr() as u64, 0, 0, 0) };
    ts[0] as u64 * 1_000_000_000 + ts[1] as u64
}

fn sleep_ns(ns: u64) {
    let req = [(ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as i64];
    unsafe { sys(35, req.as_ptr() as u64, 0, 0, 0, 0) };
}

fn main() {
    let mut bad: Vec<String> = Vec::new();

    // 1. monotonic never goes backwards (and does advance)
    let first = clock(1);
    let mut prev = first;
    for _ in 0..5000 {
        let t = clock(1);
        if t < prev {
            bad.push(format!("monotonic went backwards: {prev} -> {t}"));
            break;
        }
        prev = t;
    }

    // 2. nanosleep blocks for (at least) what was asked, and not wildly longer
    let t0 = clock(1);
    sleep_ns(1_500_000_000);
    let slept = clock(1) - t0;
    if !(1_450_000_000..2_300_000_000).contains(&slept) {
        bad.push(format!("sleep(1.5s) took {} ms", slept / 1_000_000));
    }
    let t0 = clock(1);
    sleep_ns(50_000_000);
    let short = clock(1) - t0;
    if !(45_000_000..120_000_000).contains(&short) {
        bad.push(format!("sleep(50ms) took {} ms", short / 1_000_000));
    }

    // 3. gettimeofday / time agree with clock_gettime(REALTIME)
    let real = clock(0);
    let mut tv = [0i64; 2];
    unsafe { sys(96, tv.as_mut_ptr() as u64, 0, 0, 0, 0) };
    let tsec = unsafe { sys(201, 0, 0, 0, 0, 0) };
    if (tv[0] - (real / 1_000_000_000) as i64).abs() > 1 {
        bad.push(format!("gettimeofday {} vs clock_gettime {}", tv[0], real / 1_000_000_000));
    }
    if (tsec - (real / 1_000_000_000) as i64).abs() > 1 {
        bad.push(format!("time() {} vs clock_gettime {}", tsec, real / 1_000_000_000));
    }

    // 4. a freshly written file has an mtime of "now"
    let path = b"/home/thos/clocktest.tmp\0";
    let fd = unsafe { sys(2, path.as_ptr() as u64, 0o101, 0o644, 0, 0) }; // O_WRONLY|O_CREAT
    if fd >= 0 {
        unsafe { sys(1, fd as u64, b"x".as_ptr() as u64, 1, 0, 0) };
        unsafe { sys(3, fd as u64, 0, 0, 0, 0) };
        let mut st = [0u8; 144];
        let r = unsafe { sys(262, (-100i64) as u64, path.as_ptr() as u64, st.as_mut_ptr() as u64, 0, 0) };
        let mut mb = [0u8; 8];
        mb.copy_from_slice(&st[88..96]);
        let mtime = i64::from_le_bytes(mb);
        let now = (clock(0) / 1_000_000_000) as i64;
        if r != 0 || (now - mtime).abs() > 15 {
            bad.push(format!("file mtime {mtime} vs now {now} (stat rc {r})"));
        }
        unsafe { sys(87, path.as_ptr() as u64, 0, 0, 0, 0) }; // unlink
    } else {
        bad.push(format!("could not create the test file (open rc {fd})"));
    }

    if bad.is_empty() {
        println!(
            "clock ok: real={} mono-sleep={}ms short={}ms",
            real / 1_000_000_000,
            slept / 1_000_000,
            short / 1_000_000
        );
    } else {
        println!("clock FAIL: {}", bad.join("; "));
    }
}
