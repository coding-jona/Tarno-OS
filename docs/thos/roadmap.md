# THOS – Roadmap

Each phase produces an independently useful, testable artifact. You can stop after any
phase without being back at zero.

**Status:** Phase 0 ✔ · Phase 1 ✔ (memory, GDT/IDT/traps, ACPI/MADT, Local APIC + timer,
SMP, scheduler + wait primitive + handle table) · own page tables ✔ (W^X kernel + HHDM +
low identity, every CPU switched). Next: Phase 2 — VFS, AHCI, the POSIX personality.
The `syscall` fast path moved into Phase 2 (it needs the personality layer).

Honest scale: from "boots to a framebuffer" to "a Windows game runs" is multiple
person-decades for a small team. The phasing front-loads the finite, well-understood
work (executive core, personalities) and hits the GPU driver mountain deliberately and
late.

---

## Phase 0 — Foundation & facts

- Freeze the old distro components (`FROZEN.md`); rename their CI to `*.yml.frozen`.
- **Exact hardware inventory** of the target machine — mandatory input. Run under a live
  Linux on the machine and paste into [`hw-target.md`](hw-target.md):
  `lspci -nnvvv`, `lsblk`, `lsusb -t`, `dmidecode -t baseboard -t bios`.
  **Done — see [`hw-target.md`](hw-target.md).** Confirmed: ASRock B760M-HDV/M.2 D4,
  i7-13700KF (24T), RX 6600 (`1002:73ff`), Intel AHCI (`8086:7a62`), Realtek RTL8168
  NIC (`10ec:8168`), Intel xHCI (`8086:7a60`). THOS boots the Kingston A400 SATA SSD.
- Repo skeleton: `boot/ kernel/ hal/ personalities/{posix,nt}/ drivers/ loaders/
  userland/ third_party/ xtask/ docs/thos/`.
- Toolchain: Rust `x86_64-unknown-none`, Limine bootloader, QEMU + OVMF for CI, GDB stub.
- **Milestone 0:** `make run` boots in QEMU (OVMF); the kernel stub writes to the serial
  port **and** into the GOP framebuffer. CI green.

## Phase 1 — Executive core

- Physical frame allocator; 4-level paging; kernel heap; `vmspace` object.
- x86-64 trap/IRQ scaffold: IDT, exceptions, `syscall`/`sysret`, per-CPU state (`gs`).
- **SMP bring-up of all 24 threads** via ACPI MADT; per-CPU run queues.
- **Scheduler** with one thread primitive; P/E core classes from CPUID `0x1A` (Thread
  Director / HFI feedback comes later). NT priority classes and POSIX `nice` map onto
  the same run-queue policy.
- **Object / handle manager**: generic `object` struct, per-process handle table,
  refcounting, type registry.
- **One wait/sync primitive** + timer subsystem (TSC-deadline via local APIC timer,
  HPET for calibration).
- **Milestone 1:** SMP up, kernel threads scheduled on all cores, timer interrupts, the
  sync primitive survives 10^7-iteration stress with no deadlock / lost wakeup. Verified
  in QEMU and once on the real machine via USB stick (serial log).

## Phase 2 — VFS, storage, POSIX personality

- VFS layer + `vnode` object; RAM-FS; **ext2 read/write** (or a simple own FS); **FAT**
  (read the ESP).
  - Status: ext2 read (12 direct + single + double indirect) and write —
    block/inode bitmap allocators, `write_path` (create or overwrite a regular
    file), `mkdir_path`, `unlink_path`, `rmdir_path`; primary superblock +
    group-descriptor counts kept in sync, and the **backup SB + GDT re-synced
    from the primary after every mutation** (`sparse_super` honoured) so a
    multi-group filesystem stays `e2fsck`-clean. `unlink`/`rmdir`/`unlinkat`
    syscalls wired. `cargo xtask ext2-test` runs on a **2-block-group** image:
    the kernel creates a file/dir/nested file, then deletes files + dirs
    (rejecting a non-empty `rmdir`), and the host runs `e2fsck -fn` against
    **both the primary and the group-1 backup superblock** (both clean).
    Missing: growing a dir past 12 direct blocks, htree, timestamps, hard
    links, 64-bit sizes, journalling.
  - Status: **read the ESP** — `gpt.rs` finds a partition by type GUID (the
    `C12A7328-…` EFI System Partition) in a GPT whose LBA 0 is at an arbitrary
    base; `fat.rs` reads FAT16 / FAT32 (BPB → FAT-width by cluster count → FAT
    chain → 8.3 directory walk, VFAT long-name entries skipped).
    `cargo xtask fat-test` splices a self-contained GPT image (protective MBR +
    one ESP holding a FAT32 volume) into a hole past the ext2 image (LBA 51000);
    the kernel does `gpt::find_esp(51000)` → `fat::Fat::open(esp_lba)` →
    `read_path("/EFI/THOS/HELLO.TXT")` and prints it. Missing: FAT12, FAT
    writes, long names, and mounting the ext2 root from a GPT partition rather
    than raw LBA 0 (the "one real disk layout" migration — tied to the
    installer).
