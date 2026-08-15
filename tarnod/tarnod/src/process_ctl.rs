//! Signal-Steuerung für vom Security-Subsystem angehaltene Prozesse.
//!
//! Begründung, warum SIGSTOP aus Userspace (nicht aus dem eBPF-Programm)
//! ausgelöst wird: docs/month3-tarno-layer.md#warum-sigstop-aus-userspace.

use std::io;

fn send_signal(pid: i32, sig: i32) -> io::Result<()> {
    let ret = unsafe { libc::kill(pid, sig) };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Hält einen Prozess an (wird vom Security-Subsystem bei einem
/// Policy-Treffer aufgerufen). Wird ab dem `ebpf`-Feature (Stage 3,
/// tarno-guard-ebpf) verdrahtet — im Default-Build ohne dieses Feature
/// bislang nur per Test abgedeckt, daher `allow(dead_code)`.
#[allow(dead_code)]
pub fn stop(pid: i32) -> io::Result<()> {
    send_signal(pid, libc::SIGSTOP)
}

/// Setzt einen zuvor angehaltenen Prozess fort (z.B. via `tarnoctl resume`,
/// nachdem ein Nutzer einen False Positive geprüft hat).
pub fn resume(pid: i32) -> io::Result<()> {
    send_signal(pid, libc::SIGCONT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_on_nonexistent_pid_returns_err() {
        // PID 2_000_000_000 existiert praktisch nie -> ESRCH erwartet.
        let result = resume(2_000_000_000);
        assert!(result.is_err());
    }

    #[test]
    fn stop_and_resume_real_child_process() {
        use std::process::{Command, Stdio};
        use std::{thread, time::Duration};

        let mut child = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep binary must exist for this test");
        let pid = child.id() as i32;

        stop(pid).expect("SIGSTOP should succeed on our own child");
        thread::sleep(Duration::from_millis(50));
        resume(pid).expect("SIGCONT should succeed on our own child");

        child.kill().ok();
        child.wait().ok();
    }
}
