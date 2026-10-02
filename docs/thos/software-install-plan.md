# THOS – Installing software (plan)

*Opened 2026-10-02 (user request). Planning only. Goal: a THOS user can install and
run software from **both worlds** — Windows `.exe` / `.msi` installers and Linux
packages — through one coherent, secure, customisable mechanism.*

## 0. Ground truth (from the full source review — see [`source-review-2026-10.md`](source-review-2026-10.md))

The compatibility work already done is **deep in mechanism but narrow in API
surface**, and several platform gaps sit between "THOS runs a test `.exe`" and
"THOS installs real software":

| Layer | What exists | What an installer/package needs and is missing |
|---|---|---|
| Linux ELF | static `ET_EXEC` only, 62 syscalls (many stubs), BusyBox + ~60 applets | **dynamic loader** (`PT_INTERP`, PIE, `dlopen`), file-backed `mmap`, **threads + futex**, real signals, sockets, `/proc` `/sys` `/dev`, symlinks, timestamps/clock |
| Windows PE | PE32+ only; ~130 ANSI entry points (kernel32 31, ntdll 43, msvcrt 35, user32 14, gdi32 6); real objects, SEH, APC, registry, sections | **PE32 (32-bit) / WOW64** — most installers and all old `.msi` are 32-bit; wide-char APIs; `CreateFile` create/write, `FindFirstFile`, `CreateProcess`, `Reg*` (advapi32), shell32/ole32/COM, services, `HeapFree`/`free` that really free |
| Filesystem | ext2: whole-file read on open, whole-file rewrite on write, **12-block directories**, 32 MiB kernel heap | streaming I/O + page cache + growable heap; large directories; symlinks; timestamps |
| Boot | ~~the production boot ran the whole self-test suite~~ — **fixed** (`selftest` feature; see source review F5) | an **installer** that lays down partitions + Limine + the root FS on a real disk |
| Security | exec gate, isolated Security Service, quarantine, SAK, `elevate()` — **but syscalls trust user pointers** | pointer validation + SMEP/SMAP before any "scan then run untrusted installer" claim is true |

So the package *mechanism* can be built early, but "install anything" is gated on
runtime coverage. The plan below therefore builds the transaction machinery first
and widens compatibility underneath it, in the order the review recommends.

## 1. One mechanism, many formats

A single **package manager service** (`thos-pkg`) owns install/remove/upgrade/verify
and a package database; format handlers are plug-ins that translate a foreign
package into a THOS **install transaction**.

| Format | Handler | What it really needs |
|---|---|---|
| **`.exe` installer** (NSIS, Inno Setup, InstallShield, plain self-extractors) | run the installer *inside the NT personality* in a recording sandbox | **WOW64 (32-bit PE32)** — the large majority are 32-bit — plus wide-char Win32, file create/write, registry front end, `CreateProcess`, temp dirs, shell/COM basics. The `ntdll` boundary is decided from-scratch; whether Wine's PE `kernel32`/`kernelbase` sit on top is still open |
| **`.msi`** | **msiexec** service: Windows Installer database (OLE compound file → tables), standard actions, registry/file/shortcut/service tables | MSI engine — Wine's `msi.dll` (LGPL) would itself need Wine's `kernel32`/`ole32`/`advapi32` under it, which cuts against the from-scratch `ntdll` decision; a clean-room engine is the more consistent option (**P2**). Needs WOW64, the registry front end, COM and a services manager |
| **`.apk`** (Android) | the roadmap's Android profile (Waydroid-class: `binder` + Bionic + ART on the Linux personality) — listed here so the package db is one place; far on the critical path | dynamic ELF, threads, `binder`, namespaces-lite — a long way off |
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

## 4. What this depends on (the real critical path, ordered)

1. **Hardening prerequisites** — (`selftest` split done, F5), user-pointer
   validation + SMEP/SMAP (B1), real signals/`kill` (B3), clock/RTC (B8), CSPRNG (B4).
   Without these an "untrusted installer" sandbox is not a boundary and a real
   install panics at boot.
2. **Storage that can hold software** — streaming file I/O, page cache, growable
   kernel heap (B6); ext2 directories beyond 12 blocks, symlinks, timestamps (B7).
3. **Linux runtime** — dynamic ELF loader + `ld.so`/glibc-or-musl policy (**P1**),
   file-backed `mmap`, threads (`CLONE_VM`) + real `futex`, sockets (network plan),
   `/proc` `/sys` `/dev`.
4. **Windows runtime** — **WOW64** (PE32 loader + 32-bit `ntdll` thunk layer),
   wide-char APIs, `CreateFile` (all dispositions) / `FindFirstFile` / `CreateProcess`,
   an `advapi32` registry front end over the existing hives, shell32/ole32/COM
   basics, services, a heap that frees.
5. Archive/compression libraries (zlib, xz, zstd, bzip2, cab, 7z) and signature
   verification (SHA-256 exists; needs RSA/ECDSA/Ed25519).
6. **Networking** ([`network-plan.md`](network-plan.md)) for fetching packages.

## 5. Stages

- **S-1 — Prerequisites** (section 4, items 1–2): pointer validation, signals, streaming I/O. Nothing below is honest without them.
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

## 7. Product constraints already decided elsewhere

- **"Download → it runs, zero setup"** (roadmap, *Product goal*): for simple cases the
  installer must be invisible — opening a `.exe`/`.msi`/`.deb` shows one consent
  screen, not a wizard of options. The recorded-plan view is the *detail* pane.
- **Age/maturity gate** (roadmap, *Age declaration*): the package layer checks the
  content rating at install time and consults the principal's `maturity`.
- **No circumvention** of anti-cheat/DRM attestation; vendor-sanctioned runtimes only.
- **ML/AI is paused** (*user, 2026-10-02*): no assistant-driven install features.

## 8. Open decisions

- **P1** glibc-compat vs musl-only for third-party Linux binaries (most distro `.deb`s
  assume glibc).
- **P2** MSI engine: clean-room (consistent with the from-scratch `ntdll`) vs Wine's `msi.dll` (needs Wine's DLL stack beneath it).
- **P3** Recording sandbox mechanism: filesystem/registry overlay in the kernel vs a
  userspace shim in each personality.
- **P4** Own repository format vs consuming Debian/Devuan repos directly.
- **P5** Package-db format and location.

## 9. Verification

`cargo xtask pkg-test`: install/remove a static ELF and a static PE, assert the
package db, launcher entries, integrity baseline and that removal leaves the
filesystem identical (e2fsck clean + file-tree diff). Later: a `.deb` and an `.msi`
fixture installed in QEMU.
