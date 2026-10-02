// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — a line-disciplined console.
//!
//! The xHCI keyboard thread feeds 8-byte HID boot reports here; this decodes
//! them (with a US layout + shift), echoes to the serial console, does minimal
//! line editing (backspace), and queues bytes for `read` on fd 0.
//!
//! Also the **secure attention key** — `elevate()`'s trusted path (see
//! `syscall::sys_elevate`'s own doc comment for the gap this closes):
//! Ctrl+Alt+Delete is detected directly off the raw HID report, *before*
//! any byte reaches [`TTY`] — the only thing a `read()` on fd 0 (i.e. any
//! user-mode process) can ever see. No app can draw a fake password prompt
//! here, because no app-visible input stream ever carries these keystrokes
//! at all; they're consumed entirely inside this module and never queued.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use spin::Mutex;

use crate::serial;
use crate::wait::WaitQueue;

/// The terminal input buffer, in canonical (line-buffered) mode.
///
/// `q` holds every typed byte not yet read; the last `pending` of them are the
/// line still being edited — **not readable yet**. Enter (or Ctrl+D) commits the
/// line; until then Backspace / Ctrl+U / Ctrl+W can still take characters back,
/// which is the whole point of a line discipline (the shell never sees what the
/// user erased). One lock covers both fields so a reader can't observe them torn.
struct Tty {
    q: VecDeque<u8>,
    pending: usize,
}

impl Tty {
    const fn new() -> Self {
        Self { q: VecDeque::new(), pending: 0 }
    }
    /// Append a typed byte; a newline commits the whole line.
    fn push(&mut self, c: u8) {
        self.q.push_back(c);
        if c == b'\n' {
            self.pending = 0;
        } else {
            self.pending += 1;
        }
    }
    /// Take back the last typed byte of the current line.
    fn erase_last(&mut self) -> bool {
        if self.pending == 0 {
            return false;
        }
        self.q.pop_back();
        self.pending -= 1;
        true
    }
    /// Drop the whole current line; returns how many bytes.
    fn kill_line(&mut self) -> usize {
        let n = self.pending;
        for _ in 0..n {
            self.q.pop_back();
        }
        self.pending = 0;
        n
    }
    /// Drop the last word (and blanks before it) of the current line.
    fn kill_word(&mut self) -> usize {
        let mut n = 0;
        while n < self.pending && self.q.iter().rev().nth(n) == Some(&b' ') {
            n += 1;
        }
        while n < self.pending && self.q.iter().rev().nth(n).is_some_and(|&b| b != b' ') {
            n += 1;
        }
        for _ in 0..n {
            self.q.pop_back();
        }
        self.pending -= n;
        n
    }
    /// Bytes a reader may take now.
    fn committed(&self) -> usize {
        self.q.len() - self.pending
    }
}

static TTY: Mutex<Tty> = Mutex::new(Tty::new());
/// Woken when a line is committed — a blocked `read` on fd 0 sleeps on it.
static INPUT_WQ: WaitQueue = WaitQueue::new();
static PREV: Mutex<[u8; 6]> = Mutex::new([0; 6]);

/// `true` while a secure-attention sequence is being typed: every key goes
/// to [`SAK_BUF`] instead of [`TTY`], masked, until Enter.
/// Set by Ctrl+D on an empty line: the next `read` on fd 0 reports EOF (0) once.
static EOF: AtomicBool = AtomicBool::new(false);
/// The clipboard: filled by `Ctrl+Shift+C` (copy selection), drained by
/// `Ctrl+Shift+V` (paste as typed input).
static CLIPBOARD: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Longest paste accepted in one go.
const PASTE_MAX: usize = 4096;

static SAK_ACTIVE: AtomicBool = AtomicBool::new(false);
static SAK_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Echo mode: 0 = echo the character, 1 = echo `*` (password entry),
/// 2 = echo nothing.
static ECHO: AtomicU8 = AtomicU8::new(0);

#[allow(dead_code)] // used by the `interactive` login flow
pub const ECHO_NORMAL: u8 = 0;
#[allow(dead_code)]
pub const ECHO_MASKED: u8 = 1;

