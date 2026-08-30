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
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: cargo xtask [build|iso|run|kbd-test] [--gui]");
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

/// Boot the (interactive-feature) kernel, type `init<Enter>` into the USB
/// keyboard via the QEMU monitor, and check that the console echoed it *and*
/// that the shell forked+execve'd `/init` (its "parent done" landed on serial).
fn kbd_test(iso: &Path) {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let disk = disk_image();
    let root = workspace_root();
    let log = root.join("target/kbd-serial.log");
    let sock = root.join("target/kbd-mon.sock");
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
    let mut child = qemu.spawn().expect("spawn qemu");

    let read_log = || std::fs::read_to_string(&log).unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && !read_log().contains("interactive hold") {
        std::thread::sleep(Duration::from_millis(300));
    }
    if !read_log().contains("interactive hold") {
        let _ = child.kill();
        eprintln!("kbd-test: kernel never reached the interactive hold\n{}", read_log());
        exit(1);
    }
    std::thread::sleep(Duration::from_millis(500));

    for k in ["i", "n", "i", "t", "ret"] {
        let mut s = std::os::unix::net::UnixStream::connect(&sock).expect("connect monitor");
        use std::io::Write;
        writeln!(s, "sendkey {k}").ok();
        let mut _drain = String::new();
        let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
        let _ = s.read_to_string(&mut _drain);
        std::thread::sleep(Duration::from_millis(150));
    }
    std::thread::sleep(Duration::from_millis(1500));

    let out = read_log();
    let _ = child.kill();
    let _ = child.wait();

    let after = out.split("interactive hold").nth(1).unwrap_or("");
    let echoed = after.contains("thos$ init");
    let ran = after.contains("parent done");
    if echoed && ran {
        println!("kbd-test: OK — typed `init`, shell forked+execve'd it (saw \"parent done\")");
    } else {
        eprintln!(
            "kbd-test: FAIL — echoed={echoed} ran={ran} (want both)\n---\n{after}\n---"
        );
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
