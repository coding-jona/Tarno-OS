// SPDX-License-Identifier: GPL-2.0-or-later
//! THOS build orchestrator.
//!
//!   cargo xtask build            build the kernel ELF
//!   cargo xtask iso              build a bootable BIOS+UEFI ISO (target/thos.iso)
//!   cargo xtask run [--gui]      build the ISO and boot it in QEMU
//!
//! External tools expected on PATH: `xorriso`, `qemu-system-x86_64`, and either
//! a system OVMF firmware (`/usr/share/OVMF/OVMF_CODE.fd`) or `--bios` fallback.
//! Limine is vendored as a git submodule under `third_party/limine` (binary
//! branch); if absent, `iso` prints the exact clone command and exits.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("run");
    let gui = args.iter().any(|a| a == "--gui");

    match cmd {
        "build" => {
            build_kernel(&[]);
        }
        "iso" => {
            build_kernel(&[]);
            build_iso();
        }
        "run" => {
            build_kernel(&[]);
            let iso = build_iso();
            run_qemu(&iso, gui);
        }
        "kbd-test" => {
            build_kernel(&["interactive"]);
            let iso = build_iso();
            kbd_test(&iso);
        }
        "login-test" => {
            build_kernel(&["interactive"]);
            let iso = build_iso();
            login_test(&iso);
        }
        "bootpick" => {
            build_uefi();
        }
        "bootpick-test" => {
            build_uefi();
            bootpick_test();
        }
        "ahci-test" => {
            build_kernel(&[]);
            let iso = build_iso();
            ahci_test(&iso);
        }
        "ext2-test" => {
            build_kernel(&[]);
            let iso = build_iso();
            ext2_test(&iso);
        }
        "smp-test" => {
            build_kernel(&["stress"]);
            let iso = build_iso();
            smp_test(&iso);
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!(
                "usage: cargo xtask [build|iso|run|kbd-test|bootpick|bootpick-test|ahci-test|ext2-test|smp-test] [--gui]"
            );
            exit(2);
        }
    }
}

fn workspace_root() -> PathBuf {
    // xtask lives at <root>/xtask; CARGO_MANIFEST_DIR points there.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("failed to spawn {cmd:?}: {e}");
        exit(1);
    });
    if !status.success() {
        eprintln!("command failed ({status}): {cmd:?}");
        exit(1);
    }
}

fn build_kernel(features: &[&str]) {
    let mut c = Command::new(env!("CARGO"));
    c.current_dir(workspace_root())
        .args(["build", "--package", "thos-kernel", "--release"]);
    if !features.is_empty() {
        c.arg("--features").arg(features.join(","));
    }
    run(&mut c);
}

fn kernel_elf() -> PathBuf {
    workspace_root().join("target/x86_64-unknown-none/release/thos-kernel")
}

fn build_iso() -> PathBuf {
    let root = workspace_root();
    let limine = root.join("third_party/limine");
    if !limine.join("limine").exists() && !limine.join("limine-bios.sys").exists() {
        eprintln!("Limine not vendored. Run:");
        eprintln!("  git submodule update --init third_party/limine");
        eprintln!("  make -C third_party/limine");
        exit(1);
    }

    let iso_root = root.join("target/iso_root");
    let _ = std::fs::remove_dir_all(&iso_root);
    std::fs::create_dir_all(iso_root.join("boot/limine")).unwrap();
    std::fs::create_dir_all(iso_root.join("EFI/BOOT")).unwrap();

    copy(&kernel_elf(), &iso_root.join("boot/thos-kernel"));
    copy(&root.join("boot/limine.conf"), &iso_root.join("boot/limine/limine.conf"));
    for f in ["limine-bios.sys", "limine-bios-cd.bin", "limine-uefi-cd.bin"] {
        copy(&limine.join(f), &iso_root.join("boot/limine").join(f));
    }
    copy(&limine.join("BOOTX64.EFI"), &iso_root.join("EFI/BOOT/BOOTX64.EFI"));

    let iso = root.join("target/thos.iso");
    run(Command::new("xorriso").args([
        "-as", "mkisofs", "-b", "boot/limine/limine-bios-cd.bin",
        "-no-emul-boot", "-boot-load-size", "4", "-boot-info-table",
        "--efi-boot", "boot/limine/limine-uefi-cd.bin",
        "-efi-boot-part", "--efi-boot-image", "--protective-msdos-label",
        iso_root.to_str().unwrap(), "-o", iso.to_str().unwrap(),
    ]));
    run(Command::new(limine.join("limine")).arg("bios-install").arg(&iso));
    iso
}

