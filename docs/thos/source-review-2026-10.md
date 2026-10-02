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

## 2. Open — real bugs / hazards (not fixed yet)

| # | Finding | Why it matters |
|---|---|---|
| B1 | **Syscalls trust user pointers completely** (`sys_write`, `sys_read`, `user_cstr`, `getdents64`, NT `*(a as *mut ..)` …): no range check, no SMAP/SMEP. A process can read/write kernel memory via `write()`/`read()` or crash the kernel with a bad pointer. | The security story (AV, W^X, capability policy) is moot while any process can poke ring 0. Highest-priority hardening item. |
| B2 | **The production boot runs the whole self-test suite.** `storage_milestone` & co. `expect()` on test files (`/init`, `/pe-hello.exe`, `/crt.exe`, …), write `/thos-created.txt`, hives, quarantine log, and spawn dozens of processes — on every boot, including the interactive one. A real install without those files **panics at boot**; a real user's disk gets test junk. | Blocks any real installer. Needs a `selftest` cargo feature; the default boot must be: bring-up → mount root → login → shell. |
| B3 | `kill(pid, sig)` ignores `pid` and, for SIGTERM/KILL/ABRT, **kills the caller**. Signals (`rt_sigaction`, `sigprocmask`, `sigreturn`) are accepted and ignored; no handlers ever run, no `SIGINT` on Ctrl+C, no `SIGCHLD`. | Job control, `Ctrl+C`, daemons, almost every real program. |
| B4 | `getrandom` is a fixed-seed xorshift (predictable). `cred::rand64` falls back to TSC xorshift (the Acer's Westmere has no RDRAND). | Needs a real CSPRNG (entropy pool + ChaCha20) before TLS / network / package signatures. |
| B5 | `execve` does **no execute-permission check** (`x` bits ignored) and no setuid semantics. | DAC is incomplete for exec. |
| B6 | Files are read **whole into the 32 MiB static kernel heap** on `open`, and every `write()` **rewrites the whole file** to ext2. | Large files (> ~25 MiB) cannot be opened at all; installing big packages is O(n²) and heap-bound. Needs streaming I/O + a page cache + a growable heap. |
| B7 | ext2 limits: a **directory is limited to 12 direct blocks** ("directory full"), 16-bit uid/gid, no timestamps, no symlinks/hard links via syscall, no 64-bit sizes, no journal. | A `/usr/lib` with a few hundred entries cannot be created; no `ln -s`. |
| B8 | `nanosleep` is a 1000-iteration yield spin; `clock_gettime` returns zeros, `time()` a fixed 2025-01-01, no RTC. | Anything time-dependent (TLS certs, make, logs, `date`). |
| B9 | `MSV_FREE`, `VirtualFree`, `HeapFree` free nothing; `HeapAlloc`/`malloc` take **one page-rounded mapping per call**. | Any real Windows program leaks memory quickly. |
| B10 | PS/2 keyboard is polled; under rapid synthetic input (QEMU `sendkey` at ~4 keys/s via a script) characters were duplicated/dropped in two ad-hoc runs although the xtask-driven tests pass. Cause not isolated. | Real hardware reliability → IRQ-driven i8042 (+ IO-APIC) needed. |
| B11 | `console::ascii` has **no umlauts / ß / dead keys / €** (German layout) — a German user cannot type `ä ö ü ß`. Console is also 25x80 fixed for `TIOCGWINSZ`. | Shell usability for the target user. |
| B12 | IO-APIC is never programmed; **no legacy IRQ at all** (keyboard/mouse/serial are polled), no HPET, TSC not used for time. | Needed for the PS/2 mouse/touchpad and a proper timer base. |
| B13 | `pci::find_class` scans **bus 0 only**, first match. | Fine on the Acer's chipset devices; wrong behind bridges (the RX 6600 on the ASRock sits behind a switch). |
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
- Security core is further along than the platform under it: SAK trusted path, `elevate()`, exec gate + isolated Security Service, integrity baselines, per-key registry DAC — but see B1 (pointer trust) and B2 (self-tests in prod).

## 4. Recommended order (hardening first, then the plans)

1. **B2** gate self-tests behind a `selftest` feature (unblocks real installs).
2. **B1** user-pointer validation + SMEP/SMAP (unblocks any security claim).
3. **B3** real signals + `kill`; **B8** RTC/clock; **B4** CSPRNG.
4. Streaming file I/O + page cache + growable heap (**B6**), ext2 directories past 12 blocks (**B7**).
5. Then the product features: network stack, dynamic ELF loader + threads + futex, wide-char Win32 + 32-bit WOW64, the package manager, the desktop.