/// Set how typed characters are echoed to the serial console.
#[allow(dead_code)]
pub fn set_echo(mode: u8) {
    ECHO.store(mode, Ordering::Relaxed);
}

/// HID Usage ID → ASCII for a **German (QWERTZ, ISO)** layout.
///
/// `(plain, shifted, altgr)`. 0 = ignore. Only the ASCII-representable keys are
/// mapped: umlauts (ä/ö/ü/ß), the dead keys (´`^) and €/µ/² are left at 0 until
/// the console feeds UTF-8 — a real TTY is a later milestone. The shell-relevant
/// AltGr symbols are here: `@ \ | ~ { } [ ]`.
fn ascii(code: u8, shift: bool, altgr: bool) -> u8 {
    let (lo, hi, ag): (u8, u8, u8) = match code {
        // letters — physical Y/Z swapped vs. US (QWERTZ)
        0x1C => (b'z', b'Z', 0),
        0x1D => (b'y', b'Y', 0),
        0x14 => (b'q', b'Q', b'@'), // AltGr+Q = @
        0x04..=0x1D => {
            let c = b'a' + (code - 0x04);
            (c, c - 32, 0)
        }
        // number row
        0x1E => (b'1', b'!', 0),
        0x1F => (b'2', b'"', 0),
        0x20 => (b'3', 0, 0), // shift = §  (non-ASCII)
        0x21 => (b'4', b'$', 0),
        0x22 => (b'5', b'%', 0),
        0x23 => (b'6', b'&', 0),
        0x24 => (b'7', b'/', b'{'), // AltGr+7 = {
        0x25 => (b'8', b'(', b'['), // AltGr+8 = [
        0x26 => (b'9', b')', b']'), // AltGr+9 = ]
        0x27 => (b'0', b'=', b'}'), // AltGr+0 = }
        0x2D => (0, b'?', b'\\'),   // ß key: AltGr = backslash
        0x2E => (0, 0, 0),          // ´ ` dead keys
        // control
        0x28 => (b'\n', b'\n', 0),
        0x2A => (0x08, 0x08, 0), // backspace
        0x2B => (b'\t', b'\t', 0),
        0x2C => (b' ', b' ', 0),
        // right-hand cluster
        0x2F => (0, 0, 0),         // ü / Ü
        0x30 => (b'+', b'*', b'~'), // AltGr = ~
        0x31 => (b'#', b'\'', 0),
        0x33 => (0, 0, 0),         // ö / Ö
        0x34 => (0, 0, 0),         // ä / Ä
        0x35 => (0, 0, 0),         // ^ ° dead key
        0x36 => (b',', b';', 0),
        0x37 => (b'.', b':', 0),
        0x38 => (b'-', b'_', 0),    // German "-" lives on the US "/?" key
        0x64 => (b'<', b'>', b'|'), // ISO key left of Y: AltGr = |
        _ => return 0,
    };
    if altgr {
        ag
    } else if shift {
        hi
    } else {
        lo
    }
}

/// HID Usage ID for the Delete key.
const KC_DELETE: u8 = 0x4C;

// Non-character keys the shortcuts use (HID usage IDs).
const KC_A: u8 = 0x04;
const KC_C: u8 = 0x06;
const KC_BACKSLASH: u8 = 0x31;
const KC_D: u8 = 0x07;
const KC_L: u8 = 0x0F;
const KC_U: u8 = 0x18;
const KC_V: u8 = 0x19;
const KC_W: u8 = 0x1A;
const KC_ENTER: u8 = 0x28;
const KC_ESC: u8 = 0x29;
const KC_SPACE: u8 = 0x2C;
const KC_PGUP: u8 = 0x4B;
const KC_PGDN: u8 = 0x4E;
const KC_HOME: u8 = 0x4A;
const KC_END: u8 = 0x4D;
const KC_RIGHT: u8 = 0x4F;
const KC_LEFT: u8 = 0x50;
const KC_DOWN: u8 = 0x51;
const KC_UP: u8 = 0x52;

