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

    // A hand-assembled, statically linked Win64 `.exe` (no imports, no relocs)
    // for the native PE loader -> /pe-hello.exe.
    let exe = root.join("target/pe-hello.exe");
    write_pe_hello(&exe);
    run(Command::new("debugfs").args([
        "-w", "-R", &format!("write {} pe-hello.exe", exe.to_str().unwrap()),
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
    const IMAGE_BASE: u64 = 0x1_4000_0000;
    const SECT_ALIGN: u32 = 0x1000;
    const FILE_ALIGN: u32 = 0x200;
    let text_rva = 0x1000u32;
    let reloc_rva = 0x2000u32;
    let idata_rva = 0x3000u32;

    // --- .idata: import KERNEL32.dll!{ExitProcess, GetStdHandle, WriteFile} ---
    let funcs: [&[u8]; 3] = [b"ExitProcess", b"GetStdHandle", b"WriteFile"];
    let ilt_off = 0x28u32; // after 2 IDT entries (2*20)
    let iat_off = ilt_off + (funcs.len() as u32 + 1) * 8;
    let mut hn_off = iat_off + (funcs.len() as u32 + 1) * 8;
    let mut idata: Vec<u8> = vec![0u8; 0x200];
    let put32 = |b: &mut Vec<u8>, at: u32, v: u32| {
        b[at as usize..at as usize + 4].copy_from_slice(&v.to_le_bytes());
    };
    let put64 = |b: &mut Vec<u8>, at: u32, v: u64| {
        b[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
    };
    let mut hn_rvas = [0u32; 3];
    for (i, f) in funcs.iter().enumerate() {
        hn_rvas[i] = idata_rva + hn_off;
        // 2-byte hint (0) then the NUL-terminated name
        idata[hn_off as usize + 2..hn_off as usize + 2 + f.len()].copy_from_slice(f);
        hn_off += 2 + f.len() as u32 + 1;
        hn_off = (hn_off + 1) & !1; // keep names 2-aligned
    }
    let dll_off = hn_off;
    idata[dll_off as usize..dll_off as usize + 12].copy_from_slice(b"KERNEL32.dll");
    // IDT entry 0 (entry 1 stays zero = terminator)
    put32(&mut idata, 0, idata_rva + ilt_off); // OriginalFirstThunk
    put32(&mut idata, 12, idata_rva + dll_off); // Name
    put32(&mut idata, 16, idata_rva + iat_off); // FirstThunk
    for i in 0..funcs.len() as u32 {
        put64(&mut idata, ilt_off + i * 8, hn_rvas[i as usize] as u64);
        put64(&mut idata, iat_off + i * 8, hn_rvas[i as usize] as u64);
    }
    idata.truncate(((dll_off + 13 + 15) & !15) as usize);
    let iat_exit = idata_rva + iat_off;
    let iat_gsh = idata_rva + iat_off + 8;
    let iat_wf = idata_rva + iat_off + 16;

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

    // 1) write(1, msg1, len1)
    code.extend_from_slice(&[0x48, 0xC7, 0xC0, 1, 0, 0, 0]); // mov rax, 1
    code.extend_from_slice(&[0x48, 0xC7, 0xC7, 1, 0, 0, 0]); // mov rdi, 1
    rel!([0x48, 0x8B, 0x35, 0, 0, 0, 0], ptr_slot_tag); // mov rsi, [rip+ptr_slot]
    code.extend_from_slice(&[0x48, 0xC7, 0xC2]);
    code.extend_from_slice(&(msg1.len() as u32).to_le_bytes()); // mov rdx, len1
    code.extend_from_slice(&[0x0F, 0x05]); // syscall

    // 2) WriteFile(GetStdHandle(-11), msg2, len2, &written, NULL)
    code.extend_from_slice(&[0xB9, 0xF5, 0xFF, 0xFF, 0xFF]); // mov ecx, -11
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_gsh); // call [rip+iat_GetStdHandle]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x48, 0x89, 0xC3]); // mov rbx, rax  (handle)
    code.extend_from_slice(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    rel!([0x48, 0x8D, 0x15, 0, 0, 0, 0], msg2_tag); // lea rdx, [rip+msg2]
    code.extend_from_slice(&[0x41, 0xB8]);
    code.extend_from_slice(&(msg2.len() as u32).to_le_bytes()); // mov r8d, len2
    rel!([0x4C, 0x8D, 0x0D, 0, 0, 0, 0], wr_slot_tag); // lea r9, [rip+written]
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // sub rsp, 0x38
    code.extend_from_slice(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]); // mov qword [rsp+0x20], 0
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_wf); // call [rip+iat_WriteFile]
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // add rsp, 0x38

    // 3) ExitProcess(0)
    code.extend_from_slice(&[0x31, 0xC9]); // xor ecx, ecx
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
    rel!([0xFF, 0x15, 0, 0, 0, 0], iat_exit); // call [rip+iat_ExitProcess]
    code.extend_from_slice(&[0xCC]); // int3

    // --- data slots at the end of .text ---
    while code.len() % 8 != 0 {
        code.push(0);
    }
    let ptr_off = code.len();
    code.extend_from_slice(&[0u8; 8]); // absolute ptr to msg1 (DIR64-relocated)
    let wr_off = code.len();
    code.extend_from_slice(&[0u8; 8]); // DWORD `written` (+ pad)
    let msg1_off = code.len();
    code.extend_from_slice(msg1);
    let msg2_off = code.len();
    code.extend_from_slice(msg2);

    code[ptr_off..ptr_off + 8]
        .copy_from_slice(&(IMAGE_BASE + text_rva as u64 + msg1_off as u64).to_le_bytes());
    for (pos, target) in fixups {
        let target_rva = match target {
            t if t == ptr_slot_tag => text_rva + ptr_off as u32,
            t if t == wr_slot_tag => text_rva + wr_off as u32,
            t if t == msg1_tag => text_rva + msg1_off as u32,
            t if t == msg2_tag => text_rva + msg2_off as u32,
            rva => rva,
        };
        let next_rva = text_rva as i64 + pos as i64 + 4;
        code[pos..pos + 4].copy_from_slice(&((target_rva as i64 - next_rva) as i32).to_le_bytes());
    }
    let ptr_reloc_rva = text_rva + ptr_off as u32;

    // --- .reloc: one block, one DIR64 fixup for the msg1 pointer slot ---
    let mut reloc: Vec<u8> = Vec::new();
    reloc.extend_from_slice(&(ptr_reloc_rva & !0xFFF).to_le_bytes()); // PageRVA
    reloc.extend_from_slice(&12u32.to_le_bytes()); // BlockSize
    reloc.extend_from_slice(&((10u16 << 12) | (ptr_reloc_rva & 0xFFF) as u16).to_le_bytes());
    reloc.extend_from_slice(&0u16.to_le_bytes()); // ABSOLUTE padding

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
    pe[opt + 112 + 12..opt + 112 + 16].copy_from_slice(&40u32.to_le_bytes());
    // data directory 5 = BASE_RELOC
    pe[opt + 112 + 5 * 8..opt + 112 + 5 * 8 + 4].copy_from_slice(&reloc_rva.to_le_bytes());
    pe[opt + 112 + 5 * 8 + 4..opt + 112 + 5 * 8 + 8].copy_from_slice(&reloc_vsize.to_le_bytes());
    // data directory 12 = IAT
    pe[opt + 112 + 12 * 8..opt + 112 + 12 * 8 + 4].copy_from_slice(&iat_exit.to_le_bytes());
    pe[opt + 112 + 12 * 8 + 4..opt + 112 + 12 * 8 + 8]
        .copy_from_slice(&(funcs.len() as u32 * 8).to_le_bytes());

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
        eprintln!("{tag}: kernel did not halt cleanly\n--- serial ---\n{serial}\n---");
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
        && serial.contains("PE via WriteFile") // GetStdHandle + WriteFile through the IAT
        && serial.contains("THOS: pe reject ok");
    if ok {
        println!("pe-test: OK — PE loader: reloc, imports, GetStdHandle/WriteFile marshalling");
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
