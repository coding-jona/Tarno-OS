// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 3 — real windows on the ring-3 callback mechanism ([`crate::nt`]'s
//! `invoke_ring3_callback`, built for `CallWindowProcA`) + the GDI32/User32
//! skeleton ([`crate::gdi`]).
//!
//! A window is bookkeeping (class → `WndProc`, an owning thread, a rect) plus
//! a real per-thread message queue. `GetMessageA` blocks on it for real;
//! `DispatchMessageA` and `UpdateWindow` call the target `WndProc` in ring 3
//! through `invoke_ring3_callback` and get its `LRESULT` back, exactly like
//! `CallWindowProcA`. Still no compositor: a window's rect is recorded but
//! nothing clips or offsets GDI drawing into it yet (`gdi.rs`'s DC is still
//! the whole screen) — that's the next increment.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use spin::Mutex;

use crate::wait::WaitQueue;

pub const WM_CREATE: u32 = 0x0001;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_QUIT: u32 = 0x0012;

/// One registered window class: just its `WndProc`, the only thing
/// `CreateWindowExA` needs from `RegisterClassA`'s `WNDCLASSA`.
static CLASSES: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

pub struct Window {
    pub wndproc: u64,
    pub owner_tid: u64,
    /// (x, y, width, height) — recorded for the next increment (offsetting
    /// `gdi.rs`'s drawing into the window's client area); nothing reads it
    /// yet.
    #[allow(dead_code)]
    pub rect: (i32, i32, i32, i32),
}

static WINDOWS: Mutex<BTreeMap<u32, Window>> = Mutex::new(BTreeMap::new());
static NEXT_HWND: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy)]
pub struct Msg {
    pub hwnd: u32,
    pub message: u32,
    pub wparam: u64,
    pub lparam: u64,
}

/// Real per-thread message queues (Win32: every thread that creates a window
/// gets its own). Keyed by tid, same pattern as `process::THREAD_EXITS` /
/// `CALLBACK_FRAMES`.
static QUEUES: Mutex<BTreeMap<u64, VecDeque<Msg>>> = Mutex::new(BTreeMap::new());
/// Woken on every post; `GetMessageA` blocks on it, re-checking its own
/// thread's queue (one queue can be empty while another just got a message).
static QUEUE_WQ: WaitQueue = WaitQueue::new();

/// `RegisterClassA`: remember `name`'s `WndProc`. Real `RegisterClassA`
/// returns a 16-bit ATOM identifying the class; THOS doesn't need one
/// (`CreateWindowExA` looks classes up by name again), so callers just check
/// for non-zero success.
pub fn register_class(name: String, wndproc: u64) {
    CLASSES.lock().insert(name, wndproc);
}

/// `CreateWindowExA`: `0` if `class` was never registered.
pub fn create_window(class: &str, x: i32, y: i32, w: i32, h: i32, owner_tid: u64) -> u32 {
    let Some(&wndproc) = CLASSES.lock().get(class) else { return 0 };
    let hwnd = NEXT_HWND.fetch_add(1, Ordering::Relaxed);
    WINDOWS.lock().insert(hwnd, Window { wndproc, owner_tid, rect: (x, y, w, h) });
    post(hwnd, owner_tid, WM_CREATE, 0, 0);
    hwnd
}

pub fn wndproc_of(hwnd: u32) -> Option<u64> {
    WINDOWS.lock().get(&hwnd).map(|w| w.wndproc)
}

fn owner_of(hwnd: u32) -> Option<u64> {
    WINDOWS.lock().get(&hwnd).map(|w| w.owner_tid)
}

/// Queue `message` for `tid` directly — used for the window-creation
/// `WM_CREATE` (owner already known) and by `post_quit` (always the calling
/// thread's own queue).
pub fn post(hwnd: u32, tid: u64, message: u32, wparam: u64, lparam: u64) {
    QUEUES.lock().entry(tid).or_default().push_back(Msg { hwnd, message, wparam, lparam });
    QUEUE_WQ.wake_all();
}

/// `PostMessageA(hwnd, ...)`: find `hwnd`'s owning thread and queue there.
/// `false` if `hwnd` doesn't name a live window (real `PostMessageA` fails
/// the same way on a bad `HWND`).
pub fn post_message(hwnd: u32, message: u32, wparam: u64, lparam: u64) -> bool {
    match owner_of(hwnd) {
        Some(tid) => {
            post(hwnd, tid, message, wparam, lparam);
            true
        }
        None => false,
    }
}

/// `PostQuitMessage(nExitCode)`: always targets the calling thread's own
/// queue, `hwnd` `0` (real `WM_QUIT` isn't associated with any window).
pub fn post_quit(tid: u64, exit_code: u64) {
    post(0, tid, WM_QUIT, exit_code, 0);
}

/// `GetMessageA`: block until `tid`'s queue has a message, then pop it.
pub fn get_message(tid: u64) -> Msg {
    QUEUE_WQ.wait_if(|| QUEUES.lock().get(&tid).map_or(true, VecDeque::is_empty));
    QUEUES.lock().get_mut(&tid).and_then(VecDeque::pop_front).unwrap_or(Msg {
        hwnd: 0,
        message: WM_QUIT,
        wparam: 0,
        lparam: 0,
    })
}
