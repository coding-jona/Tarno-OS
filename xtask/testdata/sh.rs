// SPDX-License-Identifier: GPL-2.0-or-later
// THOS shell: prompt, read a line off fd 0, split, fork + execve + wait4.
//
// Process ops use raw syscalls — Rust's std::process::Command reaches for
// clone(), which THOS doesn't implement.  Everything else is plain std.

use std::io::{Read, Write};

#[inline]
unsafe fn sc(n: u64, a: u64, b: u64, c: u64, d: u64) -> i64 {
    let r: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") n => r,
        in("rdi") a,
        in("rsi") b,
        in("rdx") c,
        in("r10") d,
        out("rcx") _,
        out("r11") _,
    );
    r
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// fork + execve(path, argv) + wait4; returns the child's exit code.
fn spawn(path: &str, args: &[&str]) -> i32 {
    let p = cstr(path);
    let argbufs: Vec<Vec<u8>> = args.iter().map(|a| cstr(a)).collect();
    let mut argv: Vec<*const u8> = argbufs.iter().map(|b| b.as_ptr()).collect();
    argv.push(std::ptr::null());
    let envp: [*const u8; 1] = [std::ptr::null()];

    let pid = unsafe { sc(57, 0, 0, 0, 0) }; // fork
    if pid == 0 {
        unsafe {
            sc(59, p.as_ptr() as u64, argv.as_ptr() as u64, envp.as_ptr() as u64, 0); // execve
            let m = b"sh: exec failed\n";
            sc(1, 1, m.as_ptr() as u64, m.len() as u64, 0);
            sc(60, 127, 0, 0, 0);
        }
        unreachable!()
    }
    let mut status: i32 = 0;
    unsafe { sc(61, (-1i64) as u64, &mut status as *mut i32 as u64, 0, 0) }; // wait4
    (status >> 8) & 0xff
}

fn main() {
    let mut out = std::io::stdout();
    let mut inp = std::io::stdin();
    let mut buf = [0u8; 256];
    let mut line: Vec<u8> = Vec::new();

    let _ = out.write_all(b"\nTHOS shell -- builtins: exit, help; else runs /<name>\n");
    loop {
        let _ = out.write_all(b"thos$ ");
        let _ = out.flush();

        line.clear();
        loop {
            let n = inp.read(&mut buf).unwrap_or(0);
            if n == 0 {
                continue;
            }
            let mut nl = false;
            for &b in &buf[..n] {
                if b == b'\n' {
                    nl = true;
                    break;
                }
                line.push(b);
            }
            if nl {
                break;
            }
        }

        let s = String::from_utf8_lossy(&line);
        let parts: Vec<&str> = s.split_whitespace().collect();
        match parts.split_first() {
            None => continue,
            Some((&"exit", _)) => std::process::exit(0),
            Some((&"help", _)) => {
                let _ = out.write_all(b"builtins: exit, help\n");
            }
            Some((&cmd, _)) => {
                let path = if cmd.starts_with('/') {
                    cmd.to_string()
                } else {
                    format!("/{cmd}")
                };
                let rc = spawn(&path, &parts);
                if rc != 0 {
                    let _ = writeln!(out, "[exit {rc}]");
                }
            }
        }
    }
}
