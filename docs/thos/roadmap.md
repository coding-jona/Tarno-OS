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
    file), `mkdir_path`; superblock + group-descriptor counts kept in sync.
    `cargo xtask ext2-test` has the kernel create a file, a dir and a nested
    file, then the host runs `e2fsck -fn` (clean) and `debugfs cat` on the
    result. Missing: `unlink`/`rmdir`, growing a dir past 12 direct blocks,
    htree, backup superblock/GDT copies (needed before writing the multi-group
    SSD), timestamps.
- **AHCI/SATA driver** (Intel `8086:7a62`, standard AHCI 1.3.1 register interface,
  MSI/MSI-X, command list / FIS) → real root FS from the **Kingston A400 240 GB SATA
  SSD** (`sdc`). NVMe is Windows' disk and is never touched; an NVMe driver is
  out of scope for v1.
  - Status: polled driver, single command slot. `READ`/`WRITE DMA EXT` +
    `FLUSH CACHE EXT` all working. `cargo xtask ahci-test` has the kernel write a
    pattern to a scratch LBA, read it back, and then re-checks from the host that
    it persisted into the disk image. Next: `IDENTIFY` (real capacity), multi-slot
    / NCQ, MSI-X completion interrupts instead of polling.
- **xHCI driver** (Intel `8086:7a60`) + USB HID (keyboard/mouse). PS/2 only as a QEMU
  stopgap.
- ELF loader; **POSIX personality**: syscall table (Linux ABI subset), signals, `futex`
  = the wait primitive, `mmap`, processes / `fork` / `execve`, TTY over serial + FB.
- Port **musl** as the userland libc; a BusyBox-style shell.
- **Milestone 2:** booted from the real SSD, interactive shell on a real keyboard,
  statically linked Linux `x86_64` ELF binaries (BusyBox) run unmodified.
  - Status: interactive shell (`/sh`, static-musl Rust) reads the USB keyboard,
    `fork`+`execve`+`wait4`s programs off ext2, reports exit status. Verified in
    CI (`cargo xtask kbd-test` types `init` and checks it runs). Still on the QEMU
    disk image, not the real SSD (no installer yet).
  - Fixed along the way: (1) SMP scheduler race — a thread that yielded from
    inside a syscall could be resumed on a second CPU before the first finished
    unwinding its kernel stack; now a per-thread `running` claim + deferred
    ready-queue hand-off (`thos_finish_switch`). (2) `%fs` base (TLS) is now
    context-switched per thread and inherited across `fork` — musl deref's `%fs`
    constantly. (3) `fork` now copies PML4[0] (static-musl ELFs load at
    `0x400000`), not just the higher user half.

## Phase 3 — NT personality (userspace level, still GOP graphics)

- **PE loader** native (sections, imports, TLS, PEB/TEB, `fs`/`gs` base).
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
