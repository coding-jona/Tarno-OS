// SPDX-License-Identifier: GPL-2.0-or-later
//! `alarm(2)` and `setitimer(ITIMER_REAL)`: a real-time timer per process that raises
//! `SIGALRM`. One small kernel thread owns all of them and sleeps while none is armed — no
//! cost on the idle machine.

use alloc::vec::Vec;

use spin::Mutex;

use crate::wait::WaitQueue;
use crate::{process, signal, usercopy};

struct Timer {
    pid: u64,
    deadline_ns: u64,
    interval_ns: u64,
}

static TIMERS: Mutex<Vec<Timer>> = Mutex::new(Vec::new());
static WQ: WaitQueue = WaitQueue::new();
const SIGALRM: u32 = 14;

/// Arm (or, with `value_ns == 0`, cancel) the caller's timer. Returns what was left on the
/// previous one: `(remaining_ns, interval_ns)`.
fn set(pid: u64, value_ns: u64, interval_ns: u64) -> (u64, u64) {
    let now = crate::timer::monotonic_ns();
    let mut t = TIMERS.lock();
    let prev = match t.iter().position(|x| x.pid == pid) {
        Some(i) => {
            let old = t.swap_remove(i);
            (old.deadline_ns.saturating_sub(now), old.interval_ns)
        }
        None => (0, 0),
    };
    if value_ns > 0 {
        t.push(Timer { pid, deadline_ns: now.saturating_add(value_ns), interval_ns });
    }
    drop(t);
    WQ.wake_all();
    prev
}

/// `alarm(seconds)`: the seconds that were left on the previous alarm (rounded up).
pub fn sys_alarm(secs: u64) -> i64 {
    let (left, _) = set(process::current_pid(), secs.saturating_mul(1_000_000_000), 0);
    left.div_ceil(1_000_000_000) as i64
}

fn read_timeval(ptr: u64) -> Result<u64, i64> {
    let sec = usercopy::read_u64(ptr)?;
    let usec = usercopy::read_u64(ptr.wrapping_add(8))?;
    if usec >= 1_000_000 {
        return Err(-22);
    }
    Ok(sec.saturating_mul(1_000_000_000).saturating_add(usec * 1000))
}

fn write_itimerval(ptr: u64, value_ns: u64, interval_ns: u64) -> i64 {
    for (off, ns) in [(0u64, interval_ns), (16, value_ns)] {
        if usercopy::write_u64(ptr + off, ns / 1_000_000_000).is_err()
            || usercopy::write_u64(ptr + off + 8, (ns % 1_000_000_000) / 1000).is_err()
        {
            return usercopy::EFAULT;
        }
    }
    0
}

/// `setitimer(which, new, old)` — only `ITIMER_REAL` (0).
pub fn sys_setitimer(which: u64, new: u64, old: u64) -> i64 {
    if which != 0 {
        return -22;
    }
    let (interval, value) = if new == 0 {
        (0, 0)
    } else {
        match (read_timeval(new), read_timeval(new + 16)) {
            (Ok(i), Ok(v)) => (i, v),
            (Err(e), _) | (_, Err(e)) => return e,
        }
    };
    let (left, prev_interval) = set(process::current_pid(), value, interval);
    if old != 0 {
        return write_itimerval(old, left, prev_interval);
    }
    0
}

/// `getitimer(which, cur)`.
pub fn sys_getitimer(which: u64, cur: u64) -> i64 {
    if which != 0 {
        return -22;
    }
    let pid = process::current_pid();
    let now = crate::timer::monotonic_ns();
    let (left, interval) = TIMERS
        .lock()
        .iter()
        .find(|t| t.pid == pid)
        .map_or((0, 0), |t| (t.deadline_ns.saturating_sub(now), t.interval_ns));
    write_itimerval(cur, left, interval)
}

/// The timer thread.
pub extern "C" fn timer_thread(_: usize) -> ! {
    loop {
        WQ.wait_if(|| TIMERS.lock().is_empty());
        let now = crate::timer::monotonic_ns();
        let mut fire: Vec<u64> = Vec::new();
        {
            let mut t = TIMERS.lock();
            t.retain_mut(|x| {
                if x.deadline_ns > now {
                    return true;
                }
                fire.push(x.pid);
                if x.interval_ns > 0 {
                    x.deadline_ns = now + x.interval_ns;
                    true
                } else {
                    false
                }
            });
        }
        for pid in fire {
            if let Some(task) = process::find_task(pid) {
                signal::send(&task, SIGALRM);
            }
        }
        crate::timer::sleep_ns(10_000_000);
    }
}
