# THOS – Installing software (plan)

*Opened 2026-10-02 (user request). Planning only. Goal: a THOS user can install and
run software from **both worlds** — Windows `.exe` / `.msi` installers and Linux
packages — through one coherent, secure, customisable mechanism.*

## 0. Honest starting point

Today THOS runs **statically linked** Linux x86-64 ELFs (BusyBox, static musl Rust)
and **statically linked** Win64 PEs through its own from-scratch loaders. Real-world
installers and packages are mostly *dynamic*: Windows installers pull in dozens of
system DLLs, COM, the registry and services; Linux packages need `ld.so`, glibc,
`/proc`, `/sys`, `/dev`. So "install anything" is gated on **runtime coverage**, not on
the installer UI. The plan therefore builds the package *mechanism* first and widens
compatibility underneath it, stage by stage.

## 1. One mechanism, many formats

A single **package manager service** (`thos-pkg`) owns install/remove/upgrade/verify
and a package database; format handlers are plug-ins that translate a foreign
package into a THOS **install transaction**.

| Format | Handler | What it really needs |
|---|---|---|
| **`.exe` installer** (NSIS, Inno Setup, InstallShield, plain self-extractors) | run the installer *inside the NT personality* in a recording sandbox | enough Win32 (file/registry/shell/COM basics, `CreateProcess`, temp dirs, DLL loading) — Wine's DLLs are the open decision N-DLL in `roadmap.md` |
| **`.msi`** | **msiexec** service: Windows Installer database (OLE compound file → tables), standard actions, registry/file/shortcut/service tables | MSI engine — reuse Wine's `msi.dll` (LGPL, see `licensing.md`) *or* a clean-room one; needs the registry, COM and a services manager |
| **Portable `.exe` / `.zip` / `.7z`** | unpack + register a launcher entry | archive formats only |
| **Linux `.deb`** | unpack `ar`/`tar`, run maintainer scripts in the POSIX personality | dynamic ELF (`ld.so` + glibc) for scripts and most payloads |
| **`.rpm`, `.apk`, Arch `.pkg.tar.*`** | same transaction model | as above |
| **AppImage / static tarballs** | mount/unpack, register | AppImage needs FUSE-like mounting → just unpack instead |
| **Flatpak / Snap** | out of scope (they assume systemd, bubblewrap, namespaces) — maybe a lightweight "bundle" format of our own later | — |
| **Source builds** | `make`/`cargo` toolchain packages | a compiler toolchain running on THOS (long term) |

## 2. Install transactions (the same for every format)

1. **Fetch/open** the package (local file, download, later a THOS repository).
2. **Scan first**: hand the payload to the **Security Service** (signatures, heuristics,
   quarantine) *before* anything runs — an installer is untrusted code. Verdict
   blocks or warns (secure-desktop prompt).
3. **Run in a recording sandbox**: the installer executes with a private overlay of
   the filesystem and registry; every file/registry/service/shortcut change is
   **recorded**, not applied. A Windows installer that insists on `C:\Program Files`
   writes into the overlay under the NT-view of `\Device\…`.
4. **Show the plan** (what will be written, which privileges, which services/startup
   entries) and ask for consent via the **elevation** path (`elevate()`/secure
   desktop) when admin rights are needed.
5. **Commit atomically**: apply the recorded change set to the real system, write the
   package-db entry (files, registry keys, hashes), update the **integrity
   baseline** so the exec gate trusts exactly what was installed.
6. **Remove/upgrade** use the recorded manifest — a clean uninstall without trusting
   the vendor's uninstaller (which can still be run and recorded the same way).

This gives Windows-style installs *and* Linux-style package management one
audited code path, and makes every install reversible.

## 3. Where things live

- Windows apps: per-app prefix-like roots (`/Windows/…`, `Program Files` mapped into
  the app's own tree) so removing an app removes its tree; shared system DLLs stay in
  `System32`, versioned, never overwritten silently (side-by-side).
- Linux packages: standard hierarchy managed by the package db (`/usr`, `/etc`, …);
  `/etc` files are configuration-protected on upgrade.
- Launchers: `.desktop`-style entries (data, per the customisation principle in
  [`desktop-plan.md`](desktop-plan.md)) so the shell, the launcher and the user's own
  tools all see the same application list.

## 4. Compatibility work this depends on (the real critical path)

| Need | Why | Status |
|---|---|---|
| **Dynamic ELF loader** (`ld.so`, `PT_INTERP`, `dlopen`) + glibc/musl policy | any distro package | not started — decide **glibc-compat vs musl-only** (**P1**) |
| More Linux syscalls, `/proc`, `/sys`, `/dev`, `fork/exec` details, signals | maintainer scripts, package tools | partial |
| Windows **DLL loading from System32**, registry, COM/OLE, services, shell APIs | installers | partial (own `ntdll`/`kernel32` subset) |
| **Wine PE DLLs vs own** (open decision in `roadmap.md`) | breadth of Win32 | open |
| Archive/compression libs (zlib, xz, zstd, bzip2, cab, 7z) | every format | not started |
| **Networking** ([`network-plan.md`](network-plan.md)) | fetching packages | planned |
| Signatures/hashing for repos | trust | partial (SHA-256 exists) |

## 5. Stages

- **S0 — Static payloads**: `thos-pkg` core, package db, transactions, uninstall;
  formats = tar/zip of static binaries; Security Service scan hook. *Milestone:*
  install and remove a static ELF and a static `.exe` with a launcher entry.
- **S1 — Archives & installers-as-extractors**: zip/7z/cab/tar.xz handlers; simple
  self-extracting `.exe`/NSIS in the recording sandbox (needs only file + registry).
- **S2 — `.deb`**: `ar`+`tar` unpack, dependency resolution against a local repo
  index; dynamic ELF loader lands (P1) so real payloads run; maintainer scripts via
  BusyBox `sh`.
- **S3 — `.msi`**: msiexec service; standard actions; a real small `.msi`
  installs in QEMU. Needs registry + COM basics.
- **S4 — Big installers**: Inno/InstallShield-class `.exe` on the broadened Win32
  layer; services; file associations.
- **S5 — Repositories**: signed THOS repo, update service, GUI store (a client of the
  desktop shell), `.rpm`/other formats as thin handlers.
- **S6 — Source-based extras**: toolchain packages, build-in-sandbox.

## 6. Design rules

1. Nothing installs without a **scan + a recorded plan + consent**.
2. The recording overlay means an installer can *never* write outside what the user
   saw — also a defence against malicious installers.
3. Packages are data: the user can inspect, edit, re-pack, or write their own
   handler (customisation principle).
4. Always reversible; the package db is the source of truth for "what is on my disk".
5. CLI first (`thos-pkg install foo.msi`, from the shell), GUI later.

## 7. Open decisions

- **P1** glibc-compat vs musl-only for third-party Linux binaries (most distro `.deb`s
  assume glibc).
- **P2** MSI engine: Wine's `msi.dll` vs clean-room.
- **P3** Recording sandbox mechanism: filesystem/registry overlay in the kernel vs a
  userspace shim in each personality.
- **P4** Own repository format vs consuming Debian/Devuan repos directly.
- **P5** Package-db format and location.

## 8. Verification

`cargo xtask pkg-test`: install/remove a static ELF and a static PE, assert the
package db, launcher entries, integrity baseline and that removal leaves the
filesystem identical (e2fsck clean + file-tree diff). Later: a `.deb` and an `.msi`
fixture installed in QEMU.
