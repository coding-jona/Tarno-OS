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
        "ncq-error-test" => {
            build_kernel(&["faulttest"]);
            let iso = build_iso();
            ncq_error_test(&iso);
        }
        "busybox-test" => {
            build_kernel(&["bbtest"]);
            let iso = build_iso();
            busybox_test(&iso);
        }
        "pipe-test" => {
            build_kernel(&["pipetest"]);
            let iso = build_iso();
            pipe_test(&iso);
        }
        "fat-test" => {
            build_kernel(&[]);
            let iso = build_iso();
            fat_test(&iso);
        }
        "pe-test" => {
            build_kernel(&["petest"]);
            let iso = build_iso();
            pe_test(&iso);
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!(
                "usage: cargo xtask [build|iso|run|kbd-test|bootpick|bootpick-test|ahci-test|ext2-test|smp-test|ncq-error-test|busybox-test|pipe-test|fat-test|pe-test] [--gui]"
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
    // 16384 1-KiB blocks = 2 block groups, so `sparse_super` puts a backup
    // superblock + GDT in group 1 and the ext2 write path has to keep it synced.
    run(Command::new("mke2fs").args([
        "-q", "-F", "-t", "ext2", "-b", "1024", "-I", "128",
        "-O", "^resize_inode,^dir_index,^ext_attr",
        img.to_str().unwrap(), "16384",
    ]));
    // Grow the backing file past the 16 MiB filesystem: scratch space for the
    // AHCI write test (LBA 50000) plus room for the FAT32 volume at LBA 51000.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&img)
        .and_then(|f| f.set_len(96 * 1024 * 1024))
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

    // A real, unmodified statically-linked BusyBox -> /busybox (Milestone 2:
    // stock Linux x86-64 ELF binaries run as-is). From the `busybox-static`
    // package.
    for cand in ["/bin/busybox", "/usr/bin/busybox"] {
        if std::fs::metadata(cand).map(|m| m.len() > 100_000).unwrap_or(false) {
            run(Command::new("debugfs").args([
                "-w", "-R", &format!("write {cand} busybox"),
                img.to_str().unwrap(),
            ]));
            break;
        }
    }

    // A hand-assembled statically linked Win64 `.exe` for the native PE loader,
    // plus the file it opens with CreateFileA / ReadFile.
    let exe = root.join("target/pe-hello.exe");
    write_pe_hello(&exe);
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} pe-hello.exe", exe.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));
    let peread = root.join("target/pe-read.txt");
    std::fs::write(&peread, b"PE ReadFile OK via CreateFileA\n").unwrap();
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} pe-read.txt", peread.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));

    // A real on-disk PE DLL at C:\Windows\System32\thoscrt.dll — the exe imports
    // thoscrt!thos_add, and thoscrt itself imports KERNEL32!GetLastError.
    let thoscrt = root.join("target/thoscrt.dll");
    write_thoscrt_dll(&thoscrt);
    for dir in ["/Windows", "/Windows/System32"] {
        run(Command::new("debugfs").args(["-w", "-R", &format!("mkdir {dir}"), img.to_str().unwrap()]));
    }
    run(Command::new("debugfs").args([
        "-w", "-R",
        &format!("write {} /Windows/System32/thoscrt.dll", thoscrt.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));

    // DllMain-returns-FALSE test: failcrt.dll aborts init; pe-dllfail.exe's
    // entry (which prints a line) must therefore never run.
    let failcrt = root.join("target/failcrt.dll");
    write_failcrt_dll(&failcrt);
    run(Command::new("debugfs").args([
        "-w", "-R",
        &format!("write {} /Windows/System32/failcrt.dll", failcrt.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));
    let pedllfail = root.join("target/pe-dllfail.exe");
    write_pe_dllfail(&pedllfail);
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} pe-dllfail.exe", pedllfail.to_str().unwrap()),
        img.to_str().unwrap(),
    ]));

    // BusyBox applet links: `/bin/<applet>` hard-links to the single `/busybox`
    // inode, so the shell can run `ls`, `cat`, ... by PATH lookup (BusyBox
    // dispatches on `basename(argv[0])`). `debugfs ln` does not maintain the
    // inode link count, so set it explicitly afterwards or e2fsck complains.
    const APPLETS: &[&str] = &[
        "busybox", "ls", "cat", "echo", "pwd", "mkdir", "rmdir", "rm", "cp", "mv",
        "ln", "touch", "head", "tail", "wc", "grep", "sort", "uniq", "true", "false",
        "env", "sleep", "clear", "sh",
    ];
    run(Command::new("debugfs").args(["-w", "-R", "mkdir /bin", img.to_str().unwrap()]));
    for app in APPLETS {
        run(Command::new("debugfs").args([
            "-w", "-R", &format!("ln /busybox /bin/{app}"),
            img.to_str().unwrap(),
        ]));
    }
    // links_count = the root `/busybox` entry + every `/bin/*` link.
    let links = 1 + APPLETS.len();
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("sif /busybox links_count {links}"),
        img.to_str().unwrap(),
    ]));

    // A self-contained GPT disk image — one EFI System Partition holding a
    // FAT32 volume with `/EFI/THOS/HELLO.TXT` — spliced into a hole past the
    // ext2 image (LBA 51000; the fs is the first 16 MiB, the AHCI scratch write
    // is a single sector at LBA 50000). The kernel walks GPT → ESP → FAT32.
    // Needs `sfdisk` (util-linux), `mkfs.vfat` (dosfstools), `mmd`/`mcopy`
    // (mtools).
    let gpt = root.join("target/esp-gpt.img");
    let fat = root.join("target/esp-fat.img");
    let hello = root.join("target/fat-hello.txt");
    std::fs::write(&hello, b"THOS reads FAT\n").unwrap();
    for f in [&gpt, &fat] {
        let _ = std::fs::remove_file(f);
    }

    // 48 MiB FAT32 volume.
    let fat_sectors: u64 = 48 * 1024 * 1024 / 512;
    run(Command::new("mkfs.vfat").args([
        "-F", "32", "-n", "THOSESP", "-C", fat.to_str().unwrap(), &(fat_sectors / 2).to_string(),
    ]));
    run(Command::new("mmd").args(["-i", fat.to_str().unwrap(), "::/EFI", "::/EFI/THOS"]));
    run(Command::new("mcopy").args([
        "-i", fat.to_str().unwrap(),
        hello.to_str().unwrap(), "::/EFI/THOS/HELLO.TXT",
    ]));

    // GPT container: 1 MiB alignment gap, the ESP, then room for the backup GPT.
    let part_start = 2048u64;
    let gpt_sectors = part_start + fat_sectors + 2048;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&gpt)
        .and_then(|f| f.set_len(gpt_sectors * 512))
        .expect("create esp-gpt.img");
    let script = format!(
        "label: gpt\nstart={part_start}, size={fat_sectors}, \
         type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI System\"\n"
    );
    let mut sf = Command::new("sfdisk")
        .arg(gpt.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sfdisk");
    use std::io::Write;
    sf.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    if !sf.wait().expect("wait sfdisk").success() {
        eprintln!("sfdisk failed");
        exit(1);
    }
    run(Command::new("dd").args([
        &format!("if={}", fat.to_str().unwrap()),
        &format!("of={}", gpt.to_str().unwrap()),
        "bs=512", &format!("seek={part_start}"), "conv=notrunc", "status=none",
    ]));

    // Splice the whole GPT image into the main disk at LBA 51000.
    run(Command::new("dd").args([
        &format!("if={}", gpt.to_str().unwrap()),
        &format!("of={}", img.to_str().unwrap()),
        "bs=512", "seek=51000", "conv=notrunc", "status=none",
    ]));

    img
}

