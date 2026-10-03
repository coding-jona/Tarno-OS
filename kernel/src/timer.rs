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

/// Register `t` to be `sched::unblock`ed once the clock reaches `deadline`.
/// Pairs with [`disarm`]. Used both by [`sleep_until`] and by a dual-enqueued
/// timed object wait (`wait::WaitQueue::wait_if_until`), which arms the wheel
/// *and* enqueues on the object so whichever fires first wakes the thread.
pub fn arm(deadline: u64, t: Arc<Thread>) {
    WHEEL.lock().push((deadline, t));
}

/// Remove `t`'s wheel entry if it is still pending (it was woken by something
/// else first, or the wait is over). Safe to call with no entry present.
pub fn disarm(t: &Arc<Thread>) {
    let mut wheel = WHEEL.lock();
    if let Some(p) = wheel.iter().position(|(_, x)| Arc::ptr_eq(x, t)) {
        wheel.swap_remove(p);
    }
}

/// Block the current thread until tick `deadline` (or until something else wakes
/// it — the caller re-checks its own condition). Safe from a PE syscall.
/// The timer tick `ns` nanoseconds from now, rounded up and never early (one extra tick).
pub fn deadline_after_ns(ns: u64) -> u64 {
    now().saturating_add(ns.div_ceil(NS_PER_TICK).max(1) + 1)
}

pub fn sleep_until(deadline: u64) {
    if now() >= deadline {
        return;
    }
    let me = sched::current();
    sched::mark_blocking(&me);
    if crate::signal::interrupted() {
        return; // a signal ends the sleep (`sleep_ns` reports the rest)
    }
    arm(deadline, me.clone());
    sched::block_current();
    disarm(&me); // woken (deadline or an unrelated wake) — drop any stale entry
}

// ===========================================================================
//  Wall-clock and monotonic time (nanosecond resolution, TSC-based)
// ===========================================================================

/// TSC ticks per millisecond, measured against the PIT at boot (`apic`).
static TSC_PER_MS: AtomicU64 = AtomicU64::new(0);
/// TSC value when the clock was started.
static TSC_BASE: AtomicU64 = AtomicU64::new(0);
/// Unix time (ns) corresponding to `monotonic_ns() == 0`.
static BOOT_UNIX_NS: AtomicU64 = AtomicU64::new(0);
/// Last value handed out, so the clock never runs backwards even if two CPUs'
/// TSCs are slightly out of step.
static LAST_NS: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Start the nanosecond clock: `tsc_per_ms` is the calibration, `unix_secs` the
/// RTC reading at this instant (or `None`, then wall-clock time starts at 0).
pub fn start_clock(tsc_per_ms: u64, unix_secs: Option<u64>) {
    TSC_PER_MS.store(tsc_per_ms.max(1), Ordering::Relaxed);
    TSC_BASE.store(rdtsc(), Ordering::Relaxed);
    BOOT_UNIX_NS.store(unix_secs.unwrap_or(0).saturating_mul(1_000_000_000), Ordering::Relaxed);
}

/// Nanoseconds since the clock started. Falls back to the 100 Hz tick count if
/// the TSC was never calibrated.
pub fn monotonic_ns() -> u64 {
    let per_ms = TSC_PER_MS.load(Ordering::Relaxed);
    let ns = if per_ms == 0 {
        now() * NS_PER_TICK
    } else {
        let d = rdtsc().saturating_sub(TSC_BASE.load(Ordering::Relaxed));
        ((d as u128 * 1_000_000) / per_ms as u128) as u64
    };
    LAST_NS.fetch_max(ns, Ordering::Relaxed).max(ns)
}

/// Wall-clock time, nanoseconds since the Unix epoch (UTC).
pub fn realtime_ns() -> u64 {
    BOOT_UNIX_NS.load(Ordering::Relaxed).saturating_add(monotonic_ns())
}

/// Wall-clock time in whole seconds.
pub fn unix_secs() -> u64 {
    realtime_ns() / 1_000_000_000
}

/// Block the caller for at least `ns` nanoseconds (rounded up to whole 10 ms
/// ticks, minimum one tick for any non-zero request).
/// Sleep `ns` nanoseconds; returns the nanoseconds left if a signal cut it short.
pub fn sleep_ns(ns: u64) -> u64 {
    if ns == 0 {
        return 0;
    }
    // +1: `now()` is somewhere inside the current tick, so without it the sleep could end
    // up to a whole tick short of what was asked (POSIX: at least the requested time).
    let ticks = ns.div_ceil(NS_PER_TICK).max(1) + 1;
    let deadline = now().saturating_add(ticks);
    while now() < deadline {
        if crate::signal::interrupted() {
            return (deadline - now()).saturating_mul(NS_PER_TICK).min(ns);
        }
        sleep_until(deadline);
    }
    0
}
