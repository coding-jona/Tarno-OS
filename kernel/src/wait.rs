// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — the one wait/sync primitive.
//!
//! `WaitQueue` is the single blocking primitive in the Executive core. Every
//! higher-level synchronisation object is built on it: `Event` here, and later
//! POSIX `futex`, an NT dispatcher object (`KEVENT`), a semaphore, a pipe's
//! not-empty condition — all the same queue of blocked [`sched::Thread`]s.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::sched::{self, Thread};

pub struct WaitQueue {
    waiters: Mutex<VecDeque<Arc<Thread>>>,
}

#[allow(dead_code)] // full primitive surface; personalities consume the rest
impl WaitQueue {
    pub const fn new() -> Self {
        Self { waiters: Mutex::new(VecDeque::new()) }
    }

    /// Block the current thread on this queue until woken.
    pub fn wait(&self) {
        interrupts::without_interrupts(|| {
            self.waiters.lock().push_back(sched::current());
            sched::block_current();
        });
    }

    /// Condition-variable wait, race-free against a concurrent wake: the queue
    /// lock is held across `should_block()` + the enqueue, and every `wake_*`
    /// takes that same lock, so a wake can't slip between the check and the
    /// sleep. A wake landing after the enqueue but before the actual block is
    /// still safe — the thread just resumes early (callers must re-check their
    /// condition in a loop, as with any condvar).
    pub fn wait_if<F: FnOnce() -> bool>(&self, should_block: F) {
        interrupts::without_interrupts(|| {
            let mut w = self.waiters.lock();
            if !should_block() {
                return;
            }
            w.push_back(sched::current());
            drop(w);
            sched::block_current();
        });
    }

    /// Wake one blocked thread, if any. Returns whether one was woken.
    pub fn wake_one(&self) -> bool {
        match self.waiters.lock().pop_front() {
            Some(t) => {
                sched::unblock(t);
                true
            }
            None => false,
        }
    }

    /// Wake exactly one blocked thread; if none is blocked, run `f` (both under
    /// the queue lock, so it can't race a concurrent [`wait_if`] check).
    /// Returns whether a thread was woken. This is the race-free building block
    /// for an auto-reset event's `signal`.
    pub fn wake_one_or<F: FnOnce()>(&self, f: F) -> bool {
        let mut w = self.waiters.lock();
        match w.pop_front() {
            Some(t) => {
                drop(w);
                sched::unblock(t);
                true
            }
            None => {
                f();
                false
            }
        }
    }

    /// Wake every blocked thread. Returns how many.
    pub fn wake_all(&self) -> usize {
        let drained: VecDeque<Arc<Thread>> = core::mem::take(&mut *self.waiters.lock());
        let n = drained.len();
        for t in drained {
            sched::unblock(t);
        }
        n
    }
}

/// Manual-reset (`Notification`) stays signalled until `reset`; auto-reset
/// (`Synchronization`) releases one waiter per `signal` and clears itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventMode {
    Manual,
    Auto,
}

/// A dispatcher event: `wait` blocks until `signal`. See [`EventMode`].
pub struct Event {
    signaled: AtomicBool,
    mode: EventMode,
    queue: WaitQueue,
}

#[allow(dead_code)]
impl Event {
    pub const fn new() -> Self {
        Self::with_mode(EventMode::Manual)
    }
    pub const fn with_mode(mode: EventMode) -> Self {
        Self { signaled: AtomicBool::new(false), mode, queue: WaitQueue::new() }
    }

    pub fn signal(&self) {
        match self.mode {
            EventMode::Manual => {
                self.signaled.store(true, Ordering::Release);
                self.queue.wake_all();
            }
            // Release one waiter; if none, latch so the next `wait` consumes it.
            EventMode::Auto => {
                self.queue.wake_one_or(|| self.signaled.store(true, Ordering::Release));
            }
        }
    }

    pub fn reset(&self) {
        self.signaled.store(false, Ordering::Release);
    }

    pub fn is_signaled(&self) -> bool {
        self.signaled.load(Ordering::Acquire)
    }

    /// Non-blocking check that consumes the signal for an auto-reset event.
    pub fn try_take(&self) -> bool {
        match self.mode {
            EventMode::Manual => self.signaled.load(Ordering::Acquire),
            EventMode::Auto => self.signaled.swap(false, Ordering::AcqRel),
        }
    }