/// Write a minimal statically linked Win64 console `.exe`: `.text` (RWX) +
/// `.reloc` + `.idata`, `DYNAMIC_BASE` set. The entry:
///   1. `write(1, msg1)` via a raw `syscall` — `msg1` from an **absolute** slot
///      needing a `DIR64` base relocation;
///   2. `WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), msg2, len, &written, NULL)`
///      — real Win64 arg passing (rcx/rdx/r8/r9 + a stack slot), through the
///      **IAT** into THOS's NT stubs;
///   3. `ExitProcess(0)` through the IAT.
/// Exercises header parse, section map, relocation fixup, import resolution,
/// and Win64→THOS argument marshalling.
fn write_pe_hello(path: &Path) {
    let msg1: &[u8] = b"PE on THOS via native loader\n";
    let msg2: &[u8] = b"PE via WriteFile\n";
    let msg_ntdll: &[u8] = b"PE ntdll OK\n";
    let msg_ntdll_len = msg_ntdll.len();
    const IMAGE_BASE: u64 = 0x1_4000_0000;
    const SECT_ALIGN: u32 = 0x1000;
    const FILE_ALIGN: u32 = 0x200;
    let text_rva = 0x1000u32;
    let reloc_rva = 0x2000u32;
    let idata_rva = 0x3000u32;

    // --- .idata: imports from KERNEL32.dll and the on-disk thoscrt.dll ---
    let k32_funcs: &[&[u8]] = &[
        b"ExitProcess",      // NT idx 0
        b"GetStdHandle",     // 1
        b"WriteFile",        // 2
        b"GetLastError",     // 3
        b"CreateFileA",      // 5
        b"ReadFile",         // 6
        b"GetCommandLineA",  // 8
        b"GetModuleHandleA", // 9
        b"VirtualAlloc",     // 10
        b"GetProcessHeap",   // 13
        b"HeapAlloc",        // 14
        b"GetProcAddress",   // 16
        b"LoadLibraryA",     // 17
    ];
    // A func spelled `#N` is imported by ordinal N instead of by name.
    let imports: [(&[u8], &[&[u8]]); 2] = [
        (b"KERNEL32.dll", k32_funcs),
        (b"thoscrt.dll", &[b"thos_add", b"#2", b"thos_fwd"]),
    ];
    let n_imp = imports.len();
    let import_dir_size = ((n_imp + 1) * 20) as u32;

    let put32 = |b: &mut Vec<u8>, at: u32, v: u32| {
        b[at as usize..at as usize + 4].copy_from_slice(&v.to_le_bytes());
    };
    let put64 = |b: &mut Vec<u8>, at: u32, v: u64| {
        b[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
    };

    // IMPORT_DESCRIPTOR[n_imp] + null terminator, 8-aligned.
    let mut idata: Vec<u8> = vec![0u8; ((n_imp + 1) * 20 + 7) & !7];
    // Per DLL: ILT (len+1 thunks) then IAT (len+1 thunks).
    let mut ilt_at = vec![0u32; n_imp];
    let mut iat_at = vec![0u32; n_imp];
    for d in 0..n_imp {
        ilt_at[d] = idata.len() as u32;
        idata.resize(idata.len() + (imports[d].1.len() + 1) * 8, 0);
        iat_at[d] = idata.len() as u32;
        idata.resize(idata.len() + (imports[d].1.len() + 1) * 8, 0);
    }
    // Thunk value per import: an ORDINAL_FLAG|ordinal for `#N`, else the RVA of
    // a freshly emitted hint/name entry.
    let mut thunks: Vec<Vec<u64>> = vec![Vec::new(); n_imp];
    for d in 0..n_imp {
        for f in imports[d].1 {
            if let Some(ord) = f.strip_prefix(b"#") {
                let n: u16 = std::str::from_utf8(ord).unwrap().parse().unwrap();
                thunks[d].push(0x8000_0000_0000_0000u64 | n as u64);
            } else {
                if idata.len() % 2 != 0 {
                    idata.push(0);
                }
                thunks[d].push((idata_rva + idata.len() as u32) as u64);
                idata.extend_from_slice(&[0, 0]); // hint
                idata.extend_from_slice(f);
                idata.push(0);
            }
        }
    }
    // DLL name strings.
    let mut dllname_rva = vec![0u32; n_imp];
    for d in 0..n_imp {
        if idata.len() % 2 != 0 {
            idata.push(0);
        }
        dllname_rva[d] = idata_rva + idata.len() as u32;
        idata.extend_from_slice(imports[d].0);
        idata.push(0);
    }
    while idata.len() % 16 != 0 {
        idata.push(0);
    }
    // IMPORT_DESCRIPTORs + thunk arrays (ILT == IAT pre-load).
    for d in 0..n_imp {
        let e = (d * 20) as u32;
        put32(&mut idata, e, idata_rva + ilt_at[d]); // OriginalFirstThunk
        put32(&mut idata, e + 12, dllname_rva[d]); // Name
        put32(&mut idata, e + 16, idata_rva + iat_at[d]); // FirstThunk
        for k in 0..imports[d].1.len() as u32 {
            put64(&mut idata, ilt_at[d] + k * 8, thunks[d][k as usize]);
            put64(&mut idata, iat_at[d] + k * 8, thunks[d][k as usize]);
        }
    }

    let iat0 = idata_rva + iat_at[0]; // KERNEL32 IAT
    let iat_exit = iat0;
    let iat_gsh = iat0 + 8;
    let iat_wf = iat0 + 16;
    let iat_cf = iat0 + 32; // CreateFileA
    let iat_rf = iat0 + 40; // ReadFile
    let iat_gcl = iat0 + 48; // GetCommandLineA
    let iat_gmh = iat0 + 56; // GetModuleHandleA
    let iat_va = iat0 + 64; // VirtualAlloc
    let iat_gph = iat0 + 72; // GetProcessHeap
    let iat_ha = iat0 + 80; // HeapAlloc
    let iat_gpa = iat0 + 88; // GetProcAddress
    let iat_ll = iat0 + 96; // LoadLibraryA
    let iat_add = idata_rva + iat_at[1]; // thoscrt!thos_add  (by name)
    let iat_mul = idata_rva + iat_at[1] + 8; // thoscrt!thos_mul (by ordinal 2)
    let iat_fwd = idata_rva + iat_at[1] + 16; // thoscrt!thos_fwd (forwarded to KERNEL32.GetProcessHeap)

    // --- entry machine code (x86-64) ---
    // Deferred RIP-relative fixups: (disp32 position in `code`, target RVA).
    let mut code: Vec<u8> = Vec::new();
    let mut fixups: Vec<(usize, u32)> = Vec::new();
    macro_rules! rel {
        ($bytes:expr, $target:expr) => {{
            code.extend_from_slice(&$bytes);
            fixups.push((code.len() - 4, $target));
        }};
    }
    // slots appended after the code; RVAs filled once the code length is known
    let ptr_slot_tag = u32::MAX; // sentinel targets resolved specially
    let wr_slot_tag = u32::MAX - 1;
    let msg1_tag = u32::MAX - 2;
    let msg2_tag = u32::MAX - 3;
    let stdout_slot_tag = u32::MAX - 4;
    let nread_slot_tag = u32::MAX - 5;
    let buf_tag = u32::MAX - 6;
    let fname_tag = u32::MAX - 7;
    let msg_pp_tag = u32::MAX - 8;
    let msg_ldr_tag = u32::MAX - 9;
    let msg_va_tag = u32::MAX - 10;
    let msg_gpa_tag = u32::MAX - 11;
    let k32name_tag = u32::MAX - 12;
    let wfname_tag = u32::MAX - 13;
    let ntdllname_tag = u32::MAX - 14;
    let ntwritename_tag = u32::MAX - 15;
    let iosb_tag = u32::MAX - 16;
    let msg_ntdll_tag = u32::MAX - 17;
    let msg_dll_tag = u32::MAX - 18;
    let thoscrtname_tag = u32::MAX - 19;
    let thosaddname_tag = u32::MAX - 20;
    let msg_dll_ldr_tag = u32::MAX - 21;
    let msg_ord_tag = u32::MAX - 22;
    let msg_fwd_tag = u32::MAX - 23;
    let tls_index_tag = u32::MAX - 24;
    let msg_tls_tag = u32::MAX - 25;

    // 1) write(1, msg1, len1)
    code.extend_from_slice(&[0x48, 0xC7, 0xC0, 1, 0, 0, 0]); // mov rax, 1
    code.extend_from_slice(&[0x48, 0xC7, 0xC7, 1, 0, 0, 0]); // mov rdi, 1
    rel!([0x48, 0x8B, 0x35, 0, 0, 0, 0], ptr_slot_tag); // mov rsi, [rip+ptr_slot]
    code.extend_from_slice(&[0x48, 0xC7, 0xC2]);
    code.extend_from_slice(&(msg1.len() as u32).to_le_bytes()); // mov rdx, len1
    code.extend_from_slice(&[0x0F, 0x05]); // syscall

    // 1b) touch the TEB / PEB via %gs — faults here if gs-base / TEB / PEB are
    //     wrong, so the WriteFile line below never prints.
    code.extend_from_slice(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x30, 0, 0, 0]); // mov rax, gs:[0x30]  (TEB self)
    code.extend_from_slice(&[0x48, 0x8B, 0x40, 0x60]); // mov rax, [rax+0x60]  (PEB via TEB)
    code.extend_from_slice(&[0x48, 0x8B, 0x40, 0x10]); // mov rax, [rax+0x10]  (ImageBaseAddress)

    // 2) WriteFile(GetStdHandle(-11), msg2, len2, &written, NULL)
    code.extend_from_slice(&[0xB9, 0xF5, 0xFF, 0xFF, 0xFF]); // mov ecx, -11
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gsh); // call [rip+iat_GetStdHandle]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC3]); // mov rbx, rax  (stdout handle)
    rel!([0x48, 0x89, 0x1D, 0, 0, 0, 0], stdout_slot_tag); // mov [rip+stdout_slot], rbx
    code.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg2_tag); // lea rdx, [rip+msg2]
    code.extend_from_slice(&[0x41, 0xB8]);
    code.extend_from_slice(&(msg2.len() as u32).to_le_bytes()); // mov r8d, len2
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // sub rsp, 0x38
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]); // mov qword [rsp+0x20], 0
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf); // call [rip+iat_WriteFile]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // add rsp, 0x38

    // 2b) CreateFileA(fname, GENERIC_READ, 0, 0, OPEN_EXISTING, 0, 0) -> rbx
    rel!([0x48, 0x8D, 0x0D, 0, 0, 0, 0], fname_tag); // lea rcx, [rip+fname]
    code.extend_from_slice(&[0xBA, 0x00, 0x00, 0x00, 0x80]); // mov edx, 0x80000000 (GENERIC_READ)
    code.extend_from_slice(&[0x45, 0x31, 0xC0]); // xor r8d, r8d  (share)
    code.extend_from_slice(&[0x45, 0x31, 0xC9]); // xor r9d, r9d  (security)
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // sub rsp, 0x38
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0x03, 0, 0, 0]); // [rsp+0x20]=3 disposition
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x28, 0, 0, 0, 0]); // [rsp+0x28]=0 flags
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x30, 0, 0, 0, 0]); // [rsp+0x30]=0 template
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_cf); // call [rip+iat_CreateFileA]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // add rsp, 0x38
    code.extend_from_slice(&[0x48, 0x89, 0xC3]); // mov rbx, rax  (file handle)

    // 2c) ReadFile(rbx, buf, 64, &nread, 0)
    code.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], buf_tag); // lea rdx, [rip+buf]
    code.extend_from_slice(&[0x41, 0xB8, 0x40, 0, 0, 0]); // mov r8d, 64
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], nread_slot_tag); // lea r9, [rip+nread]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // sub rsp, 0x38
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]); // [rsp+0x20]=0 overlapped
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_rf); // call [rip+iat_ReadFile]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // add rsp, 0x38

    // 2d) WriteFile(stdout, buf, nread, &written, 0)
    rel!([0x48, 0x8B, 0x0D, 0, 0, 0, 0], stdout_slot_tag); // mov rcx, [rip+stdout_slot]
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], buf_tag); // lea rdx, [rip+buf]
    rel!([0x44, 0x8B, 0x05, 0, 0, 0, 0], nread_slot_tag); // mov r8d, [rip+nread]
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // sub rsp, 0x38
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]); // [rsp+0x20]=0
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf); // call [rip+iat_WriteFile]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // add rsp, 0x38

    // 2e) PEB->ProcessParameters->StandardOutput as the WriteFile handle
    code.extend_from_slice(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x30, 0, 0, 0]); // mov rax, gs:[0x30]  (TEB)
    code.extend_from_slice(&[0x48, 0x8B, 0x40, 0x60]); // mov rax, [rax+0x60]  (PEB)
    code.extend_from_slice(&[0x48, 0x8B, 0x48, 0x20]); // mov rcx, [rax+0x20]  (ProcessParameters)
    code.extend_from_slice(&[0x48, 0x8B, 0x49, 0x28]); // mov rcx, [rcx+0x28]  (StandardOutput)
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_pp_tag); // lea rdx, [rip+msg_pp]
    let pp_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len_pp  (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2f) walk PEB->Ldr; first module DllBase must == PEB->ImageBaseAddress
    code.extend_from_slice(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x30, 0, 0, 0]); // mov rax, gs:[0x30]
    code.extend_from_slice(&[0x48, 0x8B, 0x40, 0x60]); // mov rax, [rax+0x60]  (PEB)
    code.extend_from_slice(&[0x48, 0x8B, 0x50, 0x10]); // mov rdx, [rax+0x10]  (ImageBaseAddress)
    code.extend_from_slice(&[0x48, 0x8B, 0x48, 0x18]); // mov rcx, [rax+0x18]  (Ldr)
    code.extend_from_slice(&[0x48, 0x8B, 0x49, 0x10]); // mov rcx, [rcx+0x10]  (InLoadOrder.Flink = &entry)
    code.extend_from_slice(&[0x48, 0x8B, 0x49, 0x30]); // mov rcx, [rcx+0x30]  (entry->DllBase)
    code.extend_from_slice(&[0x48, 0x39, 0xD1]); // cmp rcx, rdx
    code.extend_from_slice(&[0x0F, 0x85, 0, 0, 0, 0]); // jne .after_ldr  (patched)
    let jne_pos = code.len() - 4;
    let jne_from = code.len();
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_ldr_tag); // lea rdx, [rip+msg_ldr]
    let ldr_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len_ldr  (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);
    let after_ldr = code.len();
    code[jne_pos..jne_pos + 4].copy_from_slice(&((after_ldr - jne_from) as i32).to_le_bytes());

    // 2g) GetCommandLineA() -> rax; WriteFile(1, rax, 22, &written, 0)
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gcl); // call [rip+iat_GetCommandLineA]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax  (LPSTR)
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    code.extend_from_slice(&[0x41, 0xB8, 22, 0, 0, 0]); // mov r8d, 22
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2h) GetModuleHandleA(NULL) — just call it (a broken stub would fault)
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28, 0x31, 0xC9]); // sub rsp,0x28 ; xor ecx,ecx
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gmh);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28

    // 2i) VirtualAlloc(0, 0x1000, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE)
    code.extend_from_slice(&[0x31, 0xC9]); // xor ecx, ecx  (lpAddress = NULL)
    code.extend_from_slice(&[0xBA, 0x00, 0x10, 0, 0]); // mov edx, 0x1000
    code.extend_from_slice(&[0x41, 0xB8, 0x00, 0x30, 0, 0]); // mov r8d, 0x3000
    code.extend_from_slice(&[0x41, 0xB9, 0x04, 0, 0, 0]); // mov r9d, 0x04
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_va); // call [rip+iat_VirtualAlloc]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0xC6, 0x00, 0x5A]); // mov byte [rax], 0x5A  (#PF if unmapped)
    code.extend_from_slice(&[0x0F, 0xB6, 0x08]); // movzx ecx, byte [rax]

    // 2j) HeapAlloc(GetProcessHeap(), 0, 64); write to it
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gph); // call [rip+iat_GetProcessHeap]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax  (hHeap)
    code.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx  (flags)
    code.extend_from_slice(&[0x41, 0xB8, 0x40, 0, 0, 0]); // mov r8d, 64
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_ha); // call [rip+iat_HeapAlloc]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0xC6, 0x00, 0x42]); // mov byte [rax], 0x42  (#PF if bad)

    // 2k) WriteFile(1, msg_va, len, &written, 0)
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_va_tag); // lea rdx, [rip+msg_va]
    let va_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len_va  (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2l) LoadLibraryA("kernel32.dll") -> rbx  (Ldr name walk),
    //     GetProcAddress(rbx, "WriteFile") -> rsi  (export-directory parse),
    //     then call the resolved pointer to print the success line.
    rel!([0x48, 0x8D, 0x0D, 0, 0, 0, 0], k32name_tag); // lea rcx, [rip+k32name]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_ll); // call [rip+iat_LoadLibraryA]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC3]); // mov rbx, rax  (HMODULE)
    code.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], wfname_tag); // lea rdx, [rip+wfname]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gpa); // call [rip+iat_GetProcAddress]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC6]); // mov rsi, rax  (resolved WriteFile)
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_gpa_tag); // lea rdx, [rip+msg_gpa]
    let gpa_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len_gpa  (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    code.extend_from_slice(&[0xFF, 0xD6]); // call rsi
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // add rsp, 0x38

    // 2m) the ntdll boundary: GetModuleHandleA("ntdll.dll") ->
    //     GetProcAddress(h, "NtWriteFile") -> call it with a real 9-arg NT
    //     signature (Event/Apc/IoStatusBlock/ByteOffset/Key), IO_STATUS_BLOCK
    //     out-param. Prints via the resolved NtWriteFile itself.
    rel!([0x48, 0x8D, 0x0D, 0, 0, 0, 0], ntdllname_tag); // lea rcx, [rip+ntdllname]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gmh); // call [rip+iat_GetModuleHandleA]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC3]); // mov rbx, rax  (hNtdll)
    code.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], ntwritename_tag); // lea rdx, [rip+ntwritename]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gpa); // call [rip+iat_GetProcAddress]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC6]); // mov rsi, rax  (NtWriteFile)
    rel!([0x48, 0x8B, 0x0D, 0, 0, 0, 0], stdout_slot_tag); // mov rcx, [rip+stdout_slot]
    code.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx  (Event)
    code.extend_from_slice(&[0x45, 0x31, 0xC0]); // xor r8d, r8d  (ApcRoutine)
    code.extend_from_slice(&[0x45, 0x31, 0xC9]); // xor r9d, r9d  (ApcContext)
    rel!([0x48, 0x8D, 0x3D, 0, 0, 0, 0], iosb_tag); // lea rdi, [rip+iosb]
    rel!([0x48, 0x8D, 0x1D, 0, 0, 0, 0], msg_ntdll_tag); // lea rbx, [rip+msg_ntdll]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x58]); // sub rsp, 0x58
    code.extend_from_slice(&[0x48, 0x89, 0x7C, 0x24, 0x20]); // mov [rsp+0x20], rdi  (IoStatusBlock)
    code.extend_from_slice(&[0x48, 0x89, 0x5C, 0x24, 0x28]); // mov [rsp+0x28], rbx  (Buffer)
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x30]);
    code.extend_from_slice(&(msg_ntdll_len as u32).to_le_bytes()); // mov qword [rsp+0x30], len
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x38, 0, 0, 0, 0]); // [rsp+0x38]=0 ByteOffset
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x40, 0, 0, 0, 0]); // [rsp+0x40]=0 Key
    code.extend_from_slice(&[0xFF, 0xD6]); // call rsi
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x58]); // add rsp, 0x58

    // 2n) thoscrt.dll — a real on-disk PE DLL from C:\Windows\System32. Call
    //     its exported thos_add(40, 2) through the IAT the loader bound to the
    //     DLL's real export; trap unless it returns 42, then print the line.
    code.extend_from_slice(&[0xB9, 40, 0, 0, 0]); // mov ecx, 40
    code.extend_from_slice(&[0xBA, 2, 0, 0, 0]); // mov edx, 2
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_add); // call [rip+iat_thos_add]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x83, 0xF8, 0x2A]); // cmp eax, 42
    code.extend_from_slice(&[0x74, 0x01]); // je +1
    code.extend_from_slice(&[0xCC]); // int3 (wrong result from thos_add)
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_dll_tag); // lea rdx, [rip+msg_dll]
    let dll_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len_dll (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2o) thoscrt.dll is now in the PEB Ldr list: resolve thos_add at runtime
    //     via GetModuleHandleA + GetProcAddress (not the static IAT), call it,
    //     trap unless 42, print.
    rel!([0x48, 0x8D, 0x0D, 0, 0, 0, 0], thoscrtname_tag); // lea rcx, [rip+thoscrtname]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gmh); // call [rip+iat_GetModuleHandleA]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC3]); // mov rbx, rax  (hThoscrt)
    code.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], thosaddname_tag); // lea rdx, [rip+thosaddname]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gpa); // call [rip+iat_GetProcAddress]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC6]); // mov rsi, rax  (resolved thos_add)
    code.extend_from_slice(&[0xB9, 40, 0, 0, 0]); // mov ecx, 40
    code.extend_from_slice(&[0xBA, 2, 0, 0, 0]); // mov edx, 2
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    code.extend_from_slice(&[0xFF, 0xD6]); // call rsi
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x83, 0xF8, 0x2A]); // cmp eax, 42
    code.extend_from_slice(&[0x74, 0x01]); // je +1
    code.extend_from_slice(&[0xCC]); // int3
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_dll_ldr_tag); // lea rdx, [rip+msg_dll_ldr]
    let dll_ldr_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2p) thos_mul is imported from thoscrt.dll BY ORDINAL (2), not by name.
    //     Call it (6*7), trap unless 42, print.
    code.extend_from_slice(&[0xB9, 6, 0, 0, 0]); // mov ecx, 6
    code.extend_from_slice(&[0xBA, 7, 0, 0, 0]); // mov edx, 7
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_mul); // call [rip+iat_thos_mul]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x83, 0xF8, 0x2A]); // cmp eax, 42
    code.extend_from_slice(&[0x74, 0x01]); // je +1
    code.extend_from_slice(&[0xCC]); // int3
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_ord_tag); // lea rdx, [rip+msg_ord]
    let ord_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2q) thos_fwd is a forwarder export (thoscrt -> KERNEL32.GetProcessHeap).
    //     After the loader follows it, calling thos_fwd() is calling
    //     GetProcessHeap() -> a fixed non-zero handle. Trap on 0, else print.
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_fwd); // call [rip+iat_thos_fwd]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
    code.extend_from_slice(&[0x75, 0x01]); // jne +1
    code.extend_from_slice(&[0xCC]); // int3
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_fwd_tag); // lea rdx, [rip+msg_fwd]
    let fwd_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 2r) TLS. The loader gave this module a static-TLS block, wrote its
    //     __tls_index, pointed TEB.ThreadLocalStoragePointer at the array, and
    //     queued `tls_cb` (emitted after block 3, out of the fall-through path)
    //     to run at process start. tls_cb writes a magic into *this thread's*
    //     TLS block; the check below reaches the block via gs:[0x58] and
    //     verifies the copied template word AND the magic.
    code.extend_from_slice(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x58, 0, 0, 0]); // mov rax, gs:[0x58]
    code.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
    code.extend_from_slice(&[0x74, 0x1D]); // jz .tlsfail
    rel!([0x8B, 0x0D, 0, 0, 0, 0], tls_index_tag); // mov ecx, [rip+tls_index]
    code.extend_from_slice(&[0x48, 0x8B, 0x04, 0xC8]); // mov rax, [rax+rcx*8]
    code.extend_from_slice(&[0x81, 0x38, 0xEF, 0xBE, 0xAD, 0xDE]); // cmp dword [rax], 0xDEADBEEF
    code.extend_from_slice(&[0x75, 0x0B]); // jne .tlsfail
    code.extend_from_slice(&[0x81, 0x78, 0x04, 0x5A, 0x5A, 0x5A, 0x5A]); // cmp dword [rax+4], 0x5A5A5A5A
    code.extend_from_slice(&[0x75, 0x02]); // jne .tlsfail
    code.extend_from_slice(&[0xEB, 0x01]); // jmp .tlsok
    code.extend_from_slice(&[0xCC]); // .tlsfail: int3
    code.extend_from_slice(&[0xB9, 0x01, 0, 0, 0]); // .tlsok: mov ecx, 1
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg_tls_tag); // lea rdx, [rip+msg_tls]
    let tls_r8 = code.len() + 2;
    code.extend_from_slice(&[0x41, 0xB8, 0, 0, 0, 0]); // mov r8d, len (patched)
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38, 0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf);
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]);

    // 3) ExitProcess(0)
    code.extend_from_slice(&[0x31, 0xC9]); // xor ecx, ecx
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_exit); // call [rip+iat_ExitProcess]
    code.extend_from_slice(&[0xCC]); // int3

    // tls_cb — reached only through the pointer the loader queued, never fallen
    // into. void tls_cb(PVOID hinst, DWORD reason, PVOID reserved).
    let tls_cb_off = code.len();
    code.extend_from_slice(&[0x83, 0xFA, 0x01]); // cmp edx, 1  (DLL_PROCESS_ATTACH)
    code.extend_from_slice(&[0x75, 0x1A]); // jne .cbret
    code.extend_from_slice(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x58, 0, 0, 0]); // mov rax, gs:[0x58]
    rel!([0x8B, 0x0D, 0, 0, 0, 0], tls_index_tag); // mov ecx, [rip+tls_index]
    code.extend_from_slice(&[0x48, 0x8B, 0x04, 0xC8]); // mov rax, [rax+rcx*8]  (block VA)
    code.extend_from_slice(&[0xC7, 0x40, 0x04, 0x5A, 0x5A, 0x5A, 0x5A]); // mov dword [rax+4], 0x5A5A5A5A
    code.extend_from_slice(&[0xC3]); // .cbret: ret

    // --- data slots at the end of .text ---
    while code.len() % 8 != 0 {
        code.push(0);
    }
    let ptr_off = code.len();
    code.extend_from_slice(&[0u8; 8]); // absolute ptr to msg1 (DIR64-relocated)
    let wr_off = code.len();
    code.extend_from_slice(&[0u8; 8]); // DWORD `written` (+ pad)
    let stdout_off = code.len();
    code.extend_from_slice(&[0u8; 8]); // saved stdout HANDLE
    let nread_off = code.len();
    code.extend_from_slice(&[0u8; 8]); // DWORD `nread` (+ pad)
    let buf_off = code.len();
    code.extend_from_slice(&[0u8; 64]); // ReadFile buffer
    let fname_off = code.len();
    code.extend_from_slice(b"C:\\pe-read.txt\0");
    let k32name_off = code.len();
    code.extend_from_slice(b"kernel32.dll\0");
    let wfname_off = code.len();
    code.extend_from_slice(b"WriteFile\0");
    let ntdllname_off = code.len();
    code.extend_from_slice(b"ntdll.dll\0");
    let ntwritename_off = code.len();
    code.extend_from_slice(b"NtWriteFile\0");
    let thoscrtname_off = code.len();
    code.extend_from_slice(b"thoscrt.dll\0");
    let thosaddname_off = code.len();
    code.extend_from_slice(b"thos_add\0");
    while code.len() % 8 != 0 {
        code.push(0);
    }
    let iosb_off = code.len();
    code.extend_from_slice(&[0u8; 16]); // IO_STATUS_BLOCK { NTSTATUS; ULONG_PTR }
    let tls_dir_off = code.len();
    code.extend_from_slice(&[0u8; 40]); // IMAGE_TLS_DIRECTORY64 (fields filled + DIR64-relocated)
    let tls_raw_off = code.len();
    code.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // TLS template word 0
    code.extend_from_slice(&[0u8; 12]); // rest of the 16-byte template
    let tls_index_off = code.len();
    code.extend_from_slice(&[0u8; 4]); // loader writes __tls_index here
    while code.len() % 8 != 0 {
        code.push(0);
    }
    let tls_cbs_off = code.len();
    code.extend_from_slice(&[0u8; 16]); // PIMAGE_TLS_CALLBACK[] = { &tls_cb, NULL }
    let msg1_off = code.len();
    code.extend_from_slice(msg1);
    let msg2_off = code.len();
    code.extend_from_slice(msg2);
    let msg_pp: &[u8] = b"PE ProcParams OK\n";
    let msg_ldr: &[u8] = b"PE Ldr OK\n";
    let msg_va: &[u8] = b"PE VirtualAlloc+Heap OK\n";
    let msg_gpa: &[u8] = b"PE GetProcAddress OK\n";
    let msg_dll: &[u8] = b"PE dll thos_add=42 (DllMain ran)\n";
    let msg_dll_ldr: &[u8] = b"PE dll Ldr OK\n";
    let msg_ord: &[u8] = b"PE dll ordinal OK\n";
    let msg_fwd: &[u8] = b"PE dll forward OK\n";
    let msg_pp_off = code.len();
    code.extend_from_slice(msg_pp);
    let msg_ldr_off = code.len();
    code.extend_from_slice(msg_ldr);
    let msg_va_off = code.len();
    code.extend_from_slice(msg_va);
    let msg_gpa_off = code.len();
    code.extend_from_slice(msg_gpa);
    let msg_ntdll_off = code.len();
    code.extend_from_slice(msg_ntdll);
    let msg_dll_off = code.len();
    code.extend_from_slice(msg_dll);
    let msg_dll_ldr_off = code.len();
    code.extend_from_slice(msg_dll_ldr);
    let msg_ord_off = code.len();
    code.extend_from_slice(msg_ord);
    let msg_fwd_off = code.len();
    code.extend_from_slice(msg_fwd);
    let msg_tls: &[u8] = b"PE TLS OK\n";
    let msg_tls_off = code.len();
    code.extend_from_slice(msg_tls);

    code[tls_r8..tls_r8 + 4].copy_from_slice(&(msg_tls.len() as u32).to_le_bytes());
    code[dll_r8..dll_r8 + 4].copy_from_slice(&(msg_dll.len() as u32).to_le_bytes());
    code[dll_ldr_r8..dll_ldr_r8 + 4].copy_from_slice(&(msg_dll_ldr.len() as u32).to_le_bytes());
    code[ord_r8..ord_r8 + 4].copy_from_slice(&(msg_ord.len() as u32).to_le_bytes());
    code[fwd_r8..fwd_r8 + 4].copy_from_slice(&(msg_fwd.len() as u32).to_le_bytes());
    code[pp_r8..pp_r8 + 4].copy_from_slice(&(msg_pp.len() as u32).to_le_bytes());
    code[ldr_r8..ldr_r8 + 4].copy_from_slice(&(msg_ldr.len() as u32).to_le_bytes());
    code[va_r8..va_r8 + 4].copy_from_slice(&(msg_va.len() as u32).to_le_bytes());
    code[gpa_r8..gpa_r8 + 4].copy_from_slice(&(msg_gpa.len() as u32).to_le_bytes());
    code[ptr_off..ptr_off + 8]
        .copy_from_slice(&(IMAGE_BASE + text_rva as u64 + msg1_off as u64).to_le_bytes());

    // IMAGE_TLS_DIRECTORY64 fields (preferred-base VAs; DIR64-relocated at load).
    let ib = IMAGE_BASE + text_rva as u64;
    code[tls_dir_off..tls_dir_off + 8].copy_from_slice(&(ib + tls_raw_off as u64).to_le_bytes());
    code[tls_dir_off + 8..tls_dir_off + 16]
        .copy_from_slice(&(ib + tls_raw_off as u64 + 16).to_le_bytes());
    code[tls_dir_off + 16..tls_dir_off + 24]
        .copy_from_slice(&(ib + tls_index_off as u64).to_le_bytes());
    code[tls_dir_off + 24..tls_dir_off + 32]
        .copy_from_slice(&(ib + tls_cbs_off as u64).to_le_bytes());
    code[tls_cbs_off..tls_cbs_off + 8].copy_from_slice(&(ib + tls_cb_off as u64).to_le_bytes());

    for (pos, target) in fixups {
        let target_rva = match target {
            t if t == ptr_slot_tag => text_rva + ptr_off as u32,
            t if t == wr_slot_tag => text_rva + wr_off as u32,
            t if t == stdout_slot_tag => text_rva + stdout_off as u32,
            t if t == nread_slot_tag => text_rva + nread_off as u32,
            t if t == buf_tag => text_rva + buf_off as u32,
            t if t == fname_tag => text_rva + fname_off as u32,
            t if t == msg1_tag => text_rva + msg1_off as u32,
            t if t == msg2_tag => text_rva + msg2_off as u32,
            t if t == msg_pp_tag => text_rva + msg_pp_off as u32,
            t if t == msg_ldr_tag => text_rva + msg_ldr_off as u32,
            t if t == msg_va_tag => text_rva + msg_va_off as u32,
            t if t == msg_gpa_tag => text_rva + msg_gpa_off as u32,
            t if t == k32name_tag => text_rva + k32name_off as u32,
            t if t == wfname_tag => text_rva + wfname_off as u32,
            t if t == ntdllname_tag => text_rva + ntdllname_off as u32,
            t if t == ntwritename_tag => text_rva + ntwritename_off as u32,
            t if t == iosb_tag => text_rva + iosb_off as u32,
            t if t == msg_ntdll_tag => text_rva + msg_ntdll_off as u32,
            t if t == msg_dll_tag => text_rva + msg_dll_off as u32,
            t if t == msg_dll_ldr_tag => text_rva + msg_dll_ldr_off as u32,
            t if t == thoscrtname_tag => text_rva + thoscrtname_off as u32,
            t if t == thosaddname_tag => text_rva + thosaddname_off as u32,
            t if t == msg_ord_tag => text_rva + msg_ord_off as u32,
            t if t == msg_fwd_tag => text_rva + msg_fwd_off as u32,
            t if t == tls_index_tag => text_rva + tls_index_off as u32,
            t if t == msg_tls_tag => text_rva + msg_tls_off as u32,
            rva => rva,
        };
        let next_rva = text_rva as i64 + pos as i64 + 4;
        code[pos..pos + 4].copy_from_slice(&((target_rva as i64 - next_rva) as i32).to_le_bytes());
    }
    // --- .reloc: DIR64 fixups for the msg1 pointer slot and the five TLS VA
    //     fields, grouped into one block per 4 KiB page. ---
    let mut dir64: Vec<u32> = vec![
        text_rva + ptr_off as u32,
        text_rva + tls_dir_off as u32,      // StartAddressOfRawData
        text_rva + tls_dir_off as u32 + 8,  // EndAddressOfRawData
        text_rva + tls_dir_off as u32 + 16, // AddressOfIndex
        text_rva + tls_dir_off as u32 + 24, // AddressOfCallBacks
        text_rva + tls_cbs_off as u32,      // callback[0]
    ];
    dir64.sort_unstable();
    let mut reloc: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < dir64.len() {
        let page = dir64[i] & !0xFFF;
        let mut ents: Vec<u16> = Vec::new();
        while i < dir64.len() && dir64[i] & !0xFFF == page {
            ents.push((10u16 << 12) | (dir64[i] & 0xFFF) as u16);
            i += 1;
        }
        if ents.len() % 2 != 0 {
            ents.push(0); // ABSOLUTE pad -> 4-align the block
        }
        reloc.extend_from_slice(&page.to_le_bytes());
        reloc.extend_from_slice(&(8 + ents.len() as u32 * 2).to_le_bytes());
        for e in ents {
            reloc.extend_from_slice(&e.to_le_bytes());
        }
    }

    // --- PE32+ container ---
    let text_vsize = code.len() as u32;
    let text_raw_ptr = 0x200u32;
    let text_raw_size = text_vsize.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let reloc_raw_ptr = text_raw_ptr + text_raw_size;
    let reloc_vsize = reloc.len() as u32;
    let reloc_raw_size = reloc_vsize.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let idata_raw_ptr = reloc_raw_ptr + reloc_raw_size;
    let idata_vsize = idata.len() as u32;
    let idata_raw_size = idata_vsize.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let size_of_image = idata_rva + idata_vsize.div_ceil(SECT_ALIGN) * SECT_ALIGN;
    let size_of_headers = 0x200u32;

    let mut pe = vec![0u8; (idata_raw_ptr + idata_raw_size) as usize];
    pe[0..2].copy_from_slice(b"MZ");
    pe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());

    let o = 0x40usize;
    pe[o..o + 4].copy_from_slice(b"PE\0\0");
    let coff = o + 4;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes()); // Machine
    pe[coff + 2..coff + 4].copy_from_slice(&3u16.to_le_bytes()); // NumberOfSections
    let opt_size = 0xF0u16;
    pe[coff + 16..coff + 18].copy_from_slice(&opt_size.to_le_bytes());
    pe[coff + 18..coff + 20].copy_from_slice(&0x0022u16.to_le_bytes()); // EXECUTABLE | LARGE_ADDRESS_AWARE

    let opt = coff + 20;
    pe[opt..opt + 2].copy_from_slice(&0x020Bu16.to_le_bytes()); // PE32+
    pe[opt + 16..opt + 20].copy_from_slice(&text_rva.to_le_bytes()); // AddressOfEntryPoint
    pe[opt + 20..opt + 24].copy_from_slice(&text_rva.to_le_bytes()); // BaseOfCode
    pe[opt + 24..opt + 32].copy_from_slice(&IMAGE_BASE.to_le_bytes()); // ImageBase
    pe[opt + 32..opt + 36].copy_from_slice(&SECT_ALIGN.to_le_bytes());
    pe[opt + 36..opt + 40].copy_from_slice(&FILE_ALIGN.to_le_bytes());
    pe[opt + 40..opt + 42].copy_from_slice(&6u16.to_le_bytes()); // MajorOperatingSystemVersion
    pe[opt + 48..opt + 50].copy_from_slice(&6u16.to_le_bytes()); // MajorSubsystemVersion
    pe[opt + 56..opt + 60].copy_from_slice(&size_of_image.to_le_bytes());
    pe[opt + 60..opt + 64].copy_from_slice(&size_of_headers.to_le_bytes());
    pe[opt + 68..opt + 70].copy_from_slice(&3u16.to_le_bytes()); // Subsystem = CONSOLE
    pe[opt + 70..opt + 72].copy_from_slice(&0x0040u16.to_le_bytes()); // DllCharacteristics = DYNAMIC_BASE
    pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes()); // NumberOfRvaAndSizes
    // data directory 1 = IMPORT
    pe[opt + 112 + 8..opt + 112 + 12].copy_from_slice(&idata_rva.to_le_bytes());
    pe[opt + 112 + 12..opt + 112 + 16].copy_from_slice(&import_dir_size.to_le_bytes());
    // data directory 5 = BASE_RELOC
    pe[opt + 112 + 5 * 8..opt + 112 + 5 * 8 + 4].copy_from_slice(&reloc_rva.to_le_bytes());
    pe[opt + 112 + 5 * 8 + 4..opt + 112 + 5 * 8 + 8].copy_from_slice(&reloc_vsize.to_le_bytes());
    // data directory 9 = TLS
    pe[opt + 112 + 9 * 8..opt + 112 + 9 * 8 + 4]
        .copy_from_slice(&(text_rva + tls_dir_off as u32).to_le_bytes());
    pe[opt + 112 + 9 * 8 + 4..opt + 112 + 9 * 8 + 8].copy_from_slice(&40u32.to_le_bytes());
    // data directory 12 = IAT
    pe[opt + 112 + 12 * 8..opt + 112 + 12 * 8 + 4].copy_from_slice(&iat_exit.to_le_bytes());
    pe[opt + 112 + 12 * 8 + 4..opt + 112 + 12 * 8 + 8]
        .copy_from_slice(&(k32_funcs.len() as u32 * 8).to_le_bytes());

    let mut sec = |i: usize, name: &[u8], vsize: u32, rva: u32, raw_size: u32, raw_ptr: u32, ch: u32| {
        let h = opt + opt_size as usize + i * 40;
        pe[h..h + name.len()].copy_from_slice(name);
        pe[h + 8..h + 12].copy_from_slice(&vsize.to_le_bytes());
        pe[h + 12..h + 16].copy_from_slice(&rva.to_le_bytes());
        pe[h + 16..h + 20].copy_from_slice(&raw_size.to_le_bytes());
        pe[h + 20..h + 24].copy_from_slice(&raw_ptr.to_le_bytes());
        pe[h + 36..h + 40].copy_from_slice(&ch.to_le_bytes());
    };
    sec(0, b".text", text_vsize, text_rva, text_raw_size, text_raw_ptr, 0x6000_0020); // CODE|EXEC|READ
    sec(1, b".reloc", reloc_vsize, reloc_rva, reloc_raw_size, reloc_raw_ptr, 0x4200_0040); // IDATA|DISCARD|READ
    sec(2, b".idata", idata_vsize, idata_rva, idata_raw_size, idata_raw_ptr, 0xC000_0040); // IDATA|READ|WRITE

    pe[text_raw_ptr as usize..text_raw_ptr as usize + code.len()].copy_from_slice(&code);
    pe[reloc_raw_ptr as usize..reloc_raw_ptr as usize + reloc.len()].copy_from_slice(&reloc);
    pe[idata_raw_ptr as usize..idata_raw_ptr as usize + idata.len()].copy_from_slice(&idata);
    std::fs::write(path, &pe).expect("write pe-hello.exe");
}