- **AHCI/SATA driver** (Intel `8086:7a62`, standard AHCI 1.3.1 register interface,
  MSI/MSI-X, command list / FIS) → real root FS from the **Kingston A400 240 GB SATA
  SSD** (`sdc`). NVMe is Windows' disk and is never touched; an NVMe driver is
  out of scope for v1.
  - Status: 32-tag command list. `IDENTIFY DEVICE` gives the 48-bit (fallback
    28-bit) sector count + model + NCQ support/depth; `read`/`write`
    bounds-check the LBA. When the drive advertises NCQ, I/O goes through
    `READ`/`WRITE FPDMA QUEUED` (write sets FUA for durability) issued via
    `PxSACT`+`PxCI` — up to `queue depth` transfers outstanding, drive reorders
    them; completion is polled from `PxSACT` and a waiting thread `yield`s
    instead of spinning a core. Falls back to single-tag `DMA EXT` +
    `FLUSH CACHE EXT` without NCQ. `cargo xtask ahci-test` verifies the sector
    count against the backing file and a past-the-end read is rejected; the
    boot milestone runs **8 threads doing concurrent read/write/verify** through
    the NCQ path with no corruption. **Completion is interrupt-driven**: the
    driver programs the device's MSI-X (preferred) or MSI capability to raise a
    vector on the BSP, disables legacy INTx, and a parked submitter blocks on a
    per-tag wait queue that the IRQ wakes; a timer poll is a safety net and pure
    `yield` polling is the fallback with no MSI. `cargo xtask ahci-test` asserts
    the completion IRQ actually fired (QEMU's `ich9-ahci` gives MSI; real Raptor
    Lake `8086:7a62` gives MSI-X).
  - NCQ error handling: on `PxIS.TFES` one thread runs `recover()` (port stop →
    COMRESET via `PxSCTL.DET` → restart → best-effort `READ LOG EXT` page 0x10
    for the failing tag); the failing tag's `wait` returns `Err`, every other
    aborted tag is re-issued so its `wait` still returns `Ok`, and the per-tag
    parked waiters are woken. `PENDING[32]` records each in-flight command's
    params for the re-issue. `cargo xtask ncq-error-test` uses QEMU `blkdebug`
    to fail one read and asserts the error surfaces as `Err` with a recovery
    pass and **no hang** (a wedged port or lost waiter would time the test out).
    Post-recovery port usability is real-hardware territory — QEMU's AHCI does
    not model link recovery after an NCQ abort well enough to verify it.
- **xHCI driver** (Intel `8086:7a60`) + USB HID (keyboard/mouse). PS/2 only as a QEMU
  stopgap.
- ELF loader; **POSIX personality**: syscall table (Linux ABI subset), signals, `futex`
  = the wait primitive, `mmap`, processes / `fork` / `execve`, TTY over serial + FB.
- Port **musl** as the userland libc; a BusyBox-style shell.
- **Identity stub**: the `Principal` object + a console `login` before the shell +
  file owner/mode bits (see *Identity, privilege & login* below).