fn copy(from: &Path, to: &Path) {
    std::fs::copy(from, to).unwrap_or_else(|e| {
        eprintln!("copy {from:?} -> {to:?}: {e}");
        exit(1);
    });
}

/// QEMU exit status when the kernel writes `ExitCode::Success` (0x10) to the
/// `isa-debug-exit` port: `(0x10 << 1) | 1`.
const QEMU_SUCCESS: i32 = 33;

/// An ext2 (1 KiB block) disk image containing the compiled test programs
/// `/init` and `/child`, attached over AHCI. Rebuilt when a source or this
/// xtask changes. Needs `as`, `ld`, `mke2fs`, `debugfs` on PATH.
fn disk_image() -> PathBuf {
    let root = workspace_root();
    let img = root.join("target/disk.img");
    let progs = ["init", "child"];

    let newest_src = std::fs::read_dir(root.join("xtask/testdata"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .chain(root.join("xtask/src/main.rs").metadata().and_then(|m| m.modified()))
        .max();
    let fresh = match (img.metadata().and_then(|m| m.modified()), newest_src) {
        (Ok(i), Some(s)) => i > s,
        _ => false,
    };
    if fresh {
        return img;
    }

    std::fs::create_dir_all(root.join("target")).ok();
    let mut elfs = Vec::new();
    for name in progs {
        let src = root.join(format!("xtask/testdata/{name}.s"));
        let obj = root.join(format!("target/{name}.o"));
        let elf = root.join(format!("target/{name}"));
        run(Command::new("as").args(["-64", "-o", obj.to_str().unwrap(), src.to_str().unwrap()]));
        run(Command::new("ld").args([
            "-static", "-nostdlib", "-Ttext=0x666600000000", "-e", "_start",
            "-o", elf.to_str().unwrap(), obj.to_str().unwrap(),
        ]));
        elfs.push((name, elf));
    }

    let _ = std::fs::remove_file(&img);
    run(Command::new("mke2fs").args([
        "-q", "-F", "-t", "ext2", "-b", "1024", "-I", "128",
        "-O", "^resize_inode,^dir_index,^ext_attr",
        img.to_str().unwrap(), "8192",
    ]));
    // Grow the backing file past the 8 MiB filesystem so there is scratch space
    // for the AHCI write test (LBA 20000) that the fs never touches.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&img)
        .and_then(|f| f.set_len(16 * 1024 * 1024))
        .expect("extend disk.img");
    for (name, elf) in elfs {
        run(Command::new("debugfs").args([
            "-w", "-R", &format!("write {} {name}", elf.to_str().unwrap()),
            img.to_str().unwrap(),
        ]));
    }

    // A plain data file for the open/read/lseek test.
    let msg = root.join("target/message");
    std::fs::write(&msg, b"hello a file read via open+lseek+read\n").unwrap();
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} message", msg.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));

    // A real static-musl Rust binary -> /rusthello.
    let rs = root.join("xtask/testdata/rusthello.rs");
    let rsbin = root.join("target/rusthello");
    run(Command::new("rustc").args([
        "--target", "x86_64-unknown-linux-musl",
        "-C", "relocation-model=static",
        "-C", "link-args=-no-pie",
        "-C", "strip=symbols",
        "-O",
        "-o", rsbin.to_str().unwrap(),
        rs.to_str().unwrap(),
    ]));
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} rusthello", rsbin.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));

    // The THOS shell -> /sh (static-musl Rust, same recipe as rusthello).
    let shsrc = root.join("xtask/testdata/sh.rs");
    let shbin = root.join("target/sh");
    run(Command::new("rustc").args([
        "--target", "x86_64-unknown-linux-musl",
        "-C", "relocation-model=static",
        "-C", "link-args=-no-pie",
        "-C", "strip=symbols",
        "-O",
        "-o", shbin.to_str().unwrap(),
        shsrc.to_str().unwrap(),
    ]));
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} sh", shbin.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));
    img
}