/// A real on-disk PE32+ DLL for the `C:\Windows\System32` loader path. Exports
/// `thos_add(a, b) -> a + b`, which along the way calls its own imported
/// `KERNEL32.dll!GetLastError` — so loading it exercises **recursive** import
/// resolution. `DYNAMIC_BASE` + a minimal `.reloc` force the DLL relocation
/// path (the loader places it in its arena, not at the preferred `ImageBase`).
fn write_thoscrt_dll(path: &Path) {
    const IMAGE_BASE: u64 = 0x1_8000_0000;
    const SECT_ALIGN: u32 = 0x1000;
    const FILE_ALIGN: u32 = 0x200;
    let text_rva = 0x1000u32;
    let rdata_rva = 0x2000u32;
    let reloc_rva = 0x3000u32;

    let p32 = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let p64 = |b: &mut [u8], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());

    // --- .rdata: import table (KERNEL32!GetLastError) + export table --------
    let mut rdata: Vec<u8> = Vec::new();
    rdata.resize(40, 0); // 2 * 20-byte IMPORT_DESCRIPTOR (2nd = null terminator)
    let ilt_off = rdata.len() as u32;
    rdata.resize(rdata.len() + 16, 0); // ILT: 1 thunk + null
    let iat_off = rdata.len() as u32;
    rdata.resize(rdata.len() + 16, 0); // IAT: 1 thunk + null
    let hint_off = rdata.len() as u32;
    rdata.extend_from_slice(&[0, 0]); // hint
    rdata.extend_from_slice(b"GetLastError\0");
    if rdata.len() % 2 != 0 {
        rdata.push(0);
    }
    let dllname_off = rdata.len() as u32;
    rdata.extend_from_slice(b"KERNEL32.dll\0");
    while rdata.len() % 4 != 0 {
        rdata.push(0);
    }
    p32(&mut rdata, 0, rdata_rva + ilt_off); // OriginalFirstThunk
    p32(&mut rdata, 12, rdata_rva + dllname_off); // Name
    p32(&mut rdata, 16, rdata_rva + iat_off); // FirstThunk
    p64(&mut rdata, ilt_off as usize, (rdata_rva + hint_off) as u64);
    p64(&mut rdata, iat_off as usize, (rdata_rva + hint_off) as u64);
    let import_dir_rva = rdata_rva; // IDT starts at rdata+0
    let iat_dir_rva = rdata_rva + iat_off;
    let iat_gle_rva = rdata_rva + iat_off; // the one imported slot

    // Three exports: thos_add (ord 1, by name), thos_mul (ord 2 — pe-hello
    // imports it *by ordinal*), thos_fwd (ord 3 — a **forwarder** to
    // KERNEL32.GetProcessHeap). EAT[1] is back-patched once thos_mul's .text
    // offset is known.
    let exp_dir_off = rdata.len() as u32;
    rdata.resize(rdata.len() + 40, 0); // IMAGE_EXPORT_DIRECTORY
    let eat_off = rdata.len() as u32;
    rdata.resize(rdata.len() + 12, 0); // AddressOfFunctions[3]
    let enpt_off = rdata.len() as u32;
    rdata.resize(rdata.len() + 12, 0); // AddressOfNames[3]
    let ord_off = rdata.len() as u32;
    rdata.resize(rdata.len() + 6, 0); // AddressOfNameOrdinals[3]
    if rdata.len() % 2 != 0 {
        rdata.push(0);
    }
    let expname_off = rdata.len() as u32;
    rdata.extend_from_slice(b"thos_add\0");
    let expname2_off = rdata.len() as u32;
    rdata.extend_from_slice(b"thos_mul\0");
    let expname3_off = rdata.len() as u32;
    rdata.extend_from_slice(b"thos_fwd\0");
    let expmod_off = rdata.len() as u32;
    rdata.extend_from_slice(b"thoscrt.dll\0");
    // The forwarder target string, placed *inside* the export-directory span so
    // the loader recognises EAT[2] as a forwarder RVA.
    let fwd_str_off = rdata.len() as u32;
    rdata.extend_from_slice(b"KERNEL32.GetProcessHeap\0");
    while rdata.len() % 4 != 0 {
        rdata.push(0);
    }
    let export_dir_rva = rdata_rva + exp_dir_off;
    let export_dir_size = rdata.len() as u32 - exp_dir_off;
    p32(&mut rdata, exp_dir_off as usize + 0x0C, rdata_rva + expmod_off); // Name
    p32(&mut rdata, exp_dir_off as usize + 0x10, 1); // Base (ordinal base)
    p32(&mut rdata, exp_dir_off as usize + 0x14, 3); // NumberOfFunctions
    p32(&mut rdata, exp_dir_off as usize + 0x18, 3); // NumberOfNames
    p32(&mut rdata, exp_dir_off as usize + 0x1C, rdata_rva + eat_off);
    p32(&mut rdata, exp_dir_off as usize + 0x20, rdata_rva + enpt_off);
    p32(&mut rdata, exp_dir_off as usize + 0x24, rdata_rva + ord_off);
    p32(&mut rdata, eat_off as usize, text_rva); // EAT[0] = thos_add = start of .text
    p32(&mut rdata, eat_off as usize + 8, rdata_rva + fwd_str_off); // EAT[2] = forwarder RVA
    p32(&mut rdata, enpt_off as usize, rdata_rva + expname_off);
    p32(&mut rdata, enpt_off as usize + 4, rdata_rva + expname2_off);
    p32(&mut rdata, enpt_off as usize + 8, rdata_rva + expname3_off);
    rdata[ord_off as usize..ord_off as usize + 2].copy_from_slice(&0u16.to_le_bytes()); // name[0] -> EAT[0]
    rdata[ord_off as usize + 2..ord_off as usize + 4].copy_from_slice(&1u16.to_le_bytes()); // name[1] -> EAT[1]
    rdata[ord_off as usize + 4..ord_off as usize + 6].copy_from_slice(&2u16.to_le_bytes()); // name[2] -> EAT[2]

    // A writable slot DllMain(DLL_PROCESS_ATTACH) sets to 1; thos_add refuses
    // to compute unless it is set, so `thos_add(40,2) == 42` also proves the
    // loader ran DllMain before the exe entry.
    while rdata.len() % 4 != 0 {
        rdata.push(0);
    }
    let sentinel_rva = rdata_rva + rdata.len() as u32;
    rdata.resize(rdata.len() + 4, 0);

    // --- .text: thos_add first (its RVA is the exported address), then DllMain.
    let mut code: Vec<u8> = Vec::new();

    // thos_add(rcx=a, rdx=b): call imported GetLastError, then — only if the
    // DllMain sentinel is set — return a+b, else return 0.
    code.extend_from_slice(&[0x51]); // push rcx
    code.extend_from_slice(&[0x52]); // push rdx
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    let call_pos = code.len();
    code.extend_from_slice(&[0xFF, 0x15, 0, 0, 0, 0]); // call [rip+GetLastError]
    let disp = iat_gle_rva as i64 - (text_rva as i64 + call_pos as i64 + 6);
    code[call_pos + 2..call_pos + 6].copy_from_slice(&(disp as i32).to_le_bytes());
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x5A]); // pop rdx
    code.extend_from_slice(&[0x59]); // pop rcx
    let cmp_pos = code.len();
    code.extend_from_slice(&[0x83, 0x3D, 0, 0, 0, 0, 0x00]); // cmp dword [rip+sentinel], 0
    let cmp_disp = sentinel_rva as i64 - (text_rva as i64 + cmp_pos as i64 + 7);
    code[cmp_pos + 2..cmp_pos + 6].copy_from_slice(&(cmp_disp as i32).to_le_bytes());
    code.extend_from_slice(&[0x74, 0x04]); // je .fail (+4)
    code.extend_from_slice(&[0x8D, 0x04, 0x11]); // lea eax, [rcx+rdx]
    code.extend_from_slice(&[0xC3]); // ret
    code.extend_from_slice(&[0x31, 0xC0]); // .fail: xor eax, eax
    code.extend_from_slice(&[0xC3]); // ret

    // DllMain(rcx=hinst, edx=fdwReason, r8=lpvReserved) -> BOOL
    let dllmain_off = code.len() as u32;
    code.extend_from_slice(&[0x83, 0xFA, 0x01]); // cmp edx, 1  (DLL_PROCESS_ATTACH)
    code.extend_from_slice(&[0x75, 0x0A]); // jne .skip (+10)
    let movm_pos = code.len();
    code.extend_from_slice(&[0xC7, 0x05, 0, 0, 0, 0, 0x01, 0, 0, 0]); // mov dword [rip+sentinel], 1
    let movm_disp = sentinel_rva as i64 - (text_rva as i64 + movm_pos as i64 + 10);
    code[movm_pos + 2..movm_pos + 6].copy_from_slice(&(movm_disp as i32).to_le_bytes());
    code.extend_from_slice(&[0xB8, 0x01, 0, 0, 0]); // .skip: mov eax, 1  (TRUE)
    code.extend_from_slice(&[0xC3]); // ret

    // thos_mul(rcx=a, rdx=b) -> a*b — pe-hello imports this one by ordinal (2).
    let thos_mul_off = code.len() as u32;
    code.extend_from_slice(&[0x89, 0xC8]); // mov eax, ecx
    code.extend_from_slice(&[0x0F, 0xAF, 0xC2]); // imul eax, edx
    code.extend_from_slice(&[0xC3]); // ret
    p32(&mut rdata, (eat_off + 4) as usize, text_rva + thos_mul_off); // EAT[1] = thos_mul

    // --- .reloc: one header-only block — no fixups, but its presence makes
    //     the loader take the DLL relocation path. ---
    let mut reloc: Vec<u8> = Vec::new();
    reloc.extend_from_slice(&text_rva.to_le_bytes()); // PageRVA
    reloc.extend_from_slice(&8u32.to_le_bytes()); // BlockSize (header only)

    // --- PE32+ container ---
    let text_vsize = code.len() as u32;
    let rdata_vsize = rdata.len() as u32;
    let reloc_vsize = reloc.len() as u32;
    let text_raw_ptr = 0x200u32;
    let text_raw_size = text_vsize.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let rdata_raw_ptr = text_raw_ptr + text_raw_size;
    let rdata_raw_size = rdata_vsize.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let reloc_raw_ptr = rdata_raw_ptr + rdata_raw_size;
    let reloc_raw_size = reloc_vsize.div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let size_of_image = reloc_rva + reloc_vsize.div_ceil(SECT_ALIGN) * SECT_ALIGN;
    let size_of_headers = 0x200u32;

    let mut pe = vec![0u8; (reloc_raw_ptr + reloc_raw_size) as usize];
    pe[0..2].copy_from_slice(b"MZ");
    pe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    let o = 0x40usize;
    pe[o..o + 4].copy_from_slice(b"PE\0\0");
    let coff = o + 4;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[coff + 2..coff + 4].copy_from_slice(&3u16.to_le_bytes()); // 3 sections
    let opt_size = 0xF0u16;
    pe[coff + 16..coff + 18].copy_from_slice(&opt_size.to_le_bytes());
    pe[coff + 18..coff + 20].copy_from_slice(&0x2022u16.to_le_bytes()); // EXECUTABLE|LAA|DLL
    let opt = coff + 20;
    pe[opt..opt + 2].copy_from_slice(&0x020Bu16.to_le_bytes()); // PE32+
    pe[opt + 16..opt + 20].copy_from_slice(&(text_rva + dllmain_off).to_le_bytes()); // AddressOfEntryPoint = DllMain
    pe[opt + 20..opt + 24].copy_from_slice(&text_rva.to_le_bytes());
    pe[opt + 24..opt + 32].copy_from_slice(&IMAGE_BASE.to_le_bytes());
    pe[opt + 32..opt + 36].copy_from_slice(&SECT_ALIGN.to_le_bytes());
    pe[opt + 36..opt + 40].copy_from_slice(&FILE_ALIGN.to_le_bytes());
    pe[opt + 40..opt + 42].copy_from_slice(&6u16.to_le_bytes());
    pe[opt + 48..opt + 50].copy_from_slice(&6u16.to_le_bytes());
    pe[opt + 56..opt + 60].copy_from_slice(&size_of_image.to_le_bytes());
    pe[opt + 60..opt + 64].copy_from_slice(&size_of_headers.to_le_bytes());
    pe[opt + 68..opt + 70].copy_from_slice(&3u16.to_le_bytes()); // Subsystem
    pe[opt + 70..opt + 72].copy_from_slice(&0x0040u16.to_le_bytes()); // DllCharacteristics = DYNAMIC_BASE
    pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
    p32(&mut pe, opt + 112, export_dir_rva); // dir 0 EXPORT
    p32(&mut pe, opt + 112 + 4, export_dir_size);
    p32(&mut pe, opt + 112 + 8, import_dir_rva); // dir 1 IMPORT
    p32(&mut pe, opt + 112 + 12, 40);
    p32(&mut pe, opt + 112 + 5 * 8, reloc_rva); // dir 5 BASE_RELOC
    p32(&mut pe, opt + 112 + 5 * 8 + 4, reloc_vsize);
    p32(&mut pe, opt + 112 + 12 * 8, iat_dir_rva); // dir 12 IAT
    p32(&mut pe, opt + 112 + 12 * 8 + 4, 8);

    let mut sec = |i: usize, name: &[u8], vsize: u32, rva: u32, raw_size: u32, raw_ptr: u32, ch: u32| {
        let h = opt + opt_size as usize + i * 40;
        pe[h..h + name.len()].copy_from_slice(name);
        pe[h + 8..h + 12].copy_from_slice(&vsize.to_le_bytes());
        pe[h + 12..h + 16].copy_from_slice(&rva.to_le_bytes());
        pe[h + 16..h + 20].copy_from_slice(&raw_size.to_le_bytes());
        pe[h + 20..h + 24].copy_from_slice(&raw_ptr.to_le_bytes());
        pe[h + 36..h + 40].copy_from_slice(&ch.to_le_bytes());
    };
    sec(0, b".text", text_vsize, text_rva, text_raw_size, text_raw_ptr, 0x6000_0020); // CODE|EXEC|READ
    sec(1, b".rdata", rdata_vsize, rdata_rva, rdata_raw_size, rdata_raw_ptr, 0xC000_0040); // IDATA|READ|WRITE
    sec(2, b".reloc", reloc_vsize, reloc_rva, reloc_raw_size, reloc_raw_ptr, 0x4200_0040);

    pe[text_raw_ptr as usize..text_raw_ptr as usize + code.len()].copy_from_slice(&code);
    pe[rdata_raw_ptr as usize..rdata_raw_ptr as usize + rdata.len()].copy_from_slice(&rdata);
    pe[reloc_raw_ptr as usize..reloc_raw_ptr as usize + reloc.len()].copy_from_slice(&reloc);
    std::fs::write(path, &pe).expect("write thoscrt.dll");
}