/// Queue one typed character exactly as a key press would: into the line being
/// edited (a newline commits it), and echoed (honouring the echo mode).
fn push_typed(c: u8) {
    TTY.lock().push(c);
    match ECHO.load(Ordering::Relaxed) {
        1 if c != b'\n' => serial::write_bytes(b"*"),
        2 => {}
        _ => serial::write_bytes(&[c]),
    }
}

/// Erase `n` just-typed characters from the screen.
fn echo_erase(n: usize) {
    if ECHO.load(Ordering::Relaxed) != 2 {
        for _ in 0..n {
            serial::write_bytes(b"\x08 \x08");
        }
    }
}

/// Ctrl+U: discard the whole line being edited.
fn kill_line() {
    let n = TTY.lock().kill_line();
    echo_erase(n);
}

/// Ctrl+W: discard the last word (and the blanks before it) of the current line.
fn kill_word() {
    let n = TTY.lock().kill_word();
    echo_erase(n);
}

/// Ctrl+Shift+C / Enter in mark mode: copy the selection, then drop it.
fn copy_selection_to_clipboard() {
    if let Some(t) = crate::fbcon::copy_selection() {
        *CLIPBOARD.lock() = t;
    }
    crate::fbcon::clear_selection();
}

/// Ctrl+Shift+V: type the clipboard in. Anything that could act on its own is
/// defused first — control characters (ESC in particular) are dropped and
/// newlines become blanks, so a paste can never *execute* a command; the user
/// reviews the line and presses Enter.
fn paste() {
    let text = CLIPBOARD.lock().clone();
    for &b in text.iter().take(PASTE_MAX) {
        match b {
            b' '..=b'~' => push_typed(b),
            b'\n' | b'\t' => push_typed(b' '),
            _ => {}
        }
    }
    INPUT_WQ.wake_all();
}

/// Keyboard-driven mark mode: arrows move the selection end, Enter or
/// Ctrl+Shift+C copies, Esc cancels. Consumes every key while active.
fn mark_key(k: u8, ctrl: bool, shift: bool) {
    use crate::fbcon::*;
    match k {
        KC_LEFT => mark_move(-1, 0),
        KC_RIGHT => mark_move(1, 0),
        KC_UP => mark_move(0, -1),
        KC_DOWN => mark_move(0, 1),
        KC_PGUP => mark_move(0, -page_rows()),
        KC_PGDN => mark_move(0, page_rows()),
        KC_HOME => mark_line_edge(true),
        KC_END => mark_line_edge(false),
        KC_ENTER => copy_selection_to_clipboard(),
        KC_C if ctrl && shift => copy_selection_to_clipboard(),
        KC_A if ctrl && shift => select_all(),
        KC_ESC => clear_selection(),
        _ => {}
    }
}