- **Milestone 2:** booted from the real SSD, interactive shell on a real keyboard,
  statically linked Linux `x86_64` ELF binaries (BusyBox) run unmodified.
  - Status: **the interactive login shell is now stock BusyBox `sh` (ash)** —
    `/busybox` with `argv[0] = "sh"`, replacing the toy `/sh`. It reads the USB
    keyboard, `fork`/`execve`/`wait4`s programs off ext2, reports exit status,
    and shows the `thos$ ` prompt. Verified in CI (`cargo xtask kbd-test` types
    `init` and checks it runs under the BusyBox banner). Still on the QEMU disk
    image, not the real SSD (no installer yet).
  - Getting ash interactive needed a minimal terminal `ioctl`: `TCGETS` reports
    a canonical-mode termios with the terminal's own **ECHO off** (our
    line-disciplined console already echoes + edits), so ash turns the prompt on
    but leaves line editing to us. `TCGETS` writes only the 36-byte
    `struct __kernel_termios` glibc's `tcgetattr()` passes — writing the full
    60-byte userspace struct smashed its stack canary. Plus the syscalls ash
    needs on the way up: `clone(SIGCHLD, stack=0)`→fork, `newfstatat`,
    `dup`/`dup2`/`dup3`/`fcntl(F_DUPFD)`, `nanosleep`, `sysinfo`, `waitid`,
    `getppid`, `getpgrp`, `setpgid`/`setsid`/`chdir` (no-ops), `setuid`/`setgid`.
  - **Stock static BusyBox runs unmodified** (`cargo xtask busybox-test`, from the
    `busybox-static` package): `busybox echo …` loads via the ELF loader and
    exits cleanly through the POSIX personality — the Milestone-2 "unmodified
    Linux x86-64 binary" bar.
  - **BusyBox applets from the prompt.** The disk image lays down `/bin/<applet>`
    as **hard links** to the single `/busybox` inode (`debugfs ln` + an explicit
    `links_count` so `e2fsck` stays clean); the shell's `PATH` is `/bin:/` and
    BusyBox dispatches on `basename(argv[0])`. `ls` needed real directory reads,
    which the kernel did not have: `ext2::read_dir` (entry filetype → Linux
    `DT_*`), a `file::DirFile` that pre-renders `linux_dirent64` records, and the
    `getdents64` syscall; `open` on a directory now returns a `DirFile`. `cat`
    uses the existing `MemFile` path (plus a real `sendfile` so it takes its
    fast path). `time` is stubbed to a fixed epoch (no RTC yet).
  - **Per-process cwd.** `Task` carries a normalised absolute `cwd`;
    `process::resolve_path` folds `.` / `..` / relative paths against it and is
    applied at every path syscall (`open`/`openat`, `execve`, `newfstatat`,
    `unlink`/`rmdir`). `chdir` verifies the target is an ext2 directory before
    storing; `getcwd` returns the real string; `fork` inherits it, `execve`
    keeps it. `cargo xtask kbd-test` now does `cd /bin` then a bare `ls` / `pwd`
    and checks they act on `/bin`.
  - **Pipes.** `pipe` / `pipe2` back a bounded (64 KiB) in-memory byte stream
    with two typed `FileOps` endpoints; `read` blocks (yield) until data or all
    write ends drop (EOF), `write` blocks until space or all read ends drop
    (`EPIPE`). Endpoint counts track distinct endpoint objects, so an fd shared
    by `fork`/`dup` counts once. Descriptors now carry a **close-on-exec** flag
    (`FdEntry`): `O_CLOEXEC` on `pipe2`/`dup3`, `fcntl(F_GETFD/F_SETFD)`, and
    `execve` drops the marked fds. A zombie task's fd table is cleared at
    `exit` so the far end of a pipe sees EOF before `wait4` reaps it.
    `cargo xtask pipe-test` boots BusyBox `sh -c` with a `|` and two `$(…)` and
    checks the exact output.
  - **Real blocking waits.** `WaitQueue::wait_if(pred)` closes the condvar
    lost-wakeup race (queue lock held across predicate + enqueue). The three hot
    spin loops now sleep instead of `yield`ing: pipe read/write on the pipe's
    own `WaitQueue`, `wait4` on a global `CHILD_EXIT` queue woken at `exit`, and
    fd-0 reads on a console `INPUT_WQ` woken by the keyboard poll thread — an
    idle shell prompt no longer pins a core. Still missing: `fchdir` (no
    fd→path), writing through `/bin/*` links, `nanosleep` (still a yield spin —
    needs a timer wheel).
  - Disk I/O is batched: `mm` reserves a 1 MiB contiguous DMA arena, AHCI gives
    each tag a 32 KiB bounce buffer and transfers up to 32 KiB per NCQ command,
    and `ext2::read_file` issues one read per run of consecutive blocks. The
    boot milestone's concurrent-NCQ test went from ~1000–5000 completion IRQs
    to ~190.
  - Fixed along the way: (1) SMP scheduler race — a thread that yielded from
    inside a syscall could be resumed on a second CPU before the first finished
    unwinding its kernel stack; now a per-thread `running` claim + deferred
    ready-queue hand-off (`thos_finish_switch`). (2) `%fs` base (TLS) is now
    context-switched per thread and inherited across `fork` — musl deref's `%fs`
    constantly. (3) `fork` now copies PML4[0] (static-musl ELFs load at
    `0x400000`), not just the higher user half.
  - SMP stress: `cargo xtask smp-test` boots at **24 vCPUs** (the target's 8P×2 +
    8E) and runs `smp_stress_milestone` — 512 threads churning `yield`/`exit` in
    overlapping waves, 48 threads blocking + being mass-woken on the wait queue,
    4 real user `fork`/`wait4` processes — then asserts exact run counts (no lost
    / double-run) and per-thread stack canaries (no thread ran on two CPUs at
    once). Added `sched::reap()` to free exited threads' kernel stacks (they
    leaked forever before).

## Phase 3 — NT personality (userspace level, still GOP graphics)