// --- shared plumbing for the interactive (monitor-driven) QEMU tests ---

/// Spawn the interactive kernel with a USB keyboard, serial → file, and the
/// QEMU monitor on a UNIX socket. Returns `(child, serial_log, monitor_sock)`.
fn spawn_interactive_qemu(tag: &str, iso: &Path, disk: &Path) -> (std::process::Child, PathBuf, PathBuf) {
    let root = workspace_root();
    let log = root.join(format!("target/{tag}-serial.log"));
    let sock = root.join(format!("target/{tag}-mon.sock"));
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&sock);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args(["-M", "q35", "-m", "512M", "-smp", "4", "-cdrom", iso.to_str().unwrap()]);
    qemu.args([
        "-drive", &format!("id=disk0,if=none,format=raw,file={}", disk.to_str().unwrap()),
        "-device", "ahci,id=ahci0", "-device", "ide-hd,drive=disk0,bus=ahci0.0",
        "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
    ]);
    qemu.args(["-display", "none", "-no-reboot"]);
    qemu.args(["-serial", &format!("file:{}", log.to_str().unwrap())]);
    qemu.args(["-monitor", &format!("unix:{},server,nowait", sock.to_str().unwrap())]);
    for ovmf in ["/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/ovmf/OVMF.fd"] {
        if Path::new(ovmf).exists() {
            qemu.args(["-drive", &format!("if=pflash,format=raw,readonly=on,file={ovmf}")]);
            break;
        }
    }
    let child = qemu.spawn().expect("spawn qemu");
    (child, log, sock)
}