/// What a key press with Ctrl and/or Shift held does. Returns `true` if the key
/// was a shortcut (consumed), `false` if it should be typed normally. A Ctrl
/// chord that is *not* a known shortcut is also consumed — Ctrl+X must never
/// type an `x`.
fn shortcut(k: u8, ctrl: bool, shift: bool) -> bool {
    use crate::fbcon::*;
    if mark_active() {
        mark_key(k, ctrl, shift);
        return true;
    }
    match (ctrl, shift, k) {
        // --- copy / paste / select (terminal convention: Ctrl+Shift+...) ---
        (true, true, KC_A) => select_all(),
        (true, true, KC_C) => copy_selection_to_clipboard(),
        (true, true, KC_V) => paste(),
        (true, true, KC_SPACE) => mark_begin(),

        // --- scrollback (Shift+...) ---
        (false, true, KC_PGUP) => scroll_view(page_rows()),
        (false, true, KC_PGDN) => scroll_view(-page_rows()),
        (false, true, KC_HOME) => scroll_to(true),
        (false, true, KC_END) => scroll_to(false),
        (false, true, KC_UP) => scroll_view(1),
        (false, true, KC_DOWN) => scroll_view(-1),

        // --- line editing / terminal control (Ctrl+...) ---
        (true, false, KC_U) => kill_line(),
        (true, false, KC_W) => kill_word(),
        (true, false, KC_L) => clear_screen(),
        // Ctrl+D: on an empty line it is end-of-file; on a half-typed line it
        // hands that line to the reader without a newline (POSIX canonical mode).
        (true, false, KC_D) => {
            let mut t = TTY.lock();
            if t.pending == 0 {
                EOF.store(true, Ordering::Release);
            } else {
                t.pending = 0;
            }
            drop(t);
            INPUT_WQ.wake_all();
        }
        // Ctrl+C / Ctrl+\ (ISIG): drop the half-typed line, echo ^C, and send
        // SIGINT / SIGQUIT to the foreground process group.
        (true, false, KC_C) | (true, false, KC_BACKSLASH) => {
            kill_line();
            let (name, sig): (&[u8], u32) = if k == KC_C {
                (b"^C\r\n", crate::signal::SIGINT)
            } else {
                (b"^\\\r\n", crate::signal::SIGQUIT)
            };
            serial::write_bytes(name);
            crate::signal::send_pgrp(crate::signal::FG_PGRP.load(Ordering::Relaxed), sig);
            INPUT_WQ.wake_all();
        }
        (true, _, _) => {} // any other Ctrl chord: swallow
        _ => return false,
    }
    true
}

/// Feed one HID boot keyboard report (`[modifiers, reserved, k0..k5]`).
pub fn feed_report(rpt: &[u8; 8]) {
    crate::random::add_event(u64::from_le_bytes(*rpt)); // keystroke timing is entropy
    let shift = rpt[0] & 0b0010_0010 != 0; // L/R Shift
    let altgr = rpt[0] & 0b0100_0000 != 0; // Right Alt (AltGr)
    let ctrl = rpt[0] & 0b0001_0001 != 0; // L/R Ctrl
    let alt = rpt[0] & 0b0100_0100 != 0; // L/R Alt
    let keys = [rpt[2], rpt[3], rpt[4], rpt[5], rpt[6], rpt[7]];
    let mut prev = PREV.lock();

    // Secure attention key: checked *before* anything else in this
    // function ever runs, on the raw report — the whole point is that
    // these keystrokes never become a byte any process could read.
    if !SAK_ACTIVE.load(Ordering::Relaxed)
        && ctrl
        && alt
        && keys.contains(&KC_DELETE)
        && !prev.contains(&KC_DELETE)
    {
        *prev = keys;
        drop(prev);
        sak_begin();
        return;
    }
    if SAK_ACTIVE.load(Ordering::Relaxed) {
        sak_feed(&keys, &mut prev, shift, altgr);
        return;
    }

    let mut pushed = false;

    let ctrl_only = ctrl && !altgr; // AltGr is reported as Ctrl+Alt on some keyboards
    for &k in &keys {
        if k == 0 || prev.contains(&k) {
            continue; // held or empty — only act on new key-down
        }
        // Keys that never type a character: handled while mark mode is on, or
        // by a Ctrl/Shift chord. Plain navigation keys fall through to `ascii`
        // (which ignores them).
        let nav = matches!(k, KC_PGUP | KC_PGDN | KC_HOME | KC_END | KC_UP | KC_DOWN | KC_LEFT | KC_RIGHT);
        if (ctrl_only || (shift && nav) || crate::fbcon::mark_active())
            && shortcut(k, ctrl_only, shift)
        {
            continue;
        }
        let c = ascii(k, shift, altgr);
        if c == 0 {
            continue;
        }
        if c == 0x08 {
            if TTY.lock().erase_last() {
                echo_erase(1);
            }
        } else {
            push_typed(c);
            pushed |= c == b'\n'; // only a committed line can wake a reader
        }
    }
    *prev = keys;
    drop(prev);
    if pushed {
        INPUT_WQ.wake_all();
    }
}