- **PE loader** native (sections, imports, TLS, PEB/TEB, `fs`/`gs` base).
  - Status: **first cut** (`pe.rs`) — parse the DOS/PE/PE32+ headers + section
    table, map each section as fresh zeroed user frames at the preferred
    `ImageBase` with per-section exec bit, BSS zero-fill, `process::spawn_pe`
    enters the entry in ring 3 with a bare 16-aligned stack (no SysV block).
    Rejects imports / relocations / TLS for now. `cargo xtask pe-test` writes a
    hand-assembled statically linked Win64 `.exe` (`write(1,…)` + `exit(0)` via
    `syscall` — the CPU runs `syscall` from any ring-3 code regardless of
    container) into ext2 and checks it prints + exits. ELF and PE processes run
    in the same boot on the same kernel.
  - **Hardening done (P0):** `pe.rs` treats every input as hostile — bounds-
    checked LE reads, sane limits on `SizeOfImage` / `NumberOfSections` /
    `ImageBase`, overflow-checked arithmetic, section data clamped to what the
    file holds. A malformed `.exe` returns `Err`, never a slice panic;
    `spawn_pe` returns `Result`. `pe-test` also feeds it a truncated / bad-
    `e_lfanew` blob and asserts the kernel rejects it and stays alive.
  - **Base relocations done (P1):** `pe::load` materialises the full image at
    RVA 0, then walks data directory 5 — `IMAGE_REL_BASED_DIR64` targets get
    `delta` added, `ABSOLUTE` is padding, any other type is rejected. A PE with
    `DYNAMIC_BASE` + a `.reloc` section is loaded at an alternative base
    (fixed non-zero shift for now; a real availability check / ASLR comes with
    the DLL loader) so the fixup path is actually exercised. The `pe-test` `.exe`
    now loads its message pointer from an **absolute** slot patched by a DIR64
    fixup — a wrong delta would fault or misprint, so the exact-string check is
    the relocation test.
  - **Imports + NT syscall surface done (P1):** `pe::load` walks the Import
    Directory Table, resolves each by-name thunk against a builtin resolver, and
    patches the IAT in place (post-relocation); unresolved names / ordinals are
    rejected, not left dangling. A **shared NT stub page** is mapped into every
    PE process at a fixed high address — one 16-byte trampoline per NT call
    (`mov eax, NT_BASE|idx; mov r10, rcx; syscall; ret`). `rax` values in the
    `NT_BASE` (`0x4E540000`) range route to **`nt::dispatch`** (`nt.rs`), which
    reads the Win64 arg registers (`r10`=former `rcx`, `rdx`, `r8`, `r9`, then
    the stack) off the `UserFrame` and marshals onto THOS objects.
  - **`GetStdHandle` / `WriteFile` / `ExitProcess` implemented.** The `pe-test`
    `.exe` prints its first line via a raw `syscall` (relocated absolute
    pointer), then prints a second line via
    `WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), buf, len, &written, NULL)` —
    real Win64 arg passing, `*written` write-back, `TRUE` return — then
    `ExitProcess(0)`, all through the IAT. **A PE process now makes real Win32
    API calls that operate on THOS's own fd/console objects.**
  - **Next:** more `kernel32` (`CreateFileA`/`ReadFile`/`GetLastError`), PEB/TEB
    + `gs` base, a proper `ntdll` boundary (`Nt*` primitives) under `kernel32`,
    then real PE-built DLLs from a `C:\Windows\System32` tree; then process
    isolation / integrity for the security phase.
