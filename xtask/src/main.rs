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
        other => {
            eprintln!("unknown command: {other}");
            eprintln!(
                "usage: cargo xtask [build|iso|run|kbd-test|bootpick|bootpick-test|ahci-test] [--gui]"
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
//  AHCI write path — boot the kernel's write/read-back milestone, then confirm
//  from the host that the pattern actually persisted into the disk image.
// ===========================================================================

/// Must match `SCRATCH_LBA` / the pattern in `kernel/src/main.rs`.
const AHCI_SCRATCH_LBA: u64 = 20_000;

fn ahci_test(iso: &Path) {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let root = workspace_root();
    // Force a fresh disk image so the scratch sector starts as zeros.
    let _ = std::fs::remove_file(root.join("target/disk.img"));
    let disk = disk_image();
    let log = root.join("target/ahci-serial.log");
    let _ = std::fs::remove_file(&log);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args(["-M", "q35", "-m", "512M", "-smp", "4", "-cdrom", iso.to_str().unwrap()]);
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
        eprintln!("ahci-test: kernel did not halt cleanly\n--- serial ---\n{serial}\n---");
        exit(1);
    }
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
        println!(
            "ahci-test: OK — kernel round-tripped LBA {AHCI_SCRATCH_LBA} and it persisted to the image"
        );
    } else {
        eprintln!("ahci-test: FAIL — disk image scratch sector does not hold the pattern");
        eprintln!("  first 16 want: {:02x?}", &want[..16]);
        eprintln!("  first 16 got:  {:02x?}", &got[..16]);
        exit(1);
    }
}
