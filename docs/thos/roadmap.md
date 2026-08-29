# THOS – Roadmap

Each phase produces an independently useful, testable artifact. You can stop after any
phase without being back at zero.

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
- **AHCI/SATA driver** (Intel `8086:7a62`, standard AHCI 1.3.1 register interface,
  MSI/MSI-X, command list / FIS) → real root FS from the **Kingston A400 240 GB SATA
  SSD** (`sdc`). NVMe is Windows' disk and is never touched; an NVMe driver is
  out of scope for v1.
- **xHCI driver** (Intel `8086:7a60`) + USB HID (keyboard/mouse). PS/2 only as a QEMU
  stopgap.
- ELF loader; **POSIX personality**: syscall table (Linux ABI subset), signals, `futex`
  = the wait primitive, `mmap`, processes / `fork` / `execve`, TTY over serial + FB.
- Port **musl** as the userland libc; a BusyBox-style shell.
- **Milestone 2:** booted from the real SSD, interactive shell on a real keyboard,
  statically linked Linux `x86_64` ELF binaries (BusyBox) run unmodified.

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

## Open decisions

1. **GPU path A vs B** — decide after the Phase 4 spike.
2. **NT DLL strategy** — reuse Wine PE DLLs vs write our own. Licensing decided
   (see [`licensing.md`](licensing.md)); the Wine-vs-own build choice is still open.
3. ~~Exact NIC / board chips~~ — resolved, see [`hw-target.md`](hw-target.md).
4. **TCP/IP stack** — own vs smoltcp/lwIP port; decide in Phase 5.