- **NT personality**: SSDT dispatch; `Nt*` core (`NtCreateFile` / `NtReadFile` /
  `Nt*VirtualMemory` / `NtWaitForSingleObject` …) onto executive primitives;
  **`\Device\` namespace** + drive letters as a VFS view; a minimal **registry** as a
  transactional key-value store; **SEH** ↔ trap dispatch; **APC** delivery.
- Write the `ntdll` lower boundary; layer **Wine PE-built DLLs** (`kernel32` /
  `kernelbase` / `user32` core) on top.
- **Milestone 3:** a statically linked Win32 **console** `.exe` (`CreateFile`,
  `WriteFile(stdout)`, `WaitForSingleObject`) runs through the THOS NT path — **with no
  wine process in the tree**. ELF and PE processes appear in one `ps` output.

### 32-bit Windows apps — WOW64 thunks, not code translation

An x86-64 CPU runs 32-bit code natively (long mode's compatibility sub-mode), so
there is **no instruction translation** and no emulator for i386 `.exe`s — same as
on real Windows. What a 32-bit PE32 process needs on top of the 64-bit kernel:

- run its threads with a **32-bit code segment** (compat mode); the loader picks
  the segment from the PE `Machine` field (`0x14c` i386 vs `0x8664` x86-64);
- a **32-bit `ntdll`** whose stubs widen arguments and enter the 64-bit kernel —
  the classic **WOW64 thunk layer**: marshal the 32-bit stack/register args into
  the 64-bit `Nt*` ABI, call, narrow the result back. Pointers stay in the low
  4 GiB for these processes so handles/addresses round-trip.
- 32-bit and 64-bit code **never mix in one process** (no loading a 32-bit DLL
  into a 64-bit process or vice versa), exactly like Windows.

Real architecture emulation (x86 ↔ ARM, à la Rosetta / Windows-on-ARM) is **out
of scope** — the target is Intel x86-64, where every mainstream Windows binary is
i386 or x86-64 and runs on the metal. WOW64 is a Phase 3+ item, after the 64-bit
NT path of Milestone 3 works.

### Filesystems for the NT personality — no NTFS driver needed

Windows apps expect `C:\...` with NTFS *semantics* (ADS, ACLs, case-insensitive,
reparse points), **not** a real NTFS on-disk driver. `C:` is a directory tree on the
ext4 root — the `\Device\HarddiskVolume` → drive-letter → VFS-path mapping does the
translation, exactly like Wine's `drive_c/`. Case-insensitivity and ADS are handled in
the NT VFS layer, on top of ext4.

A real **NTFS driver** is only for *mounting the actual Windows partition* (the 512 GB
NVMe) to see Windows' own files:
- **read-only NTFS**: moderate (MFT, runlists, `$DATA`, basic compression) — optional,
  post-Milestone-3.
- **read-write NTFS**: hard and risky (`$LogFile`, USN journal, consistency) — Phase 4+
  research item, same tier as loading `.sys` drivers. Not on the critical path.

### Identity, privilege & login

Old Tarno-OS inherited the whole Unix multi-user stack (PAM, shadow, sudoers,
polkit) and the pain was gluing it together. THOS picks **one** coherent model
instead of bolting Unix and Windows identity side by side.

- **One executive `Principal` / security context (token)**: a stable principal id
  + group membership + a privilege set. The **POSIX** personality projects it as
  `uid/gid`; the **NT** personality projects it as a SID + access token —
  deterministic mapping, not two separate identities the way Wine fakes one.
- **Multi-user-*capable* from day one, single-user in practice.** The token layer
  has SIDs / groups / per-principal `\home` from the start; the installer creates
  exactly one **admin** principal. Adding users later needs no redesign. *(User
  decision, 2026-08-30.)*
- **One canonical ACL in the VFS.** Unix mode bits and an NT DACL are both *views*
  of it (like macOS: POSIX perms + native ACLs coexist).
- **No root login — admin elevates** (the most defensible model, *user decision*):
  - `SYSTEM` / principal 0 has **no password and no login**. A credential that
    doesn't exist can't be phished or brute-forced.
  - Even the admin's normal session runs **unprivileged** (low integrity, uid ≠ 0).
    Ambient privilege is the exception, never the default.
  - **One elevation primitive** `elevate(cmd)`: policy check (caller in the admin
    group?) + re-authentication + the request travels a **trusted path** (a
    secure-attention key, à la Ctrl-Alt-Del) so no app can draw a fake prompt.
    The elevated token is **scoped** to that process/operation with only a short
    grace window — not a standing root shell. A `doas`-style CLI and a Win32
    UAC-manifest are just two front-ends to this one mechanism.
  - **Recovery** is a boot-time mode (offered by the boot picker) gated by
    **physical presence + the disk passphrase**, not by a reachable account.
- **Auth**: `argon2id` password hashes in a THOS-native credential store
  (SAM/shadow-shaped but our own format), not `/etc/shadow`.

#### Age declaration & content restriction (family-safety)

THOS ships a built-in maturity gate so an under-age operator is protected by
default — the same feature class as Windows Family Safety / iOS Screen Time /
Android parental controls, wired into the principal model rather than bolted on
(*user requirement, 2026-08-31*).

- **Not at setup — triggered on demand.** THOS installs and runs without any age
  check. The `age-verify` flow fires only when a principal that is not currently
  `adult-verified` tries to reach **18+ content** (an app/site/package the policy
  engine rates adult). The result is cached on the principal and reused; it
  **expires after ~14 days of account inactivity** (and a hard max regardless),
  after which the next 18+ access re-runs the flow. A `minor` account never hits
  this — the admin sets `minor` directly and only the admin can lift it.

  **The decided flow** (*user, 2026-08-31*), fully local, buffers zeroed
  immediately, nothing written to disk:
  1. **Capture front + back of a national ID card** (webcam, or uploaded stills
     if the machine has no camera) — the whole card legible.
  2. **Pull the data** — MRZ / VIZ OCR → date of birth (+ cross-check MRZ↔VIZ,
     check digits); derive the age.
  3. **Face match** — one webcam photo of the operator; an algorithm compares
     the ID portrait against the live face (+ a basic liveness check so it isn't
     a photo of a photo). **Skipped if there is no webcam.**
  4. Grant `adult-verified` with a timestamp; discard the images and OCR crops.

  Why an ID card and not a chip reader: THOS **does not assume an NFC reader
  exists**. It does assume that anyone entitled to 18+ content is ≥ 18 and that
  adults reliably hold a national ID card, while minors often do not — so
  requiring the card as the artefact is itself a real filter, and the face match
  stops a minor simply presenting a parent's card. This is `assurance = optical`
  (with-face) / `optical-doc-only` (no webcam): it **deters**, it is not a
  cryptographic proof. The NFC-chip path (PACE → Passive + Chip Authentication
  against a CSCA master list) stays specified as an **optional high-assurance
  upgrade** for deployers who want `chip-verified`, not a default requirement.
- **Default-deny for adult content.** Until a principal is `adult`, the policy
  engine (the same one the exec-gate and Security Service already run) denies:
  launching apps/packages carrying an `18+` age rating; installs from stores
  above the principal's rating; and network access to domain categories
  (adult / gambling / …) via the Security Service's category blocklist. A
  `minor` principal cannot lift its own restriction — only the **admin
  principal** can change a `maturity` attribute, through the trusted-path
  elevation prompt.
- **Enforcement points** already exist in the design: the native-exec gate adds
  an age-rating check to its `policy engine` step; the Security Service's
  network firewall/IDS does the domain-category filtering; the package/store
  layer checks the rating at install time.
- **`age-verify` module** outputs `(age, assurance, verified_at)` and never
  persists the source material — camera / OCR / APDU buffers are zeroed the
  moment the age is extracted, nothing touches disk.
  `assurance ∈ { declared, optical-doc-only, optical, chip-verified }`; the
  policy engine decides which clears the 18+ gate (default: `optical` and up,
  since the real backstop is the admin-lock + default-deny filter, not the
  proof strength) and how long a grant lasts before the inactivity re-scan.

Engineering reality:
- **Primary (optical) path:** a UVC camera driver (a large item on its own),
  MRZ / OCR-B recognition (a purpose-built recogniser, not a full OCR port), a
  face-detect + face-embedding compare with a basic liveness check, all on
  device. OCR is noisy (~80 % on clear MRZ) — hence `optical`, not proof.
- **Optional chip upgrade — a substantial, security-critical module:**
  **USB PC/SC (CCID) reader driver** — a new device class for THOS.
- **PACE** is password-authenticated EC Diffie-Hellman (domain params, the
  generic-mapping step, mutual auth); BAC is the older 3DES fallback. Getting
  this wrong makes the gate worthless, so it needs real review.
- **Passive Authentication** — CMS/PKCS#7 `SOD` signature verification, X.509
  path building to a `CSCA`, per-DG hash checks. Ship + periodically refresh the
  CSCA master list (out-of-band; not a phone-home).
- **Active / Chip Authentication** — RSA or ECDSA challenge-response.
- Hardware assumptions become real: the operator needs a contactless reader and
  an ID with an ICAO-9303 chip (all EU eIDs and biometric passports have one;
  some older / non-EU documents do not — hence the admin-permitted declaration
  fallback).

Open-source building blocks (2026-08 survey — reuse, don't reinvent):
- **eMRTD chip protocol, in Rust:** `worldfnd/icao-9303` — pure-Rust eMRTD core
  with BAC, **PACE (ECDH-GM P-256)**, LDS parsing, secure messaging. This is the
  biggest win — the hard crypto already exists to vendor / port. Cross-check
  against **JMRTD** (Java, the reference implementation) and **pypassport**
  (Python: BAC + partial PACE + Passive + Active Auth).
- **MRZ parse + check digits:** the `mrz` crate (zero deps, `wasm`-clean → very
  likely `no_std`-portable), or `mrtd` (`asmarques/mrtd`). Trivial to vendor;
  the *parse* is easy, the OCR that feeds it is the weak link.
- **PC/SC + CCID:** `pcsc-lite` + `libccid` as the reference for a THOS
  USB-CCID class driver (the CCID spec is small).
- **CSCA trust anchors:** the ICAO PKD **master list** (LDIF, signed by the UN
  CSCA). ⚠ its terms are **non-commercial only** — for a distributed THOS use
  national master lists (e.g. German BSI) instead. Ship + refresh out-of-band.
- **MRZ OCR (optical path):** PassportEye (Tesseract, ~80 % precision —
  confirms OCR is noisy and why `optical` is `deters`, not `proves`),
  `mrz-scanner` (fully-offline PWA) as UX reference.
- **Face compare / age (optical path):** `BetterAgeVerify` (privacy-first OSS,
  on-device, images deleted immediately) and general OSS face-embedding models
  as references for the ID-portrait ↔ live-face match + a photo-of-photo
  liveness check.
- **Content-category domain lists (policy-engine network filter):**
  `StevenBlack/hosts` (porn / gambling extensions), **HaGeZi dns-blocklists**
  (NSFW + gambling categories), `blocklistproject/Lists`, `aegis-blocklist`
  (child-safety, VPN/proxy bypass-prevention). The Security Service just ingests
  these — no list to author.
- **Malware scanning (AV):** `yara-x` (pure-Rust YARA), ClamAV signature DBs.

Hard limit — **CSAM is not a content-filter feature and THOS will not implement
a CSAM scanner.** Such material is illegal to possess irrespective of any
filter, and detection is a specialised legal/reporting domain (hash databases,
mandated reporting) that a from-scratch OS must not reinvent. The only coverage
the design gives is incidental: the Security Service already blocks
known-malicious / known-bad domains from threat-intel feeds, so hosts on those
feeds are blocked by the same mechanism as malware C2 — no dedicated subsystem,
no content inspection.

Sequencing: designed in now (the `maturity` attribute rides the principal model
being built), enforced once the policy engine / exec-gate / network filter land
in the security phase — after the NT personality.

Phasing:
- **Phase 2 (stub):** the `Principal` object exists; a console `login` runs before
  the shell and sets the session's principal; files carry an owner + mode bits;
  one admin principal; `elevate` = a password re-check.
  - Status: **first-run setup + login done** (`kernel/src/{cred,login}.rs`). No
    account ships. First boot forces the operator to set the admin name +
    password (masked) in a console overlay; it is PBKDF2-HMAC-SHA-256'd (salt
    from `RDRAND`, soft-SHA — the kernel only enables SSE) into
    `/etc/thos/admin.cred` on ext2. Every later boot authenticates against it;
    the session `Principal` (uid 1000 — the admin session is unprivileged) is
    stamped onto every task and returned by `getuid`/`getgid`. `cargo xtask
    login-test`: setup runs once, reboot goes straight to login, a wrong
    password is rejected. Still stub: PBKDF2 not argon2id, no `Principal`
    object proper, no file-owner enforcement, no `elevate` yet, password
    changing is "rewrite the store + reboot" not a settings action.
- **Phase 3 (full):** SID / token model, NT DACL ↔ canonical-ACL translation, the
  UAC path, the trusted-path prompt, privilege sets (`SeDebugPrivilege` …).

## Phase 4 — Real graphics (the GPU mountain)

- **GPU driver for Navi 23** — decide after a 2–4 week spike:
  - Path A: port the Linux `amdgpu` KMS driver.
  - Path B: a thin RDNA2 KMS (DCN modeset, GPUVM, GFX/SDMA PM4 rings, SMU power) + a
    **DRM ioctl compat layer** so **Mesa RADV runs unmodified**.
- A small own **Wayland compositor** on the KMS object.
- **GDI / `HDC`** → compositor surface (Cairo/Pixman); **DXGI/WDDM personality** →
  compositor + **DXVK / vkd3d-proton** for D3D9–12.
- **Milestone 4:** RADV `vkcube` renders natively; a D3D11 Win32 GUI app draws into a
  window; no tearing (native page flips).

## Phase 5 — Hardening & a real app

- Board **NIC driver** + TCP/IP stack (own, or port smoltcp/lwIP); sockets in both
  personalities bind the same endpoint objects.
- HD-Audio; hotplug; suspend optional.
- Scheduler: real Thread-Director HFI feedback for P/E placement.
- **Milestone 5:** a real Windows game (D3D11, no anti-cheat, statically resolvable)
  starts and is playable; a native Linux workload runs on the same cores concurrently.

### Security architecture — a full antivirus, enforced in the kernel, scanning in userspace

THOS ships a **full-featured antivirus / anti-malware capability** — this is a
committed deliverable, not an optional add-on (*user, 2026-08-31*). What is
deliberate is *where* it lives: the **scanner never runs in the kernel**. A bug
in a YARA rule or a PE analyser must not be able to panic the box. "Full" means
real detection coverage (signatures, heuristics, static analysis, quarantine,
on-access + on-exec + on-demand scanning), delivered as the isolated userspace
Security Service below — not "pick the strongest off-the-shelf AV and staple it
into ring 0."

- **Security Core (kernel):** the hard boundaries only — measured / secure boot,
  process isolation, the capability policy (already the identity model's
  direction), W^X + memory protection, file-integrity baselines. Small, auditable,
  no parsing of untrusted formats beyond what the loaders already do (now
  hostile-input hardened).
- **Security Service (isolated userspace) — the full AV:** real-time (on-access
  + on-exec) and on-demand scanning; file scanner (YARA + open-source signature
  sets, e.g. ClamAV-style DBs); exec scanner (PE/ELF static analysis, reusing
  `pe.rs` / `elf.rs` — import table, section entropy, packer detection);
  heuristics; quarantine store; update mechanism for rules/signatures; network
  firewall / IDS. Talks to the Security Core over a narrow capability-gated
  interface; a crash there degrades to a policy default, it does not take the
  kernel down.
- **Milestone (Security):** a known-malicious EICAR-class test PE and ELF are
  caught by the exec gate before their first instruction runs, quarantined, and
  logged — with the scanner process killable and restartable without touching
  the kernel.
- **The native-exec gate** (unique to the hybrid design): every program entering
  the system — PE *or* ELF — passes one pipeline before it is allowed to run:
  `format detect → parse headers → hash / signature check → YARA / static
  analysis → policy engine → ALLOW | QUARANTINE`. Because both container formats
  execute natively, this gate covers the whole system with one mechanism instead
  of two half-measures.
Sequencing: the AV is a firm requirement, but it is built **after** the NT
personality (PE imports / `Nt*` dispatch / process isolation) is real — a
scanner has nothing to protect until Windows binaries actually run, and the
exec gate reuses the loader internals that are being built now. Designed in
from the start (loaders already hostile-input hardened; identity model already
capability-shaped), implemented as its own phase once M3 lands.

## Phase 6 — Research track: real `.sys` drivers (after M5)

- Scope **hard-limited to one device class** (recommended: NDIS networking, as
  `ndiswrapper` historically did). GPU / storage / USB via `.sys` are excluded.
- Parts: `ntoskrnl` / `hal` export shim; WDM IRP state machine (`IoCallDriver` /
  `IoCompletion`); minimal PnP/power IRPs; the **IRQL model** (`PASSIVE` / `DISPATCH` /
  `DIRQL` → THOS scheduler preemption states / softirq / spinlock — THOS has an edge
  over a Linux fork here: the preemption model is its own and can bake in IRQL from
  the start).
- **Documented blockers** — Go/No-Go before starting (see [`feasibility.md`](feasibility.md)):
  no signature / code-integrity chain for third-party `.sys`; no iGPU fallback if a GPU
  `.sys` crashes; `.sys` expects exact NT kernel struct layouts (`_ETHREAD`, `_KPCR` …)
  that must be reproduced; anti-cheat / DRM `.sys` actively probe the environment.
- **Milestone 6:** a real NDIS `.sys` binds a virtual NIC in QEMU, then the board NIC;
  data path through the NT-personality sockets.

## Phase 7 — Hardware breadth (LAST — only once the OS is feature-complete on the target)

Everything above is **hard-coded for the one machine** (ASRock B760M-HDV, i7-13700KF,
RX 6600). Broad hardware support is deliberately the *final* phase: it multiplies
every driver's test surface and is worthless before the OS itself is done.

Enabling structure (small, can land earlier as good hygiene):

- **Driver-binding registry** — each driver declares a match table
  (`{pci_vendor, pci_device, class, acpi_hid}`); the bus code binds automatically
  from PCI/ACPI enumeration instead of the current hand-written `find_ahci()` /
  `find_xhci()`. This is the one piece worth building before Phase 7.
- **Stable loadable-driver ABI** — a versioned in-kernel interface so drivers ship
  **prebuilt** and load at runtime (`insmod`-style), *not* recompiled per machine.
  Without this you have re-invented DKMS / Gentoo: a toolchain + kernel source on
  every install. Compiling a driver on the target stays an **escape hatch** for
  unpackaged hardware, never the model. (Firmware blobs — e.g. `amdgpu/*.bin` — are
  fetched, never compiled, regardless.)
- **`hwdetect` + a driver repo** — map detected IDs to driver packages, fetch them.

Scope notes for when this phase actually starts:

- **Intel CPUs** (desktop): mild — differences are ACPI tables, chipset AHCI/xHCI
  (already standard interfaces), CPU features via CPUID.
- **Intel laptops**: a big step beyond desktop — embedded controller, ACPI
  thermal/battery/backlight, S0ix sleep, `_DSM` quirks, Thunderbolt. The platform
  surface here rivals the GPU.
- **AMD GPUs beyond Navi 23**: each generation (RDNA1/2/3, Vega, APUs) is its own
  DCN display block + power management + firmware set — multiplies the Phase 4
  "GPU mountain".

Fork-in-the-road to decide before Phase 7: monolithic-with-loadable-modules (Linux
model) vs. userspace drivers (Fuchsia model). The stable-ABI item above assumes the
former.

---

## Multi-boot: THOS as the OS picker

Goal: set the Kingston (THOS) first in the mainboard boot order; every power-on
lands in a THOS-drawn menu that lists the OSes found on the other disks (Windows
on the NVMe, Devuan on the Samsung, …) and boots the chosen one with **no
keypress required** for the default after a timeout.

**Status — v1 built (`loaders/thos-boot`), verified in QEMU.** A standalone
`x86_64-unknown-uefi` app (the `uefi` crate). It:

- enumerates loaders two ways and merges them: the `Boot####` / `BootOrder`
  NVRAM entries (filtered to on-disk `*.efi` options — firmware apps like the
  setup UI, UEFI shell, and the generic "UEFI …" fallbacks are dropped), and a
  direct probe of every `SimpleFileSystem` volume for well-known paths
  (`\EFI\Microsoft\Boot\bootmgfw.efi`, `\EFI\<distro>\{shim,grub}x64.efi`,
  `\EFI\systemd\systemd-bootx64.efi`, `\EFI\limine\BOOTX64.EFI` → "THOS");
- reads `\EFI\thos\boot.conf` from the ESP it launched from — `timeout=<secs>`
  and `default=<index>`|`<label substring>`;
- draws a text menu (redraws only on change), counts down, and on select does
  `LoadImage(FromDevicePath)` + `StartImage`. No `BootOrder` writes.

`cargo xtask bootpick-test` boots it under OVMF with three fake disks (a THOS
disk with the picker + a `default=THOS` conf, a "Windows" disk, a "Linux" disk)
and asserts it enumerated all three and chainloaded the THOS entry.

**Still to do before relying on it:** read GPT explicitly (today we lean on the
firmware's own partition/FAT drivers, which is enough for real ESPs but not for
listing partitions ourselves); a graphical (GOP) menu; real-hardware test on the
ASRock board; and the risks below.

- **Chainloading.** Picking "Windows" = `LoadImage` on
  `\EFI\Microsoft\Boot\bootmgfw.efi` from that disk's ESP and `StartImage`.
  Picking "THOS" = chainload our Limine + kernel as today. This is exactly what
  rEFInd and the systemd-boot menu do; it is not virtualization and not a fork.
- **Where it lives.** `loaders/thos-boot`. The THOS kernel stays uninvolved —
  the picker runs before any kernel loads. Ships onto the Kingston ESP as
  `\EFI\BOOT\BOOTX64.EFI` (additive; touches no other disk).
- **Risks to keep in mind.** Firmware NVRAM quirks (some boards re-assert their
  own `BootOrder`), Secure Boot (chainloading MS's loader is fine; loading our
  unsigned kernel needs SB off or our keys enrolled), and BitLocker (measuring a
  different pre-boot environment can trigger a recovery-key prompt on the
  Windows side — needs testing before relying on it).

## Open decisions

1. **GPU path A vs B** — decide after the Phase 4 spike.
2. **NT DLL strategy** — reuse Wine PE DLLs vs write our own. Licensing decided
   (see [`licensing.md`](licensing.md)); the Wine-vs-own build choice is still open.
3. ~~Exact NIC / board chips~~ — resolved, see [`hw-target.md`](hw-target.md).
4. **TCP/IP stack** — own vs smoltcp/lwIP port; decide in Phase 5.
5. **Driver model** — monolithic-with-loadable-modules vs userspace drivers.
   Only forces a decision at **Phase 7** (hardware breadth); until then everything
   is compiled in for the one target machine.
