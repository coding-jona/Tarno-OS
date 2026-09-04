// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 1 — the one wait/sync primitive.
//!
//! `WaitQueue` is the single blocking primitive in the Executive core. Every
//! higher-level synchronisation object is built on it: `Event` here, and later
//! POSIX `futex`, an NT dispatcher object (`KEVENT`), a semaphore, a pipe's
//! not-empty condition — all the same queue of blocked [`sched::Thread`]s.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

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
