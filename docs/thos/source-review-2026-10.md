# THOS – Source review, 2026-10-02

*A read-through of the whole kernel tree (`kernel/src/*.rs`, ~22k lines), the
loaders, the xtask test harness and the docs, done to ground the Acer / desktop /
network / software-install plans in what the code really does today. Findings are
split into **fixed in this pass**, **open (needs work)**, and **facts that shape
the plans**. Nothing here is a criticism of the phase order — most gaps are the
expected "not yet" of a young kernel — but several are real bugs.*

## 1. Fixed in this pass

| # | Finding | Fix | Proof |
|---|---|---|---|
| F1 | **No x87/SSE state saved on context switch.** The timer preempts between processes at any instruction; user code (musl, Rust, every Win64 binary) uses `xmm0-15` constantly, so processes silently shared each other's vector registers. | Per-thread 512-byte `FXSAVE` area (`sched.rs`): saved when a thread stops, restored when it runs. | `fputest` (12 forked processes, unique pattern in `xmm0-7`): **12/12 corrupt before, 0/12 after**; part of `cargo xtask kbd-test`. |
| F2 | **`execve` of a non-ELF file panicked the kernel** (`elf::load(..).expect(..)`) — typing `./message` or any text file at the shell prompt. The ELF loader also indexed the file with unchecked offsets (the PE loader never did). | `elf::validate` bounds-checks every header/segment first; `execve` returns `ENOEXEC`. | Code path; `kbd-test` unchanged. |
| F3 | Fixed-LBA AHCI write tests ran on any disk — on a real partitioned disk they would overwrite user data. | Skipped when the root FS is in an MBR partition (`ext2::on_partition`). | `bios-test` log. |
| F4 | `0xAA` (Left-Shift release) was dropped by the new PS/2 decoder → stuck Shift. | Removed from the filter. | `bios-kbd-test`. |
| F5 | **B2 — the production boot ran the whole self-test suite** (and `expect()`ed on test files; a real install would panic at boot and its disk would get test junk). | New cargo feature **`selftest`** (kernel `Cargo.toml`): all milestones/checks/scratch tests compile only with it. A normal boot is now `sched init → ahci → mount root → registry hives → Security Service → input devices → login → shell`, with `fatal_boot()` giving readable errors (no SATA disk / no root FS / no `/busybox`) instead of bare panics. `cargo xtask *-test` still build with `selftest`; the `bios-*` commands build the real boot path (18 s vs. > 60 s to a prompt in QEMU). | `bios-test`, `bios-kbd-test`, `bios-power-test` run the production boot; all legacy tests run with `selftest`. |
| F7 | **B1 — syscalls trusted user pointers completely.** A process could pass a *kernel* address as a `write()` buffer and read kernel memory (the old kernel really dumped kernel code bytes onto the console in the test), pass one as a `read()` buffer and overwrite it, or an unmapped address to fault the kernel. | New `usercopy.rs`: every pointer must lie in the user half and be mapped (and writable where the kernel writes) in the caller's address space, else `-EFAULT`. Applied to all Linux syscalls (read/write/writev/pipe/fstat/newfstatat/ioctl/sendfile/getdents/getcwd/uname/sysinfo/clock_gettime/getrandom/poll/arch_prctl/time/wait4, all path arguments, `execve` argv/envp). NT layer: every simple out-parameter write and read (≈120 sites) now goes through `usercopy::put/get` (validated, bad pointer = no-op / 0), the larger out-buffers (`NtQueryInformationProcess`, `NtQueryVirtualMemory`, `VirtualQuery`, registry key/value queries) are range-checked up front, the msvcrt `memcpy`/`memset`/`vfprintf` va_list reads are validated, Win64 stack arguments are read through checked reads, and the SEH / APC / ring-3-callback code no longer writes to a user-controlled `rsp` unchecked. (A first attempt — rejecting any call with a kernel-half *argument* — was dropped: unused argument registers legitimately hold stale kernel addresses, so it refused valid calls and broke `pe-test`.) A kernel `#PF` on a *user* address now kills the offending process instead of panicking. SMEP is enabled where the CPU has it. | New `ptrtest` in `kbd-test`: 18/18 bad pointers refused with EFAULT, valid calls unaffected. Against the old kernel the same program leaked kernel memory to the console. `bios-test` runs on `-cpu max` and requires `SMEP enabled`. |
| F6 | **ACPI S5 poweroff was silently disabled on ACPI-1.0 firmware** (FADT 116 bytes; QEMU's `-M pc` builds one) — the code required ≥ 132 bytes and returned without a word. Also: `bios-power-test` could not tell ACPI from the emulator fallback (a timing race on a log line), so the earlier "verified via the real ACPI S5 path" was **not actually proven**. | `power::init` accepts ≥ 116-byte FADTs (reset register / X_DSDT stay guarded) and logs every abort path; the test now requires the kernel's `THOS: power ok … SLP_TYP` line. | `bios-power-test` 3/3 runs, log shows `ACPI S5 (PM1a 0x604, SLP_TYP 0/0)`. |

## 2. Open — real bugs / hazards (not fixed yet)

| # | Finding | Why it matters |
|---|---|---|
| B1 | **Fixed (F7)** — remaining hardening: **SMAP** (needs `stac`/`clac` around every user access), and a few NT paths still use the raw pointer inside a block that was range-checked once (fine), but there is no automated audit that *every* new NT call goes through `usercopy`. | Defence in depth; track as B1b. |
| B3 | `kill(pid, sig)` ignores `pid` and, for SIGTERM/KILL/ABRT, **kills the caller**. Signals (`rt_sigaction`, `sigprocmask`, `sigreturn`) are accepted and ignored; no handlers ever run, no `SIGINT` on Ctrl+C, no `SIGCHLD`. | Job control, `Ctrl+C`, daemons, almost every real program. |
| B4 | `getrandom` is a fixed-seed xorshift (predictable). `cred::rand64` falls back to TSC xorshift (the Acer's Westmere has no RDRAND). | Needs a real CSPRNG (entropy pool + ChaCha20) before TLS / network / package signatures. |
| B5 | `execve` does **no execute-permission check** (`x` bits ignored) and no setuid semantics. | DAC is incomplete for exec. |
| B6 | Files are read **whole into the 32 MiB static kernel heap** on `open`, and every `write()` **rewrites the whole file** to ext2. | Large files (> ~25 MiB) cannot be opened at all; installing big packages is O(n²) and heap-bound. Needs streaming I/O + a page cache + a growable heap. |
| B7 | ext2 limits: a **directory is limited to 12 direct blocks** ("directory full"), 16-bit uid/gid, no timestamps, no symlinks/hard links via syscall, no 64-bit sizes, no journal. | A `/usr/lib` with a few hundred entries cannot be created; no `ln -s`. |
| B8 | `nanosleep` is a 1000-iteration yield spin; `clock_gettime` returns zeros, `time()` a fixed 2025-01-01, no RTC. | Anything time-dependent (TLS certs, make, logs, `date`). |
| B9 | `MSV_FREE`, `VirtualFree`, `HeapFree` free nothing; `HeapAlloc`/`malloc` take **one page-rounded mapping per call**. | Any real Windows program leaks memory quickly. |
| B10 | The PS/2 keyboard is **polled** (no IRQ1 via the IO-APIC). The "duplicated / dropped characters" seen in two ad-hoc runs were **not a driver bug**: the test typist did not swap Y/Z for the console's QWERTZ layout and QEMU `sendkey` has no uppercase letters (`thjos` was `thos` typed on the wrong keys). Fixed in `xtask::type_line`. | Still worth moving to IRQ-driven input (B12) for latency and to avoid a busy poll thread. |
| B11 | `console::ascii` has **no umlauts / ß / dead keys / €** (German layout) — a German user cannot type `ä ö ü ß`. Console is also 25x80 fixed for `TIOCGWINSZ`. | Shell usability for the target user. |
| B12 | IO-APIC is never programmed; **no legacy IRQ at all** (keyboard/mouse/serial are polled), no HPET, TSC not used for time. | Needed for the PS/2 mouse/touchpad and a proper timer base. |
| B13 | `pci::find_class` scans **bus 0 only**, first match. | Fine on the Acer's chipset devices; wrong behind bridges (the RX 6600 on the ASRock sits behind a switch). |
| B15 | **AHCI NCQ error recovery occasionally hangs** (`cargo xtask ncq-error-test`: the deliberately poisoned read never returns in ~1 of 4 runs). **Pre-existing**: the original `d123fcf` fails 2/8 runs identically, before any of this review's changes. The hang is in `ahci::read` → `recover()` after the injected `blkdebug` error (the kernel is idle after `ext2 unlink ok`, never reaching `ncq error ok`). | Real-hardware reliability: a disk error that wedges the port instead of failing the read. Needs a root-cause pass on `recover()` / `PENDING` / `TAG_WAKE` (a lost wake-up is the likely suspect). |
| B14 | No `Drop`-time/`shutdown` flush path: ext2 writes are individually durable, but there is no unmount, no orderly service stop, other CPUs are not halted at poweroff. | Orderly shutdown story. |

## 3. Facts that shape the plans

### POSIX personality (what a Linux program can actually do today)
- **ELF:** static `ET_EXEC` only. No `PT_INTERP`, no PIE (`ET_DYN`), no dynamic loader, no `PT_TLS` handling beyond what static musl does itself.
- **Syscalls:** 62 dispatched, of which many are stubs (`mprotect/madvise/munmap/futex/prctl/sigaction…` return 0; `waitid` ECHILD; `readlink` EINVAL; `uname` zeros; `sysinfo` zeros).
- **Memory:** `mmap` = anonymous only (flags/prot/fd ignored); `munmap` is a no-op; `brk` real; no COW, `fork` copies eagerly.
- **Threads:** `clone(CLONE_VM)` → ENOSYS. `futex` is a stub. So no pthreads.
- **No** sockets, `/proc`, `/sys`, `/dev` (no `/dev/null`, `/dev/tty`, `/dev/urandom`), pty, symlinks, `select/epoll` (poll is a stub), `fchdir`, file timestamps.
- **Works well:** fork/exec/wait, pipes with real blocking, per-process cwd, close-on-exec, dup*, `getdents64`, DAC owner/group/other, `O_CREAT`, `chmod/chown`, ext2 create/unlink/mkdir/rmdir, BusyBox ash + ~60 applets.

### NT personality (what a Windows program can do today)
- **API surface:** `kernel32` 31 functions, `ntdll` 43, `msvcrt` 35 (+3 data), `user32` 14, `gdi32` 6 — i.e. ~130 entry points, **ANSI (`A`) only, no wide-char APIs**, no `advapi32`/`shell32`/`ole32`/`ws2_32`/`ucrt`.
- **Notable gaps for installers:** `CreateFileA` is `OPEN_EXISTING` only (cannot create/write a file); no `FindFirstFile`, `GetModuleFileName`, `GetTempPath`, `CreateProcess`, `RegOpenKeyEx…` (the registry exists in the kernel but has no Win32 front end), no services, no COM, no `CreateThread` beyond one worker, `TlsAlloc` missing, critical sections are no-ops, `printf` prints `<float>` for floats.
- **Loader:** PE32+ (x86-64) only; imports/relocs/TLS/exports/forwarders/DllMain/delay-less all real; **PE32 (32-bit) is rejected** (`Machine != 0x8664`) — WOW64 is planned but not started. Most `.exe`/`.msi` installers in the wild are 32-bit.
- **Real and solid (mechanism-wise):** executive objects (event/semaphore/mutant/section/key), multi-object waits, APC and SEH delivery to ring 3, ring-3 callbacks (WndProc), `\Device\` + drive-letter namespace, hive-backed registry with per-key DAC and change-notify, W^X `VirtualProtect`, process teardown.
- **Decision on record:** the `ntdll` boundary stays **from scratch** (not Wine's unixlib/wineserver). Whether Wine's PE `kernel32`/`kernelbase`/`user32` DLLs are layered on top remains open — it would require a much larger `Nt*` surface than exists.

### Platform
- Scheduler: one global ready queue + one lock (fine to ~dozens of cores), no priorities/classes yet, 16 KiB kernel stacks, idle = `sti;hlt`.
- Memory: free-list frame allocator, no zones/NUMA, 1 MiB DMA arena (32 tags x 32 KiB), kernel heap 32 MiB static.
- Boot/BIOS: now Limine BIOS + MBR (Acer) or UEFI + GPT (ASRock); the UEFI boot picker (`loaders/thos-boot`) is not part of the BIOS path.
- Drivers present: AHCI (NCQ, MSI/MSI-X, error recovery), xHCI (keyboard only, no descriptor parsing), PS/2 keyboard, 16550 serial, LAPIC/PIT. **Absent:** EHCI, mouse/touchpad, NIC, audio, GPU, RTC, HPET, IO-APIC, CSPRNG.
- Security core is further along than the platform under it: SAK trusted path, `elevate()`, exec gate + isolated Security Service, integrity baselines, per-key registry DAC — but see B1 (pointer trust).

## 4. Recommended order (hardening first, then the plans)

1. ~~B2 `selftest` feature~~ — done (F5).
2. ~~B1 user-pointer validation + SMEP~~ — done (F7); SMAP and per-site NT checks remain (B1b).
3. **B3** real signals + `kill`; **B8** RTC/clock; **B4** CSPRNG.
4. Streaming file I/O + page cache + growable heap (**B6**), ext2 directories past 12 blocks (**B7**).
5. Then the product features: network stack, dynamic ELF loader + threads + futex, wide-char Win32 + 32-bit WOW64, the package manager, the desktop.


---

## Status update — 2026-10-03

Resolved or substantially advanced since the review (see `night-report-2026-10-03.md` for the evidence):

| Finding | State |
|---|---|
| **B1** user pointers | done (`usercopy.rs`); per-site NT checks and SMAP (B1b) still open |
| **B3** signals | **done**: handlers on a Linux `rt_sigframe`, masks, `SA_RESTART`, EINTR, Ctrl+C → SIGINT, process groups, `alarm`/`setitimer`; per-thread signal state still open |
| **B4** CSPRNG | **done** (entropy pool + ChaCha20, fast key erasure); AT_RANDOM now random |
| **B6** big files | **partly**: read-only files > 256 KiB are streamed (`Ext2Stream`), writes are write-back with `fsync`, ext2 allocation is batched (1 MiB write 28 s → 1.7 s). Still open: a growable kernel heap, truly incremental ext2 writes (each flush still rewrites the whole file), files > 64 MiB on 1 KiB-block images (no triple-indirect) |
| **B8** clock | **done** (RTC + TSC clock, real `nanosleep`, `sysinfo`, ext2 timestamps) |
| **B10/B12** input IRQs | **done for PS/2**: the I/O APIC is programmed, keyboard (IRQ 1) and mouse (IRQ 12) are interrupt-driven with a polling safety net; HPET and a TSC-deadline timer still open |
| **B15** AHCI hang | open (`ncq-error-test` still flaky) |
| new **B16** | `smp-test` hang — **resolved (2026-10-03): a race in the test, not the scheduler**: the wave `mark` was computed after spawning, so when churn threads finished during the spawn loop it ended up above the number of threads that exist; it is now taken before the spawn (5/5 green). The frame-count baseline was hardened separately |
| new **B17** | BusyBox `ps` forked from the shell crashes in `__run_exit_handlers` at exit (BusyBox-specific; plain glibc fork/atexit works) |

New capabilities worth recording: dynamically linked PIE programs with `ld.so` (glibc 2.41 runs), POSIX threads
(`clone(CLONE_THREAD)` + futex), BSD sockets over virtio-net + smoltcp with DHCP, virtual `/dev` and `/proc`,
PS/2 mouse (`/dev/input/mice`).
