// SPDX-License-Identifier: GPL-2.0-or-later
//! A monotonic tick clock + a small timer wheel for real timed blocking.
//!
//! The per-CPU APIC timer fires at ~[`TICK_HZ`]; [`tick`] (from the timer IRQ,
//! CPU 0 only, so the count is not multiplied by the CPU count) advances the
//! clock and moves any thread whose deadline has passed back onto the ready
//! queue. [`sleep_until`] blocks the caller on the executive until its deadline.
//!
//! A PE thread's syscall runs with `IF=0` (cooperative), but [`sleep_until`]
//! does a *clean* [`sched::block_current`] switch — the timer IRQ then fires on
//! whatever runs next with `IF=1` (another thread, or a CPU's `sti;hlt` idle
//! loop) and wakes the sleeper from there.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use crate::sched::{self, Thread};

/// Matches `apic`'s periodic-timer rate.
pub const TICK_HZ: u64 = 100;
const NS_PER_TICK: u64 = 1_000_000_000 / TICK_HZ;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Ticks since boot (≈ 10 ms each).
pub fn now() -> u64 {
    TICKS.load(Ordering::Acquire)
}

/// Deadline tick for a relative NT timeout (`rel` in negative 100 ns units, the
/// only form callers pass here). Rounds up; minimum one tick.
pub fn deadline_from_relative_100ns(rel: i64) -> u64 {
    let ns = rel.unsigned_abs().saturating_mul(100);
    now().saturating_add(ns.div_ceil(NS_PER_TICK).max(1))
}

static WHEEL: Mutex<Vec<(u64, Arc<Thread>)>> = Mutex::new(Vec::new());

/// Advance the clock and wake expired sleepers. IRQ context (IF=0): must not
/// block — `sched::unblock` only pushes to the ready queue.
pub fn tick() {
    // Before the scheduler is up, per-CPU `gs` may not be live — and nothing
    // sleeps yet, so there is nothing to do.
    if !sched::is_started() || crate::smp::this_cpu() != 0 {
        return; // one clock, driven by CPU 0 only
    }
    let now = TICKS.fetch_add(1, Ordering::AcqRel) + 1;
    let mut wheel = WHEEL.lock();
    let mut i = 0;
    while i < wheel.len() {
        if wheel[i].0 <= now {
            let (_, t) = wheel.swap_remove(i);
            sched::unblock(t);
        } else {
            i += 1;
        }
    }
}

/// Block the current thread until tick `deadline` (or until something else wakes
/// it — the caller re-checks its own condition). Safe from a PE syscall.
pub fn sleep_until(deadline: u64) {
    if now() >= deadline {
        return;
    }
    let me = sched::current();
    WHEEL.lock().push((deadline, me.clone()));
    sched::block_current();
    // Woken (deadline reached, or an unrelated wake). Drop any stale entry.
    let mut wheel = WHEEL.lock();
    if let Some(p) = wheel.iter().position(|(_, t)| Arc::ptr_eq(t, &me)) {
        wheel.swap_remove(p);
    }
}