    pub fn wait(&self) {
        match self.mode {
            EventMode::Manual => {
                while !self.signaled.load(Ordering::Acquire) {
                    self.queue.wait();
                }
            }
            // `wait_if` holds the queue lock across the check + enqueue, and
            // `signal`'s `wake_one_or` takes the same lock — so either we
            // consume the latched flag, or we block and `signal` wakes us
            // (the wakeup *is* the signal; nothing else to check).
            EventMode::Auto => {
                self.queue.wait_if(|| !self.signaled.swap(false, Ordering::AcqRel));
            }
        }
    }
}

/// A counting semaphore (NT `KSEMAPHORE`). Signalled while `count > 0`; each
/// successful `wait` / `try_take` consumes one unit, `release(n)` adds `n`
/// (never past `limit`) and wakes up to `n` waiters.
pub struct Semaphore {
    count: AtomicI32,
    limit: i32,
    queue: WaitQueue,
}

#[allow(dead_code)]
impl Semaphore {
    pub fn new(initial: i32, limit: i32) -> Self {
        Self { count: AtomicI32::new(initial), limit, queue: WaitQueue::new() }
    }

    /// Take one unit without blocking.
    pub fn try_take(&self) -> bool {
        let mut c = self.count.load(Ordering::Acquire);
        loop {
            if c <= 0 {
                return false;
            }
            match self.count.compare_exchange_weak(c, c - 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(n) => c = n,
            }
        }
    }

    pub fn wait(&self) {
        loop {
            if self.try_take() {
                return;
            }
            self.queue.wait_if(|| self.count.load(Ordering::Acquire) <= 0);
        }
    }

    /// Add `n` units and wake up to `n` waiters. `None` if it would exceed
    /// `limit`; otherwise the previous count.
    pub fn release(&self, n: i32) -> Option<i32> {
        let mut c = self.count.load(Ordering::Acquire);
        loop {
            if n <= 0 || c.checked_add(n).map_or(true, |v| v > self.limit) {
                return None;
            }
            match self.count.compare_exchange_weak(c, c + n, Ordering::AcqRel, Ordering::Acquire) {
                Ok(prev) => {
                    for _ in 0..n {
                        if !self.queue.wake_one() {
                            break;
                        }
                    }
                    return Some(prev);
                }
                Err(x) => c = x,
            }
        }
    }

    pub fn is_signaled(&self) -> bool {
        self.count.load(Ordering::Acquire) > 0
    }
}

/// A mutant (NT mutex): recursive, thread-owned. `owner == 0` ⇒ free. Signalled
/// to a thread iff free or already owned by it.
pub struct Mutant {
    owner: AtomicU64,
    recursion: AtomicU32,
    queue: WaitQueue,
}

#[allow(dead_code)]
impl Mutant {
    /// `initial_owner == 0` ⇒ created free; otherwise held by that thread id
    /// with recursion 1.
    pub fn new(initial_owner: u64) -> Self {
        Self {
            owner: AtomicU64::new(initial_owner),
            recursion: AtomicU32::new(if initial_owner != 0 { 1 } else { 0 }),
            queue: WaitQueue::new(),
        }
    }

    pub fn try_acquire(&self, tid: u64) -> bool {
        if self.owner.load(Ordering::Acquire) == tid {
            self.recursion.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self.owner.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            self.recursion.store(1, Ordering::Relaxed);
            return true;
        }
        false
    }

    pub fn acquire(&self, tid: u64) {
        loop {
            if self.try_acquire(tid) {
                return;
            }
            self.queue.wait_if(|| self.owner.load(Ordering::Acquire) != 0);
        }
    }

    /// Give up one recursion level. `Err` if `tid` is not the owner; otherwise
    /// the recursion count *before* this release (1 ⇒ the mutant is now free).
    pub fn release(&self, tid: u64) -> Result<u32, ()> {
        if self.owner.load(Ordering::Acquire) != tid {
            return Err(());
        }
        let prev = self.recursion.load(Ordering::Acquire);
        if prev <= 1 {
            self.recursion.store(0, Ordering::Release);
            self.owner.store(0, Ordering::Release);
            self.queue.wake_one();
        } else {
            self.recursion.store(prev - 1, Ordering::Release);
        }
        Ok(prev)
    }

    pub fn is_signaled(&self, tid: u64) -> bool {
        matches!(self.owner.load(Ordering::Acquire), 0) || self.owner.load(Ordering::Acquire) == tid
    }
}