/// Poll `log` until it contains `needle` or `secs` elapse.
fn wait_for(log: &Path, needle: &str, secs: u64) -> bool {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if std::fs::read_to_string(log).unwrap_or_default().contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn mon(sock: &Path, cmd: &str) {
    use std::io::{Read, Write};
    use std::time::Duration;
    let mut s = std::os::unix::net::UnixStream::connect(sock).expect("connect monitor");
    writeln!(s, "{cmd}").ok();
    let _ = s.set_read_timeout(Some(Duration::from_millis(80)));
    let mut drain = String::new();
    let _ = s.read_to_string(&mut drain);
    std::thread::sleep(Duration::from_millis(110));
}

/// Type `text` (ascii `a-z0-9` only — QEMU `sendkey` names) then Enter.
fn type_line(sock: &Path, text: &str) {
    for c in text.chars() {
        mon(sock, &format!("sendkey {c}"));
    }
    mon(sock, "sendkey ret");
}

fn kill(child: &mut std::process::Child, tag: &str, why: &str, log: &Path) -> ! {
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("{tag}: FAIL — {why}\n--- serial ---\n{}\n---", std::fs::read_to_string(log).unwrap_or_default());
    exit(1);
}

/// Log in with the fixed test admin (`thos` / `pass`), running first-run setup
/// first if the serial shows it.
fn drive_login(sock: &Path, log: &Path, child: &mut std::process::Child, tag: &str) {
    if wait_for(log, "THOS first-run setup", 8) {
        std::thread::sleep(std::time::Duration::from_millis(400));
        type_line(sock, "thos"); // admin username
        type_line(sock, "pass"); // password
        type_line(sock, "pass"); // repeat
        if !wait_for(log, "THOS login:", 20) {
            kill(child, tag, "no login prompt after first-run setup", log);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    type_line(sock, "thos");
    type_line(sock, "pass");
}

/// Boot the interactive kernel: run first-run setup + login, then type
/// `init<Enter>` and check the shell forked+execve'd it.
fn kbd_test(iso: &Path) {
    let _ = std::fs::remove_file(workspace_root().join("target/disk.img"));
    let disk = disk_image();
    let (mut child, log, sock) = spawn_interactive_qemu("kbd", iso, &disk);

    if !wait_for(&log, "THOS first-run setup", 40) {
        kill(&mut child, "kbd-test", "kernel never reached first-run setup", &log);
    }
    drive_login(&sock, &log, &mut child, "kbd-test");

    if !wait_for(&log, "interactive hold", 25) {
        kill(&mut child, "kbd-test", "never reached the shell after login", &log);
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    type_line(&sock, "init");
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let out = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();

    let after = out.split("interactive hold").nth(1).unwrap_or("");
    if after.contains("thos$ init") && after.contains("parent done") {
        println!("kbd-test: OK — first-run setup + login, shell ran `init`");
    } else {
        eprintln!("kbd-test: FAIL — after login:\n---\n{after}\n---");
        exit(1);
    }
}

/// First-run setup happens exactly once; reboot goes straight to login; a wrong
/// password is rejected.
fn login_test(iso: &Path) {
    let _ = std::fs::remove_file(workspace_root().join("target/disk.img"));
    let disk = disk_image();

    // Boot 1 — fresh disk: must run first-run setup, then log in.
    let (mut c1, log1, sock1) = spawn_interactive_qemu("login1", iso, &disk);
    if !wait_for(&log1, "THOS first-run setup", 40) {
        kill(&mut c1, "login-test", "boot 1 showed no first-run setup", &log1);
    }
    drive_login(&sock1, &log1, &mut c1, "login-test");
    let ok1 = wait_for(&log1, "interactive hold", 25);
    let _ = c1.kill();
    let _ = c1.wait();
    if !ok1 {
        eprintln!("login-test: FAIL — boot 1 never reached the shell");
        exit(1);
    }

    // Boot 2 — same disk: straight to login, no setup; reject a wrong password.
    let (mut c2, log2, sock2) = spawn_interactive_qemu("login2", iso, &disk);
    if !wait_for(&log2, "THOS login:", 40) {
        kill(&mut c2, "login-test", "boot 2 showed no login prompt", &log2);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    type_line(&sock2, "thos");
    type_line(&sock2, "wrongpw");
    if !wait_for(&log2, "login incorrect", 20) {
        kill(&mut c2, "login-test", "boot 2 did not reject the wrong password", &log2);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    type_line(&sock2, "thos");
    type_line(&sock2, "pass");
    let ok2 = wait_for(&log2, "interactive hold", 25);
    let full2 = std::fs::read_to_string(&log2).unwrap_or_default();
    let _ = c2.kill();
    let _ = c2.wait();

    if ok2 && !full2.contains("first-run setup") {
        println!("login-test: OK — setup once, login on reboot, wrong password rejected");
    } else {
        eprintln!("login-test: FAIL — ok2={ok2}, setup_ran_again={}", full2.contains("first-run setup"));
        exit(1);
    }
}

fn run_qemu(iso: &Path, gui: bool) {
    let disk = disk_image();
    let mut qemu = Command::new("qemu-system-x86_64");
    // -smp 4 so the MADT actually carries multiple Local APICs to enumerate.
    qemu.args(["-M", "q35", "-m", "512M", "-smp", "4", "-cdrom", iso.to_str().unwrap()]);
    qemu.args([
        "-drive",
        &format!("id=disk0,if=none,format=raw,file={}", disk.to_str().unwrap()),
        "-device",
        "ahci,id=ahci0",
        "-device",
        "ide-hd,drive=disk0,bus=ahci0.0",
        "-device",
        "qemu-xhci,id=xhci",
        "-device",
        "usb-kbd,bus=xhci.0",
    ]);
    qemu.args(["-serial", "stdio", "-no-reboot"]);
    qemu.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    if !gui {
        qemu.args(["-display", "none"]);
    }
    for ovmf in ["/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/ovmf/OVMF.fd"] {
        if Path::new(ovmf).exists() {
            qemu.args(["-drive", &format!("if=pflash,format=raw,readonly=on,file={ovmf}")]);
            break;
        }
    }

    let status = qemu.status().unwrap_or_else(|e| {
        eprintln!("failed to spawn qemu: {e}");
        exit(1);
    });
    match status.code() {
        Some(QEMU_SUCCESS) => {}
        Some(c) => {
            eprintln!("qemu exited with {c} (kernel did not reach ExitCode::Success)");
            exit(1);
        }
        None => {
            eprintln!("qemu killed by signal");
            exit(1);
        }
    }
}

// ===========================================================================
//  Boot picker (loaders/thos-boot) — build + a multi-disk OVMF smoke test.
// ===========================================================================

fn build_uefi() {
    let mut c = Command::new(env!("CARGO"));
    c.current_dir(workspace_root()).args([
        "build",
        "--package",
        "thos-boot",
        "--target",
        "x86_64-unknown-uefi",
        "--release",
    ]);
    run(&mut c);
}

fn uefi_efi(name: &str) -> PathBuf {
    workspace_root().join(format!("target/x86_64-unknown-uefi/release/{name}.efi"))
}

/// Make a bare-FAT (no partition table) disk image and populate it from a
/// staging tree via mtools. `files` maps an in-image path to a local source.
fn make_fat(dir: &Path, name: &str, files: &[(&str, PathBuf)]) -> PathBuf {
    let img = dir.join(name);
    let stage = dir.join(format!("{name}.stage"));
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_file(&img);

    for (dest, src) in files {
        let out = stage.join(dest.trim_start_matches('/'));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::copy(src, &out)
            .unwrap_or_else(|e| panic!("stage {src:?} -> {out:?}: {e}"));
    }

    run(Command::new("dd").args([
        "if=/dev/zero",
        &format!("of={}", img.to_str().unwrap()),
        "bs=1M",
        "count=64",
        "status=none",
    ]));
    run(Command::new("mkfs.vfat").args(["-F", "32", "-n", "THOSTEST", img.to_str().unwrap()]));

    for entry in std::fs::read_dir(&stage).unwrap() {
        let p = entry.unwrap().path();
        let mut c = Command::new("mcopy");
        c.env("MTOOLS_SKIP_CHECK", "1").args([
            "-s",
            "-i",
            img.to_str().unwrap(),
            p.to_str().unwrap(),
            "::/",
        ]);
        run(&mut c);
    }
    img
}

/// Boot the picker under OVMF with three fake disks (a "THOS" disk carrying the
/// picker + a `boot.conf` with `default=THOS`, a "Windows" disk, a "Linux"
/// disk), and assert it enumerated all three and chainloaded the THOS entry.
fn bootpick_test() {
    use std::time::{Duration, Instant};

    let root = workspace_root();
    let dir = root.join("target/bootpick");
    std::fs::create_dir_all(&dir).unwrap();

    let picker = uefi_efi("thos-boot");
    let stub = uefi_efi("thos-boot-stub");
    let conf = dir.join("boot.conf");
    std::fs::write(&conf, b"timeout=1\ndefault=THOS\n").unwrap();

    let thos = make_fat(
        &dir,
        "bp-thos.img",
        &[
            ("/EFI/BOOT/BOOTX64.EFI", picker.clone()),
            ("/EFI/limine/BOOTX64.EFI", stub.clone()),
            ("/EFI/thos/boot.conf", conf.clone()),
        ],
    );
    let win = make_fat(&dir, "bp-win.img", &[("/EFI/Microsoft/Boot/bootmgfw.efi", stub.clone())]);
    let lin = make_fat(&dir, "bp-lin.img", &[("/EFI/debian/grubx64.efi", stub.clone())]);

    let log = dir.join("serial.log");
    let _ = std::fs::remove_file(&log);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args(["-M", "q35", "-m", "256M", "-no-reboot", "-display", "none"]);
    qemu.args(["-serial", &format!("file:{}", log.to_str().unwrap())]);
    qemu.arg("-device").arg("ahci,id=ahci0");
    for (i, img) in [&thos, &win, &lin].iter().enumerate() {
        qemu.args([
            "-drive",
            &format!("id=d{i},if=none,format=raw,file={}", img.to_str().unwrap()),
            "-device",
            &format!("ide-hd,drive=d{i},bus=ahci0.{i}"),
        ]);
    }
    // OVMF: prefer the unified image (writable, so NVRAM works); fall back to
    // the split CODE/VARS pair with a private VARS copy.
    if Path::new("/usr/share/ovmf/OVMF.fd").exists() {
        let v = dir.join("OVMF.fd");
        std::fs::copy("/usr/share/ovmf/OVMF.fd", &v).unwrap();
        qemu.args(["-drive", &format!("if=pflash,format=raw,file={}", v.to_str().unwrap())]);
    } else if Path::new("/usr/share/OVMF/OVMF_CODE_4M.fd").exists() {
        let v = dir.join("OVMF_VARS.fd");
        std::fs::copy("/usr/share/OVMF/OVMF_VARS_4M.fd", &v).unwrap();
        qemu.args([
            "-drive",
            "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd",
            "-drive",
            &format!("if=pflash,format=raw,file={}", v.to_str().unwrap()),
        ]);
    } else {
        eprintln!("bootpick-test: no OVMF firmware found");
        exit(1);
    }

    let mut child = qemu.spawn().expect("spawn qemu");
    let read_log = || std::fs::read_to_string(&log).unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !read_log().contains("STUB OK") {
        std::thread::sleep(Duration::from_millis(300));
    }
    std::thread::sleep(Duration::from_millis(300));
    let out = read_log();
    let _ = child.kill();
    let _ = child.wait();

    let want = [
        ("picker banner", "THOS boot picker"),
        ("windows entry", "Windows Boot Manager"),
        ("linux entry", "Debian (GRUB)"),
        ("thos entry", "THOS"),
        ("chainloaded a stub", "STUB OK"),
        ("chainloaded the THOS entry", "limine"),
    ];
    let mut ok = true;
    for (what, needle) in want {
        if !out.contains(needle) {
            eprintln!("bootpick-test: FAIL — missing {what} ({needle:?})");
            ok = false;
        }
    }
    if ok {
        println!("bootpick-test: OK — enumerated 3 disks, counted down, chainloaded THOS");
    } else {
        eprintln!("--- serial log ---\n{out}\n---");
        exit(1);
    }
}

// ===========================================================================
//  Disk write tests — boot the kernel's storage milestone headless, then verify
//  the result from the host against the raw disk image.
// ===========================================================================

/// Boot the non-interactive kernel with `disk` attached over AHCI, wait for a
/// clean `ExitCode::Success` halt, and return the serial log. Exits on failure.
fn boot_kernel_headless(tag: &str, iso: &Path, disk: &Path, smp: u32) -> String {
    use std::time::{Duration, Instant};

    let log = workspace_root().join(format!("target/{tag}-serial.log"));
    let _ = std::fs::remove_file(&log);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args(["-M", "q35", "-m", "512M", "-smp", &smp.to_string(), "-cdrom", iso.to_str().unwrap()]);
    qemu.args([
        "-drive", &format!("id=disk0,if=none,format=raw,file={}", disk.to_str().unwrap()),
        "-device", "ahci,id=ahci0", "-device", "ide-hd,drive=disk0,bus=ahci0.0",
    ]);
    qemu.args(["-display", "none", "-no-reboot"]);
    qemu.args(["-serial", &format!("file:{}", log.to_str().unwrap())]);
    qemu.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    for ovmf in ["/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/ovmf/OVMF.fd"] {
        if Path::new(ovmf).exists() {
            qemu.args(["-drive", &format!("if=pflash,format=raw,readonly=on,file={ovmf}")]);
            break;
        }
    }

    let mut child = qemu.spawn().expect("spawn qemu");
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(s) = child.try_wait().expect("wait qemu") {
            break Some(s);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let serial = std::fs::read_to_string(&log).unwrap_or_default();
    if status.and_then(|s| s.code()) != Some(QEMU_SUCCESS) {
        eprintln!("{tag}: kernel did not halt cleanly\n--- serial ---\n{serial}\n---");
        exit(1);
    }
    serial
}

/// Must match `SCRATCH_LBA` / the pattern in `kernel/src/main.rs`.
const AHCI_SCRATCH_LBA: u64 = 20_000;

fn ahci_test(iso: &Path) {
    use std::io::Read;

    let root = workspace_root();
    let _ = std::fs::remove_file(root.join("target/disk.img")); // fresh: scratch = zeros
    let disk = disk_image();

    let serial = boot_kernel_headless("ahci", iso, &disk, 4);
    if !serial.contains("THOS: ahci write ok") {
        eprintln!("ahci-test: FAIL — no in-boot write/read-back marker\n{serial}");
        exit(1);
    }

    // Host-side: the pattern the kernel wrote must now be in the disk file.
    let mut f = std::fs::File::open(&disk).expect("open disk.img");
    let mut got = [0u8; 512];
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(AHCI_SCRATCH_LBA * 512)).unwrap();
    f.read_exact(&mut got).expect("read scratch sector");

    let want: Vec<u8> = (0..512u32).map(|i| (i as u8) ^ 0xA5).collect();
    if got[..] == want[..] {
        println!("ahci-test: OK — kernel round-tripped LBA {AHCI_SCRATCH_LBA} and it persisted");
    } else {
        eprintln!("ahci-test: FAIL — disk image scratch sector does not hold the pattern");
        eprintln!("  want[..16] {:02x?}\n  got [..16] {:02x?}", &want[..16], &got[..16]);
        exit(1);
    }
}

/// Boot the ext2-write milestone, then from the host run `e2fsck -fn` on the
/// image and `debugfs` the files the kernel created.
fn ext2_test(iso: &Path) {
    let root = workspace_root();
    let _ = std::fs::remove_file(root.join("target/disk.img")); // start from a pristine fs
    let disk = disk_image();

    let serial = boot_kernel_headless("ext2", iso, &disk, 4);
    if !serial.contains("THOS: ext2 write ok") {
        eprintln!("ext2-test: FAIL — no in-boot ext2-write marker\n{serial}");
        exit(1);
    }

    // The filesystem must still be consistent after our writes.
    let fsck = Command::new("e2fsck")
        .args(["-fn", disk.to_str().unwrap()])
        .output()
        .expect("run e2fsck");
    if !fsck.status.success() {
        eprintln!(
            "ext2-test: FAIL — e2fsck reported problems (exit {:?})\n{}\n{}",
            fsck.status.code(),
            String::from_utf8_lossy(&fsck.stdout),
            String::from_utf8_lossy(&fsck.stderr),
        );
        exit(1);
    }

    let cat = |path: &str| -> String {
        let o = Command::new("debugfs")
            .args(["-R", &format!("cat {path}"), disk.to_str().unwrap()])
            .output()
            .expect("run debugfs");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let a = cat("/thos-created.txt");
    let b = cat("/thosdir/nested.txt");
    if a.contains("ext2 write works on THOS") && b.contains("nested ok") {
        println!("ext2-test: OK — e2fsck clean; /thos-created.txt + /thosdir/nested.txt on disk");
    } else {
        eprintln!("ext2-test: FAIL — created files not readable from the host");
        eprintln!("  /thos-created.txt   = {a:?}");
        eprintln!("  /thosdir/nested.txt = {b:?}");
        exit(1);
    }
}

/// Boot the `stress` kernel at a realistic CPU count (24 = the target's 8P×2 +
/// 8E threads) and require its SMP scheduler stress milestone to pass.
fn smp_test(iso: &Path) {
    let _ = std::fs::remove_file(workspace_root().join("target/disk.img")); // pristine fs
    let disk = disk_image();
    let serial = boot_kernel_headless("smp", iso, &disk, 24);
    if serial.contains("THOS: smp stress ok") {
        let line = serial.lines().find(|l| l.contains("smp stress ok")).unwrap_or("");
        println!("smp-test: OK — {}", line.trim());
    } else {
        eprintln!("smp-test: FAIL — stress milestone did not pass\n--- serial ---\n{serial}\n---");
        exit(1);
    }
}
