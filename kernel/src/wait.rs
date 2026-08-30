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

/// A manually-reset event: `wait` blocks until `signal`, `reset` clears it.
pub struct Event {
    signaled: AtomicBool,
    queue: WaitQueue,
}

#[allow(dead_code)]
impl Event {
    pub const fn new() -> Self {
        Self { signaled: AtomicBool::new(false), queue: WaitQueue::new() }
    }

    pub fn signal(&self) {
        self.signaled.store(true, Ordering::Release);
        self.queue.wake_all();
    }

    pub fn reset(&self) {
        self.signaled.store(false, Ordering::Release);
    }

    pub fn is_signaled(&self) -> bool {
        self.signaled.load(Ordering::Acquire)
    }

    pub fn wait(&self) {
        while !self.signaled.load(Ordering::Acquire) {
            self.queue.wait();
        }
    }
}
