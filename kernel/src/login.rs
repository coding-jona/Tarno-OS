// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — console first-run setup + login (stub).
//!
//! The ancestor of the later graphical settings overlay: on first boot the
//! operator *must* set the admin name + password here before anything else
//! runs; every later boot asks for them. No default account, no autologin.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::cred::{self, Cred, ADMIN_UID};
use crate::ext2::Ext2;
use crate::{console, kprintln, sched, serial};

/// A resolved session identity. Grows into the executive `Principal` / token.
#[derive(Clone)]
pub struct Session {
    pub name: String,
    pub uid: u32,
}

/// Read one line from the keyboard console. `mask` hides it (password entry).
fn prompt(label: &str, mask: bool) -> String {
    serial::write_bytes(label.as_bytes());
    console::set_echo(if mask { console::ECHO_MASKED } else { console::ECHO_NORMAL });

    let mut line: Vec<u8> = Vec::new();
    let mut b = [0u8; 1];
    loop {
        while console::read(&mut b) == 0 {
            sched::yield_now();
        }
        match b[0] {
            b'\n' | b'\r' => break,
            0x08 | 0x7f => {
                line.pop();
            }
            c => line.push(c),
        }
    }
    console::set_echo(console::ECHO_NORMAL);
    serial::write_bytes(b"\n");
    String::from_utf8_lossy(&line).trim().to_string()
}

/// First boot: force the operator to choose the admin name + password.
pub fn first_run_setup(fs: &Ext2) -> Session {
    kprintln!("");
    kprintln!("  ┌─ THOS first-run setup ─────────────────────────────");
    kprintln!("  │  No account exists yet. Create the administrator.");
    kprintln!("  │  (changeable later via settings + reboot)");
    kprintln!("  └───────────────────────────────────────────────────");

    let name = loop {
        let n = prompt("  admin username: ", false);
        if n.is_empty() || n.len() > 32 || !n.bytes().all(|c| c.is_ascii_graphic()) {
            kprintln!("  ! 1-32 printable ASCII chars");
            continue;
        }
        break n;
    };

    let password = loop {
        let p1 = prompt("  password: ", true);
        if p1.len() < 4 {
            kprintln!("  ! at least 4 characters");
            continue;
        }
        let p2 = prompt("  repeat password: ", true);
        if p1 != p2 {
            kprintln!("  ! passwords do not match");
            continue;
        }
        break p1;
    };

    let c = Cred::create(&name, &password);
    cred::save(fs, &c).expect("write credential store");
    kprintln!("  ✓ administrator '{}' created.\n", name);
    Session { name, uid: ADMIN_UID }
}

/// Every later boot: authenticate against the stored credential.
pub fn login(fs: &Ext2) -> Session {
    let stored = cred::load(fs).expect("credential store unreadable");
    loop {
        let name = prompt("\nTHOS login: ", false);
        let password = prompt("password: ", true);
        if stored.verify(&name, &password) {
            kprintln!("  welcome, {name}.\n");
            return Session { name, uid: ADMIN_UID };
        }
        kprintln!("  login incorrect");
        // token slow-down against guessing (~1s of yielding)
        for _ in 0..2000 {
            sched::yield_now();
        }
    }
}

/// Resolve the session identity: run first-run setup if there is no store,
/// then authenticate.
pub fn establish(fs: &Ext2) -> Session {
    if !cred::exists(fs) {
        first_run_setup(fs);
    }
    login(fs)
}