/// Enter SAK mode: freeze normal input delivery and print a banner that
/// only this module could have written — there is no way for a user-mode
/// process to forge it appearing at exactly this moment, since it's
/// printed synchronously from the keyboard interrupt path itself.
fn sak_begin() {
    SAK_ACTIVE.store(true, Ordering::Relaxed);
    SAK_BUF.lock().clear();
    serial::write_bytes(
        b"\r\n-- THOS secure attention (kernel prompt, not an application) --\r\nadmin password: ",
    );
}

/// Decode one HID report's worth of keys while a SAK sequence is being
/// typed: same layout decode as the normal path, but every character is
/// masked and appended to [`SAK_BUF`] instead of [`TTY`] — it never
/// becomes readable by any process, elevated or not.
fn sak_feed(keys: &[u8; 6], prev: &mut [u8; 6], shift: bool, altgr: bool) {
    for &k in keys.iter() {
        if k == 0 || prev.contains(&k) {
            continue;
        }
        let c = ascii(k, shift, altgr);
        if c == 0 {
            continue;
        }
        if c == 0x08 {
            if SAK_BUF.lock().pop().is_some() {
                serial::write_bytes(b"\x08 \x08");
            }
        } else if c == b'\n' {
            serial::write_bytes(b"\r\n");
            *prev = *keys;
            sak_finish();
            return;
        } else {
            SAK_BUF.lock().push(c);
            serial::write_bytes(b"*");
        }
    }
    *prev = *keys;
}

/// Re-authenticate against the real credential store and, on success,
/// spawn a trusted uid-0 process — the one action this first slice of the
/// trusted path offers (real Windows SAK opens a whole secure desktop with
/// several choices; THOS has no GUI here yet, so this is deliberately the
/// smallest real thing "the trusted path leads somewhere privileged" can
/// mean). Not available outside `interactive` builds — there's no
/// credential store to check against without a login flow.
#[cfg(feature = "interactive")]
fn sak_finish() {
    let pw = SAK_BUF.lock().clone();
    SAK_BUF.lock().clear();
    SAK_ACTIVE.store(false, Ordering::Relaxed);
    let ok = crate::ext2::open().ok().and_then(|fs| crate::cred::load(&fs)).is_some_and(|c| {
        core::str::from_utf8(&pw).is_ok_and(|s| c.verify(&c.name, s))
    });
    if !ok {
        serial::write_bytes(b"THOS: SAK denied\r\n");
        return;
    }
    serial::write_bytes(b"THOS: SAK accepted -- spawning a trusted uid-0 process\r\n");
    let Some(fs) = crate::ext2::open().ok() else { return };
    let Some(bytes) = fs.read_path("/elevated-check") else { return };
    let _ = crate::process::spawn_elevated(0, &bytes, &["/elevated-check"], &[], 0, 0);
}

#[cfg(not(feature = "interactive"))]
fn sak_finish() {
    SAK_BUF.lock().clear();
    SAK_ACTIVE.store(false, Ordering::Relaxed);
}

/// Non-blocking read into `buf`; returns bytes moved. Only *committed* input is
/// readable — the line still being typed stays with the line editor.
pub fn read(buf: &mut [u8]) -> usize {
    let mut t = TTY.lock();
    let n = buf.len().min(t.committed());
    for b in buf.iter_mut().take(n) {
        *b = t.q.pop_front().unwrap();
    }
    n
}

/// Block the current thread until a committed line (or an EOF from Ctrl+D) is
/// available for `read`.
pub fn wait_for_input() {
    INPUT_WQ.wait_if_intr(|| TTY.lock().committed() == 0 && !EOF.load(Ordering::Acquire));
}

/// Is a Ctrl+D end-of-file waiting to be read?
pub fn eof_pending() -> bool {
    EOF.load(Ordering::Acquire)
}

/// Consume a pending Ctrl+D EOF, if any.
pub fn take_eof() -> bool {
    EOF.swap(false, Ordering::AcqRel)
}

#[allow(dead_code)] // used by an interactive line-reader
pub fn has_input() -> bool {
    TTY.lock().committed() > 0
}
