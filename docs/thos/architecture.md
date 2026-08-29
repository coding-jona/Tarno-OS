# THOS – Architecture

**THOS** (Tarno Hybrid OS) is a clean-slate operating system kernel that runs **native
ELF/POSIX programs and native PE/Win32 programs side by side**, without a Linux fork and
without virtualization. It is built for **one specific machine** (see
[`hw-target.md`](hw-target.md)) so that the driver surface is finite.

## Design principle

The compatibility contracts are fixed and non-negotiable — they *are* the product:

- PE (`MZ`) executable format, Microsoft x64 calling convention, the Win32/NT API surface
- ELF executable format, System V AMD64 ABI, the Linux syscall ABI (subset)

Everything **below** those contracts is designed fresh: the kernel, scheduler, memory
manager, object model, IPC, driver model, boot path.

## Layers

```
+---------------------------------------------------------------+
|  USER SPACE                                                    |
|   ELF/POSIX processes        PE/Win32 processes                |
|   musl libc (ported)         ntdll / kernelbase / kernel32     |
|                              + DXVK / vkd3d (D3D9-12 -> Vulkan)|
|                              + RADV (Mesa Vulkan userspace)    |
+---------------------------------------------------------------+
|  PERSONALITIES  (per process, selected by binary format)      |
|   POSIX subsystem            NT subsystem                      |
|   syscalls, signals,         SSDT dispatch, APCs, SEH,         |
|   futex, /proc view          Ob/Ps/Io/Se, \Device namespace,   |
|                              registry view                     |
+---------------------------------------------------------------+
|  EXECUTIVE CORE  (one set of primitives; both personalities)  |
|   Handle/Object Manager | Scheduler (P/E-core aware) | VMM     |
|   VFS | one Wait/Sync primitive | IPC/Ports | Timer | IRQ     |
+---------------------------------------------------------------+
|  HAL + DRIVERS  (this machine only)                           |
|   APIC/x2APIC, IOAPIC, MSI-X | ACPI (ACPICA) | PCIe           |
|   TSC/HPET | AHCI/SATA | xHCI (HID) | RTL8168 NIC | GPU (Navi 23)   |
+---------------------------------------------------------------+
|  BOOT: UEFI + Limine -> long mode, GOP framebuffer, memory map |
+---------------------------------------------------------------+
```

## The unifying idea: one object, many views

A POSIX `futex`, an NT dispatcher object (`KEVENT`), and a Win32 `HANDLE` to an event are
**three views of the same executive object**. Waiting, signalling, and reference counting
are implemented once in the Executive Core. POSIX signal delivery and NT APC delivery
share one asynchronous-delivery mechanism.

This is what makes THOS "hybrid" structurally rather than by emulation: neither
personality is a guest of the other.

## Personality selection

`execve`/`NtCreateUserProcess` inspects the image header:

- `ELF` magic  → POSIX personality, SysV AMD64 ABI, Linux syscall table
- `MZ`/`PE`    → NT personality, MS x64 ABI, SSDT dispatch

A process has exactly one personality for its lifetime. Cross-personality interaction
happens through shared executive objects (files, pipes, sockets, shared memory,
events) — never by one personality calling the other's syscall table.

## Language & reuse

- Kernel and drivers: **Rust nightly** (`x86_64-unknown-none`), `#![no_std]`. Nightly is
  pinned in `rust-toolchain.toml` (bare-metal deps such as the `limine` crate need
  unstable features).
- C FFI ports (isolated under `third_party/`): **ACPICA** (ACPI), **Mesa/RADV**
  (Vulkan userspace), possibly **amdgpu** KMS, **Wine** PE-built DLLs above `ntdll`.
- Written fresh: executive core, object model, scheduler, both personalities, the
  `ntdll` lower boundary, VMM, VFS, HAL, AHCI/xHCI/NIC drivers, boot path.

See [`roadmap.md`](roadmap.md) for the phased build order, [`feasibility.md`](feasibility.md)
for the honest cost/blocker analysis, and [`licensing.md`](licensing.md) for the
AGPL-3.0 ↔ LGPL/GPL reuse question.
