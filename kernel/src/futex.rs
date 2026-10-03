// SPDX-License-Identifier: GPL-2.0-or-later
//! `futex(2)`: the kernel half of every userspace lock, condition variable and thread join.
//!
//! A futex is just a 32-bit word in user memory; the kernel only has to park a thread until
//! somebody wakes that address, *unless* the word no longer holds the value the waiter saw
//! (then the wake already happened). The word is identified by its **physical** address, so
//! threads of one process — and, later, processes sharing a mapping — meet at the same queue.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use spin::Mutex;

use crate::wait::WaitQueue;
use crate::{process, sched, signal, usercopy};

const EAGAIN: i64 = -11;
const EINVAL: i64 = -22;
const ETIMEDOUT: i64 = -110;
const EFAULT: i64 = usercopy::EFAULT;

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
const FUTEX_CLOCK_REALTIME: u64 = 256;

/// Wait queues by physical address of the futex word.
static QUEUES: Mutex<BTreeMap<u64, Arc<WaitQueue>>> = Mutex::new(BTreeMap::new());

fn key_of(uaddr: u64) -> Result<u64, i64> {
    if uaddr & 3 != 0 || !usercopy::user_ok(uaddr, 4, false) {
        return Err(EFAULT);
    }
    let space = sched::current().task().ok_or(EINVAL)?.space();
    space.translate(uaddr).ok_or(EFAULT)
}

fn queue(key: u64) -> Arc<WaitQueue> {
    QUEUES.lock().entry(key).or_insert_with(|| Arc::new(WaitQueue::new())).clone()
}

/// Wake up to `n` threads waiting on `uaddr`; the number woken.
pub fn wake(uaddr: u64, n: u64) -> i64 {
    let Ok(key) = key_of(uaddr) else { return EFAULT };
    let q = match QUEUES.lock().get(&key) {
        Some(q) => q.clone(),
        None => return 0,
    };
    let mut woken = 0;
    while woken < n && q.wake_one() {
        woken += 1;
    }
    if q.is_empty() {
        let mut t = QUEUES.lock();
        // Only drop the entry if nobody queued up in the meantime.
        if t.get(&key).is_some_and(|x| Arc::ptr_eq(x, &q)) && q.is_empty() {
            t.remove(&key);
        }
    }
    woken as i64
}

/// Block while `*uaddr == val`, until woken, `deadline` (a timer tick) passes, or a signal.
fn wait(uaddr: u64, val: u32, deadline: Option<u64>) -> i64 {
    let key = match key_of(uaddr) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let q = queue(key);
    let word = uaddr as *const u32;
    // The check runs under the queue lock, so a wake cannot slip in between it and the sleep.
    // The page was validated and is mapped for this process, so the read cannot fault.
    let still_equal = || unsafe { core::ptr::read_volatile(word) } == val && !signal::interrupted();
    if unsafe { core::ptr::read_volatile(word) } != val {
        return EAGAIN;
    }
    let timed_out = match deadline {
        Some(d) => q.wait_if_until(d, still_equal),
        None => {
            q.wait_if(still_equal);
            false
        }
    };
    if timed_out {
        ETIMEDOUT
    } else if signal::interrupted() {
        -4 // EINTR
    } else {
        0
    }
}

/// `futex(uaddr, op, val, timeout, uaddr2, val3)`.
pub fn sys_futex(uaddr: u64, op: u64, val: u64, timeout: u64, _uaddr2: u64, _val3: u64) -> i64 {
    let cmd = op & 0x7f;
    match cmd {
        FUTEX_WAKE | FUTEX_WAKE_BITSET => wake(uaddr, val as u32 as u64),
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let deadline = if timeout == 0 {
                None
            } else {
                let (Ok(sec), Ok(nsec)) = (usercopy::read_u64(timeout), usercopy::read_u64(timeout.wrapping_add(8))) else {
                    return EFAULT;
                };
                if nsec >= 1_000_000_000 {
                    return EINVAL;
                }
                let want = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
                let ns = if cmd == FUTEX_WAIT_BITSET {
                    // absolute: monotonic, or realtime with the flag
                    let now = if op & FUTEX_CLOCK_REALTIME != 0 {
                        crate::timer::realtime_ns()
                    } else {
                        crate::timer::monotonic_ns()
                    };
                    want.saturating_sub(now)
                } else {
                    want
                };
                Some(crate::timer::deadline_after_ns(ns))
            };
            wait(uaddr, val as u32, deadline)
        }
        _ => -38, // ENOSYS: requeue / PI futexes are not needed by glibc's lock paths
    }
}

/// A thread is going away with a `clear_child_tid` address: store 0 there and wake the joiner.
pub fn thread_cleared(addr: u64) {
    if addr == 0 {
        return;
    }
    let _ = usercopy::write_u32(addr, 0);
    wake(addr, 1);
    let _ = process::current_pid();
}