/// Assemble a minimal PE32+ (`.exe` or `.dll`) from parts. `sections` are
/// `(name, rva, characteristics, bytes)`; `dirs` are `(index, rva, size)` data
/// directory entries. With `reloc_stub`, a header-only `.reloc` section +
/// `DYNAMIC_BASE` are appended (0 fixups — the caller's content must be
/// position-independent), so the loader will accept the image at any base.
fn write_min_pe(
    path: &Path,
    is_dll: bool,
    image_base: u64,
    entry_rva: u32,
    sections: &[(&[u8], u32, u32, &[u8])],
    dirs: &[(usize, u32, u32)],
    reloc_stub: bool,
) {
    const SECT_ALIGN: u32 = 0x1000;
    const FILE_ALIGN: u32 = 0x200;

    let text_rva = sections[0].1;
    let reloc_rva = sections
        .iter()
        .map(|(_, rva, _, d)| rva + (d.len() as u32).div_ceil(SECT_ALIGN) * SECT_ALIGN)
        .max()
        .unwrap_or(SECT_ALIGN);
    let reloc_body: Vec<u8> = {
        let mut v = Vec::new();
        v.extend_from_slice(&text_rva.to_le_bytes()); // PageRVA
        v.extend_from_slice(&8u32.to_le_bytes()); // BlockSize (header only)
        v
    };
    let mut secs: Vec<(&[u8], u32, u32, &[u8])> = sections.to_vec();
    if reloc_stub {
        secs.push((b".reloc", reloc_rva, 0x4200_0040, &reloc_body));
    }
    let sections = &secs[..];

    let opt_size = 0xF0usize;
    let hdr = 0x40 + 4 + 20 + opt_size + sections.len() * 40;
    let size_of_headers = (hdr as u32).div_ceil(FILE_ALIGN) * FILE_ALIGN;

    let mut raw_ptr = size_of_headers;
    let mut layout: Vec<(u32, u32, u32)> = Vec::new(); // (raw_ptr, raw_size, vsize)
    for (_, _, _, data) in sections {
        let rs = (data.len() as u32).div_ceil(FILE_ALIGN) * FILE_ALIGN;
        layout.push((raw_ptr, rs, data.len() as u32));
        raw_ptr += rs;
    }
    let size_of_image = sections
        .iter()
        .map(|(_, rva, _, d)| rva + (d.len() as u32).div_ceil(SECT_ALIGN) * SECT_ALIGN)
        .max()
        .unwrap_or(SECT_ALIGN);

    let mut pe = vec![0u8; raw_ptr as usize];
    pe[0..2].copy_from_slice(b"MZ");
    pe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    let o = 0x40usize;
    pe[o..o + 4].copy_from_slice(b"PE\0\0");
    let coff = o + 4;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[coff + 2..coff + 4].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    pe[coff + 16..coff + 18].copy_from_slice(&(opt_size as u16).to_le_bytes());
    let chars: u16 = if is_dll { 0x2022 } else { 0x0022 }; // EXECUTABLE|LAA (+DLL)
    pe[coff + 18..coff + 20].copy_from_slice(&chars.to_le_bytes());
    let opt = coff + 20;
    pe[opt..opt + 2].copy_from_slice(&0x020Bu16.to_le_bytes());
    pe[opt + 16..opt + 20].copy_from_slice(&entry_rva.to_le_bytes());
    pe[opt + 24..opt + 32].copy_from_slice(&image_base.to_le_bytes());
    pe[opt + 32..opt + 36].copy_from_slice(&SECT_ALIGN.to_le_bytes());
    pe[opt + 36..opt + 40].copy_from_slice(&FILE_ALIGN.to_le_bytes());
    pe[opt + 40..opt + 42].copy_from_slice(&6u16.to_le_bytes());
    pe[opt + 48..opt + 50].copy_from_slice(&6u16.to_le_bytes());
    pe[opt + 56..opt + 60].copy_from_slice(&size_of_image.to_le_bytes());
    pe[opt + 60..opt + 64].copy_from_slice(&size_of_headers.to_le_bytes());
    pe[opt + 68..opt + 70].copy_from_slice(&3u16.to_le_bytes()); // Subsystem
    if reloc_stub {
        pe[opt + 70..opt + 72].copy_from_slice(&0x0040u16.to_le_bytes()); // DYNAMIC_BASE
    }
    pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
    let mut dirs = dirs.to_vec();
    if reloc_stub {
        dirs.push((5, reloc_rva, reloc_body.len() as u32));
    }
    for &(idx, rva, size) in &dirs {
        pe[opt + 112 + idx * 8..opt + 112 + idx * 8 + 4].copy_from_slice(&rva.to_le_bytes());
        pe[opt + 112 + idx * 8 + 4..opt + 112 + idx * 8 + 8].copy_from_slice(&size.to_le_bytes());
    }
    for (i, (name, rva, sc, data)) in sections.iter().enumerate() {
        let h = opt + opt_size + i * 40;
        pe[h..h + name.len()].copy_from_slice(name);
        pe[h + 8..h + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        pe[h + 12..h + 16].copy_from_slice(&rva.to_le_bytes());
        pe[h + 16..h + 20].copy_from_slice(&layout[i].1.to_le_bytes());
        pe[h + 20..h + 24].copy_from_slice(&layout[i].0.to_le_bytes());
        pe[h + 36..h + 40].copy_from_slice(&sc.to_le_bytes());
        pe[layout[i].0 as usize..layout[i].0 as usize + data.len()].copy_from_slice(data);
    }
    std::fs::write(path, &pe).expect("write min pe");
}

/// `failcrt.dll` — exports `fc_dummy`, and its `DllMain(DLL_PROCESS_ATTACH)`
/// returns **FALSE**. Loaded at its preferred base (no relocations).
fn write_failcrt_dll(path: &Path) {
    const IB: u64 = 0x1_9000_0000;
    let text_rva = 0x1000u32;
    let rdata_rva = 0x2000u32;
    let p32 = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());

    // .text: fc_dummy at 0, DllMain after it.
    let mut text: Vec<u8> = Vec::new();
    text.extend_from_slice(&[0x31, 0xC0, 0xC3]); // fc_dummy: xor eax,eax ; ret
    let dllmain_off = text.len() as u32;
    text.extend_from_slice(&[0x83, 0xFA, 0x01]); // cmp edx, 1
    text.extend_from_slice(&[0x75, 0x03]); // jne .ok
    text.extend_from_slice(&[0x31, 0xC0, 0xC3]); // xor eax,eax ; ret   (FALSE)
    text.extend_from_slice(&[0xB8, 0x01, 0, 0, 0]); // .ok: mov eax, 1
    text.extend_from_slice(&[0xC3]); // ret

    // .rdata: export directory with one name, fc_dummy -> EAT[0] = text_rva.
    let mut rd = vec![0u8; 40];
    let eat_off = rd.len() as u32;
    rd.extend_from_slice(&[0u8; 4]);
    let enpt_off = rd.len() as u32;
    rd.extend_from_slice(&[0u8; 4]);
    let ord_off = rd.len() as u32;
    rd.extend_from_slice(&[0u8; 2]);
    let name_off = rd.len() as u32;
    rd.extend_from_slice(b"fc_dummy\0");
    let mod_off = rd.len() as u32;
    rd.extend_from_slice(b"failcrt.dll\0");
    while rd.len() % 4 != 0 {
        rd.push(0);
    }
    p32(&mut rd, 0x0C, rdata_rva + mod_off);
    p32(&mut rd, 0x10, 1);
    p32(&mut rd, 0x14, 1);
    p32(&mut rd, 0x18, 1);
    p32(&mut rd, 0x1C, rdata_rva + eat_off);
    p32(&mut rd, 0x20, rdata_rva + enpt_off);
    p32(&mut rd, 0x24, rdata_rva + ord_off);
    p32(&mut rd, eat_off as usize, text_rva);
    p32(&mut rd, enpt_off as usize, rdata_rva + name_off);
    let exp_size = rd.len() as u32;

    write_min_pe(
        path,
        true,
        IB,
        text_rva + dllmain_off,
        &[
            (b".text", text_rva, 0x6000_0020, &text),
            (b".rdata", rdata_rva, 0x4000_0040, &rd),
        ],
        &[(0, rdata_rva, exp_size)],
        true,
    );
}

/// `pe-dllfail.exe` — imports `failcrt.dll!fc_dummy` (so the DLL loads and its
/// FALSE `DllMain` runs). Its entry prints "PE DLLFAIL REACHED ENTRY" via raw
/// Linux syscalls and exits — the line only appears if init was *not* aborted.
fn write_pe_dllfail(path: &Path) {
    const IB: u64 = 0x1_4000_0000;
    let text_rva = 0x1000u32;
    let idata_rva = 0x2000u32;
    let p32 = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let p64 = |b: &mut [u8], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());

    // .idata: one IMPORT_DESCRIPTOR for failcrt.dll, one thunk (fc_dummy).
    let mut id = vec![0u8; 40]; // IDT[0] + null
    let ilt_off = id.len() as u32;
    id.extend_from_slice(&[0u8; 16]);
    let iat_off = id.len() as u32;
    id.extend_from_slice(&[0u8; 16]);
    let hint_off = id.len() as u32;
    id.extend_from_slice(&[0, 0]);
    id.extend_from_slice(b"fc_dummy\0");
    if id.len() % 2 != 0 {
        id.push(0);
    }
    let dll_off = id.len() as u32;
    id.extend_from_slice(b"failcrt.dll\0");
    while id.len() % 4 != 0 {
        id.push(0);
    }
    p32(&mut id, 0, idata_rva + ilt_off);
    p32(&mut id, 12, idata_rva + dll_off);
    p32(&mut id, 16, idata_rva + iat_off);
    p64(&mut id, ilt_off as usize, (idata_rva + hint_off) as u64);
    p64(&mut id, iat_off as usize, (idata_rva + hint_off) as u64);

    // .text: write(1, msg, len) ; exit_group(0)  — RIP-relative, no relocs.
    let msg: &[u8] = b"PE DLLFAIL REACHED ENTRY\n";
    let mut text: Vec<u8> = Vec::new();
    text.extend_from_slice(&[0xB8, 1, 0, 0, 0]); // mov eax, 1  (write)
    text.extend_from_slice(&[0xBF, 1, 0, 0, 0]); // mov edi, 1
    let lea_at = text.len();
    text.extend_from_slice(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]); // lea rsi, [rip+msg]
    text.extend_from_slice(&[0xBA]);
    text.extend_from_slice(&(msg.len() as u32).to_le_bytes()); // mov edx, len
    text.extend_from_slice(&[0x0F, 0x05]); // syscall
    text.extend_from_slice(&[0xB8, 231, 0, 0, 0]); // mov eax, 231 (exit_group)
    text.extend_from_slice(&[0x31, 0xFF]); // xor edi, edi
    text.extend_from_slice(&[0x0F, 0x05]); // syscall
    let msg_off = text.len();
    text.extend_from_slice(msg);
    let disp = msg_off as i64 - (lea_at as i64 + 7);
    text[lea_at + 3..lea_at + 7].copy_from_slice(&(disp as i32).to_le_bytes());

    write_min_pe(
        path,
        false,
        IB,
        text_rva,
        &[
            (b".text", text_rva, 0x6000_0020, &text),
            (b".idata", idata_rva, 0xC000_0040, &id),
        ],
        &[(1, idata_rva, 40)],
        false,
    );
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
    let gui = std::env::args().any(|a| a == "--gui");
    qemu.args(["-display", if gui { "gtk" } else { "none" }, "-no-reboot"]);
    qemu.args(["-serial", &format!("file:{}", log.to_str().unwrap())]);
    qemu.args(["-monitor", &format!("unix:{},server,nowait", sock.to_str().unwrap())]);
    for ovmf in ["/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/ovmf/OVMF.fd"] {
        if Path::new(ovmf).exists() {
            qemu.args(["-drive", &format!("if=pflash,format=raw,readonly=on,file={ovmf}")]);
            break;
        }
    }
    let child = qemu.spawn().expect("spawn qemu");

    // Follow the serial log and stream new lines to this terminal so the run is
    // visible (the monitor drives the keyboard, so the serial can't be stdio).
    {
        let log = log.clone();
        let tag = tag.to_string();
        let pid = child.id();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Seek, SeekFrom};
            let mut pos: u64 = 0;
            loop {
                if let Ok(mut f) = std::fs::File::open(&log) {
                    let _ = f.seek(SeekFrom::Start(pos));
                    for line in BufReader::new(&f).lines().map_while(Result::ok) {
                        println!("  {tag} │ {line}");
                    }
                    pos = f.stream_position().unwrap_or(pos);
                }
                // stop once the qemu process is gone
                if std::fs::read(format!("/proc/{pid}/stat")).is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
    }
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
        // QEMU `sendkey` wants key *names*, not glyphs, for non-alphanumerics.
        // Key *names* / chords for QEMU `sendkey`. The kernel console maps
        // scancodes through a **German (QWERTZ)** layout, so `/` is `shift-7`
        // and the US `/`-key (`slash`) would actually produce `-`.
        let key = match c {
            ' ' => "spc",
            '/' => "shift-7",
            '-' => "slash",
            '_' => "shift-slash",
            '.' => "dot",
            ',' => "comma",
            '|' => "altgr-less", // DE: AltGr + the key left of Y
            '$' => "shift-4",
            '(' => "shift-8",
            ')' => "shift-9",
            _ => {
                mon(sock, &format!("sendkey {c}"));
                continue;
            }
        };
        mon(sock, &format!("sendkey {key}"));
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
        if !wait_for(log, "THOS login:", 90) {
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

    if !wait_for(&log, "THOS first-run setup", 90) {
        kill(&mut child, "kbd-test", "kernel never reached first-run setup", &log);
    }
    drive_login(&sock, &log, &mut child, "kbd-test");

    if !wait_for(&log, "interactive hold", 90) {
        kill(&mut child, "kbd-test", "never reached the shell after login", &log);
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    type_line(&sock, "init");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // BusyBox applet links reached via `/bin/*` hard-links to `/busybox`, plus a
    // per-process cwd: `cd /bin` then a bare `ls` / `pwd` must act on /bin.
    type_line(&sock, "cd /bin");
    std::thread::sleep(std::time::Duration::from_millis(600));
    type_line(&sock, "pwd");
    std::thread::sleep(std::time::Duration::from_millis(800));
    type_line(&sock, "ls");
    std::thread::sleep(std::time::Duration::from_millis(2000));
    type_line(&sock, "cat /message");
    // Wait for the last command's output rather than a fixed sleep — the host
    // running CI can be slow enough that a 2 MiB BusyBox applet takes seconds.
    let _ = wait_for(&log, "hello a file read via open+lseek+read", 30);

    let out = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();

    let after = out.split("interactive hold").nth(1).unwrap_or("");
    let shell_ok = after.contains("thos$ init") && after.contains("parent done");
    // bare `ls` after `cd /bin` must list the applet link names.
    let ls_ok = ["busybox", "touch", "grep", "sleep"].iter().all(|n| after.contains(n));
    // `pwd` prints the cwd we chdir'd into.
    let cwd_ok = after.contains("\n/bin\n");
    let cat_ok = after.contains("hello a file read via open+lseek+read");
    if shell_ok && ls_ok && cwd_ok && cat_ok {
        println!("kbd-test: OK — `init`, BusyBox applets, per-process cwd (cd/pwd/ls)");
    } else {
        eprintln!(
            "kbd-test: FAIL (shell_ok={shell_ok} ls_ok={ls_ok} cwd_ok={cwd_ok} cat_ok={cat_ok})\n---\n{after}\n---"
        );
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
    if !wait_for(&log1, "THOS first-run setup", 90) {
        kill(&mut c1, "login-test", "boot 1 showed no first-run setup", &log1);
    }
    drive_login(&sock1, &log1, &mut c1, "login-test");
    let ok1 = wait_for(&log1, "interactive hold", 90);
    let _ = c1.kill();
    let _ = c1.wait();
    if !ok1 {
        eprintln!("login-test: FAIL — boot 1 never reached the shell");
        exit(1);
    }

    // Boot 2 — same disk: straight to login, no setup; reject a wrong password.
    let (mut c2, log2, sock2) = spawn_interactive_qemu("login2", iso, &disk);
    if !wait_for(&log2, "THOS login:", 90) {
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
    let ok2 = wait_for(&log2, "interactive hold", 90);
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
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let log = workspace_root().join(format!("target/{tag}-serial.log"));
    // The kernel serial is streamed to this terminal live *and* captured for the
    // assertions *and* written to the log file. `cargo xtask <test> --gui` also
    // opens a QEMU window so the framebuffer is visible; otherwise `tail -f
    // target/<tag>-serial.log` in another shell follows a run.
    let gui = std::env::args().any(|a| a == "--gui");

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args(["-M", "q35", "-m", "512M", "-smp", &smp.to_string(), "-cdrom", iso.to_str().unwrap()]);
    qemu.args([
        "-drive", &format!("id=disk0,if=none,format=raw,file={}", disk.to_str().unwrap()),
        "-device", "ahci,id=ahci0", "-device", "ide-hd,drive=disk0,bus=ahci0.0",
    ]);
    qemu.args(["-display", if gui { "gtk" } else { "none" }, "-no-reboot"]);
    qemu.args(["-serial", "stdio", "-monitor", "none"]);
    qemu.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    for ovmf in ["/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/ovmf/OVMF.fd"] {
        if Path::new(ovmf).exists() {
            qemu.args(["-drive", &format!("if=pflash,format=raw,readonly=on,file={ovmf}")]);
            break;
        }
    }
    qemu.stdin(std::process::Stdio::null());
    qemu.stdout(std::process::Stdio::piped());

    let mut child = qemu.spawn().expect("spawn qemu");
    let out = child.stdout.take().expect("qemu stdout");
    let serial: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let reader = {
        let serial = Arc::clone(&serial);
        let log = log.clone();
        let tag = tag.to_string();
        std::thread::spawn(move || {
            let mut logf = std::fs::File::create(&log).ok();
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                println!("  {tag} │ {line}");
                if let Some(f) = logf.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
                let mut s = serial.lock().unwrap();
                s.push_str(&line);
                s.push('\n');
            }
        })
    };

    let deadline = Instant::now() + Duration::from_secs(240);
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
    let _ = reader.join();

    let serial = Arc::try_unwrap(serial).unwrap().into_inner().unwrap();
    if status.and_then(|s| s.code()) != Some(QEMU_SUCCESS) {
        eprintln!("{tag}: kernel did not halt cleanly (see the stream above / target/{tag}-serial.log)");
        exit(1);
    }
    serial
}

/// Must match `SCRATCH_LBA` / the pattern in `kernel/src/main.rs`.
const AHCI_SCRATCH_LBA: u64 = 50_000;

fn ahci_test(iso: &Path) {
    use std::io::Read;

    let root = workspace_root();
    let _ = std::fs::remove_file(root.join("target/disk.img")); // fresh: scratch = zeros
    let disk = disk_image();

    let serial = boot_kernel_headless("ahci", iso, &disk, 4);
    for m in ["THOS: ahci ident", "THOS: ahci cap ok", "THOS: ahci ncq ok", "THOS: ahci write ok"] {
        if !serial.contains(m) {
            eprintln!("ahci-test: FAIL — missing marker {m:?}\n{serial}");
            exit(1);
        }
    }

    // IDENTIFY's sector count must match the backing file exactly.
    let want_sectors = std::fs::metadata(&disk).unwrap().len() / 512;
    let got_sectors: u64 = serial
        .lines()
        .find(|l| l.contains("ahci ident"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.parse().ok()))
        .unwrap_or(0);
    if got_sectors != want_sectors {
        eprintln!("ahci-test: FAIL — IDENTIFY reported {got_sectors} sectors, file has {want_sectors}");
        exit(1);
    }

    // The completion path must be interrupt-driven, not the timer safety net:
    // the "ahci ncq ok" line reports how many completion IRQs were taken.
    let irqs: u64 = serial
        .lines()
        .find(|l| l.contains("ahci ncq ok"))
        .and_then(|l| l.rsplit_once(", ").and_then(|(_, r)| r.split_whitespace().next()?.parse().ok()))
        .unwrap_or(0);
    if !serial.contains("MSI") || irqs == 0 {
        eprintln!("ahci-test: FAIL — no MSI completion interrupts (irqs={irqs})\n{serial}");
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
            "ahci-test: OK — IDENTIFY {want_sectors} sectors; LBA {AHCI_SCRATCH_LBA} round-tripped + persisted"
        );
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
    for m in ["THOS: ext2 write ok", "THOS: ext2 unlink ok"] {
        if !serial.contains(m) {
            eprintln!("ext2-test: FAIL — missing marker {m:?}\n{serial}");
            exit(1);
        }
    }

    // The filesystem must still be consistent after the writes + deletes, via
    // the primary superblock and the group-1 backup (`sync_backups`).
    for sb in [None, Some("8193")] {
        let mut c = Command::new("e2fsck");
        c.arg("-fn");
        if let Some(b) = sb {
            c.args(["-b", b]);
        }
        let out = c.arg(disk.to_str().unwrap()).output().expect("run e2fsck");
        if !out.status.success() {
            eprintln!(
                "ext2-test: FAIL — e2fsck ({}) reported problems (exit {:?})\n{}",
                sb.map_or("primary", |b| b),
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
            );
            exit(1);
        }
    }

    let cat = |path: &str| -> String {
        let o = Command::new("debugfs")
            .args(["-R", &format!("cat {path}"), disk.to_str().unwrap()])
            .output()
            .expect("run debugfs");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    // `/thos-created.txt` is never deleted; `/thos-temp.txt` + `/thosdir` are.
    let survivor = cat("/thos-created.txt");
    let deleted = cat("/thos-temp.txt");
    if survivor.contains("ext2 write works on THOS") && !deleted.contains("delete me") {
        println!("ext2-test: OK — e2fsck clean (primary + backup); create + unlink/rmdir verified");
    } else {
        eprintln!("ext2-test: FAIL — survivor={survivor:?} deleted-still-there={deleted:?}");
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

/// Boot with QEMU `blkdebug` poisoning one read of LBA 41000, so the kernel's
/// NCQ error-recovery path runs: the read must fail cleanly (no hang / panic),
/// the port must recover, and the retry must succeed.
fn ncq_error_test(iso: &Path) {
    use std::time::{Duration, Instant};

    let root = workspace_root();
    let _ = std::fs::remove_file(root.join("target/disk.img"));
    let disk = disk_image();
    let cfg = root.join("target/ncq-fault.conf");
    std::fs::write(
        &cfg,
        "[inject-error]\nevent = \"read_aio\"\nerrno = \"5\"\nonce = \"on\"\nsector = \"41000\"\n",
    )
    .unwrap();
    let log = root.join("target/ncq-serial.log");
    let _ = std::fs::remove_file(&log);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args(["-M", "q35", "-m", "512M", "-smp", "4", "-cdrom", iso.to_str().unwrap()]);
    qemu.args([
        "-drive",
        &format!(
            "if=none,id=disk0,format=raw,file=blkdebug:{}:{}",
            cfg.to_str().unwrap(),
            disk.to_str().unwrap()
        ),
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
    let deadline = Instant::now() + Duration::from_secs(240);
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
        eprintln!("ncq-error-test: FAIL — kernel did not halt cleanly (hang / panic on the poisoned read)\n--- serial ---\n{serial}\n---");
        exit(1);
    }
    if serial.contains("THOS: ncq error ok") && serial.contains("THOS: ahci recover") {
        let line = serial.lines().find(|l| l.contains("ncq error ok")).unwrap_or("");
        println!("ncq-error-test: OK — {}", line.trim());
    } else {
        eprintln!("ncq-error-test: FAIL — recovery markers missing\n--- serial ---\n{serial}\n---");
        exit(1);
    }
}

/// Boot a `bbtest` kernel and require the stock static BusyBox to run and print.
fn busybox_test(iso: &Path) {
    let disk = disk_image();
    let serial = boot_kernel_headless("busybox", iso, &disk, 4);
    if serial.contains("THOS: busybox ok") && serial.contains("THOS: busybox says hello") {
        println!("busybox-test: OK — stock static BusyBox `echo` ran unmodified");
    } else {
        eprintln!("busybox-test: FAIL — BusyBox did not run to a clean exit\n--- serial ---\n{serial}\n---");
        exit(1);
    }
}

fn fat_test(iso: &Path) {
    let disk = disk_image();
    let serial = boot_kernel_headless("fat", iso, &disk, 4);
    let ok = serial.contains("THOS: gpt ok")
        && serial.contains("THOS: fat ok")
        && serial.contains("THOS reads FAT");
    if ok {
        println!("fat-test: OK — GPT → ESP → FAT32, read /EFI/THOS/HELLO.TXT");
    } else {
        eprintln!("fat-test: FAIL — GPT/FAT read did not produce the file\n--- serial ---\n{serial}\n---");
        exit(1);
    }
}

fn pe_test(iso: &Path) {
    let disk = disk_image();
    let serial = boot_kernel_headless("pe", iso, &disk, 4);
    let ok = serial.contains("THOS: pe exited")
        && serial.contains("PE on THOS via native loader") // raw syscall + DIR64 reloc
        && serial.contains("PE via WriteFile") // GetStdHandle + WriteFile + gs/TEB/PEB
        && serial.contains("PE ReadFile OK via CreateFileA") // CreateFileA + ReadFile
        && serial.contains("PE ProcParams OK") // PEB->ProcessParameters->StandardOutput
        && serial.contains("PE Ldr OK") // PEB->Ldr module list walk
        && serial.contains("PE argv0 pe-hello.exe") // GetCommandLineA
        && serial.contains("PE VirtualAlloc+Heap OK") // VirtualAlloc + GetProcessHeap + HeapAlloc
        && serial.contains("PE GetProcAddress OK") // LoadLibraryA + synthetic kernel32 export table
        && serial.contains("PE ntdll OK") // ntdll boundary: GetModuleHandleA + GetProcAddress + 9-arg NtWriteFile
        && serial.contains("PE dll thos_add=42 (DllMain ran)") // System32 DLL + recursive imports + DllMain before exe entry
        && serial.contains("PE dll Ldr OK") // file DLL in PEB Ldr: GetModuleHandleA + GetProcAddress at runtime
        && serial.contains("PE dll ordinal OK") // import-by-ordinal from a file DLL
        && serial.contains("PE dll forward OK") // forwarder export (thoscrt -> KERNEL32.GetProcessHeap)
        && serial.contains("PE TLS OK") // static TLS: block copied, __tls_index written, callback ran
        && serial.contains("THOS: pe dllfail ok") // a DllMain returning FALSE aborted process init
        && !serial.contains("PE DLLFAIL REACHED ENTRY") // ...so the exe entry never ran
        && serial.contains("THOS: pe reject ok");
    if ok {
        println!("pe-test: OK — PE loader: reloc/imports/gs+TEB+PEB/Ldr+params/file I/O/VirtualAlloc+Heap/GetProcAddress/ntdll/System32-DLL (in Ldr)");
    } else {
        eprintln!("pe-test: FAIL — the PE did not run to exit\n--- serial ---\n{serial}\n---");
        exit(1);
    }
}

fn pipe_test(iso: &Path) {
    let disk = disk_image();
    let serial = boot_kernel_headless("pipe", iso, &disk, 4);
    // `echo THOS-PIPE $(ls /bin | grep -c sleep) sub-$(echo works)`:
    //   the `|` count is 1, the nested `$(…)` yields `works`.
    if serial.contains("THOS: pipe ok") && serial.contains("THOS-PIPE 1 sub-works") {
        println!("pipe-test: OK — `|` and `$(…)` work through BusyBox sh");
    } else {
        eprintln!("pipe-test: FAIL — pipe / command-substitution output wrong\n--- serial ---\n{serial}\n---");
        exit(1);
    }
}
