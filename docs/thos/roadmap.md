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
  - **PEB / TEB + `gs` base done.** `pe::load` allocates + maps a TEB and PEB
    page for the process (`gs:[0x30]` self, `gs:[0x60]` PEB, `NT_TIB` stack
    bounds, `PEB.ImageBaseAddress`); the thread carries the TEB as its `%gs`
    base. `apply_cpu_state` programs `IA32_KERNEL_GS_BASE` per thread (per-CPU
    pointer for POSIX/kernel threads, the TEB for a PE thread) and skips the
    `wrmsr` when the value is unchanged — a POSIX→POSIX switch costs nothing, so
    the existing `swapgs`/`gs:0` per-CPU discipline is untouched. PE threads are
    **cooperatively scheduled for now** (`IF=0` in ring 3) so a ring-3 IRQ
    `swapgs` shim isn't needed yet. `pe-test` reads `gs:[0x30]→[+0x60]→[+0x10]`
    (TEB → PEB → ImageBaseAddress) before its `WriteFile` line, so a broken `gs`
    / TEB / PEB would fault it.
  - **Imports + NT syscall surface done (P1):** `pe::load` walks the Import
    Directory Table, resolves each by-name thunk against a builtin resolver, and
    patches the IAT in place (post-relocation); unresolved names / ordinals are
    rejected, not left dangling. A **shared NT stub page** is mapped into every
    PE process at a fixed high address — one 16-byte trampoline per NT call
    (`mov eax, NT_BASE|idx; mov r10, rcx; syscall; ret`). `rax` values in the
    `NT_BASE` (`0x4E540000`) range route to **`nt::dispatch`** (`nt.rs`), which
    reads the Win64 arg registers (`r10`=former `rcx`, `rdx`, `r8`, `r9`, then
    the stack) off the `UserFrame` and marshals onto THOS objects.
  - **`kernel32` file I/O implemented:** `GetStdHandle`, `WriteFile`,
    `ReadFile`, `CreateFileA` (read-only `OPEN_EXISTING`; a Windows path is
    mapped `X:\a\b` → `/a/b`), `CloseHandle`, `GetLastError` / `SetLastError`
    (`TEB.LastErrorValue` at `gs:[0x68]`, reached through the thread's saved
    `%gs` base), `ExitProcess`. Win64 stack args are read at `[rsp+0x28]`+ off
    the stub frame; `nt::dispatch` marshals all of them onto THOS's fd table +
    ext2. The `pe-test` `.exe` prints line 1 via a raw `syscall` (DIR64-relocated
    absolute pointer), line 2 via `WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),…)`,
    then **opens `C:\pe-read.txt` with `CreateFileA`, reads it with `ReadFile`,
    and echoes it with `WriteFile`** — a PE process doing real Win32 file I/O on
    THOS objects — then `ExitProcess(0)`, all through the IAT.
  - **PEB `Ldr` + `ProcessParameters` + memory:** a parameter page per PE
    process holds `RTL_USER_PROCESS_PARAMETERS` (std handles, `ImagePathName` /
    `CommandLine` / `CurrentDirectory` as `UNICODE_STRING`s, an empty
    environment) and a `PEB_LDR_DATA` + one circular `LDR_DATA_TABLE_ENTRY` for
    the exe (`DllBase` / `EntryPoint` / `SizeOfImage` / names), wired at
    `PEB+0x20` / `PEB+0x18`. `PEB+0x30` = `ProcessHeap`. Stubs:
    `GetCommandLineA`, `GetModuleHandleA(NULL)`→ImageBase, **`VirtualAlloc`** /
    `VirtualFree` / `VirtualProtect` (anon zeroed mapping via `mmap_anon`; RWX
    for now), **`GetProcessHeap` / `HeapAlloc` / `HeapFree`** (one anon mapping
    per alloc — a real allocator comes later). `pe-test` `VirtualAlloc`s a page
    and `HeapAlloc`s a block and writes to both (`PE VirtualAlloc+Heap OK`).
  - **Synthetic `kernel32.dll` + dynamic resolution:** a one-page PE32+ image at
    `NT_STUB_BASE+0x4000` — minimal headers, a second copy of the NT
    trampolines, and a real `IMAGE_EXPORT_DIRECTORY` whose
    `AddressOfFunctions[i]` is trampoline `i` and `AddressOfNames[i]` is
    `nt::NT_EXPORTS[i]` (the one list `resolve_import` also indexes, so imports
    and exports cannot drift). It is threaded into all three `PEB->Ldr` module
    lists as a second `LDR_DATA_TABLE_ENTRY` (`BaseDllName` `KERNEL32.DLL`).
    `GetModuleHandleA(name)` / `LoadLibraryA` walk `Ldr` (`ldr_find`,
    case-insensitive, `.dll` implied); `GetProcAddress` parses any module's
    export directory (`ExportDir::parse`) by name or ordinal, rejecting
    forwarder RVAs. `pe-test` does `LoadLibraryA("kernel32.dll")` →
    `GetProcAddress(h,"WriteFile")` → calls the resolved pointer
    (`PE GetProcAddress OK`).
  - **`ntdll` boundary:** `nt::dispatch` is now two personalities over one set
    of cores. `dispatch_ntdll` is the native `Nt*`/`Ldr*` layer — NTSTATUS
    returns, `IO_STATUS_BLOCK` out-params, in-out pointers: `NtClose`,
    `NtWriteFile`, `NtReadFile`, `NtAllocateVirtualMemory`,
    `NtFree`/`ProtectVirtualMemory`, `NtTerminateProcess`,
    `LdrGetProcedureAddress`, `LdrLoadDll`. `dispatch_kernel32` is the Win32
    shim (BOOL / `LastError` / fd-as-HANDLE) and now *calls* the shared cores
    (`file_write_core` / `file_read_core` / `handle_close_core` /
    `mem_alloc_core` / `proc_terminate`). Selector bit `NT_NTDLL_FLAG` in a
    stub's index routes between them. A second synthetic module `ntdll.dll`
    (one page, `map_synth_dll`) is threaded into the `Ldr` lists as the third
    `LDR_DATA_TABLE_ENTRY`. The standalone stub page is gone — `resolve_import`
    returns the trampoline VA inside the owning module page, so a bound IAT
    slot and a `GetProcAddress` result agree. `pe-test`:
    `GetModuleHandleA("ntdll.dll")` → `GetProcAddress(h,"NtWriteFile")` → a
    real 9-arg NT call (`PE ntdll OK`).
  - **Real on-disk DLLs from `C:\Windows\System32`:** `pe.rs` is built around a
    per-`load()` `Loader`. `stage_image(file, want_base)` parses + materialises
    + relocates one image (exe or DLL) without mapping or binding. The
    `Loader` seeds the two synthetic modules with their export maps, owns a
    bump arena of VA space (`PE_DLL_ARENA`), and resolves imports:
    `resolve_module` = synthetic hit / already-loaded / read
    `/Windows/System32/<name>` from ext2 and `load_dll` it; `load_dll` stages
    into the arena, `parse_exports` from the image, registers **before**
    recursing (so an import cycle terminates), binds its own imports, then
    maps its segments; `bind_imports` walks the IDT and patches each IAT slot
    with the real export VA (synthetic trampoline **or** on-disk `.text`),
    depth-capped. `pe-test` ships a real PE32+ `thoscrt.dll` (exports
    `thos_add`, itself imports `KERNEL32!GetLastError`) at
    `C:\Windows\System32`; the exe imports `thoscrt!thos_add`, calls it, and
    asserts `42` (`PE dll thos_add=42`). `DllMain` is not run yet.
  - **DLLs in the PEB `Ldr` lists:** `map_teb_peb` now writes a real
    `LDR_DATA_TABLE_ENTRY` for every on-disk DLL into all three module lists
    (after the exe and the synthetic pair), in a dedicated `PE_LDRDATA` page;
    a `link_ring` helper threads each circular list across both pages. Runtime
    `GetModuleHandleA` / `LoadLibraryA` / `GetProcAddress` resolve against
    System32 DLLs, not just the synthetic ones — `pe-test` looks up
    `thoscrt!thos_add` at runtime and calls it (`PE dll Ldr OK`). Capped at 12
    file DLLs (multi-page region later).
  - **`DllMain` before the exe entry:** when a loaded DLL has an entry point,
    `load` builds a one-page ring-3 **process-bootstrap** (`PE_BOOTSTRAP_ADDR`)
    and starts the thread there — a small loop that calls each
    `DllMain(hinst, DLL_PROCESS_ATTACH, 1)` in dependency order, then jumps to
    the real exe entry (Windows does this in `LdrpInitializeProcess`; THOS has
    no user-mode loader yet, so the kernel emits the shim). `pe-test`'s
    `thoscrt.dll` has a real `DllMain` that gates `thos_add` behind a sentinel,
    so `thos_add(40,2) == 42` now also proves `DllMain` ran first. Return value
    not yet acted on.
  - **Toward real (Wine-sourced) DLLs.** Dropping a real `kernel32` / `ntdll` /
    `user32` into `C:\Windows\System32` needs loader features that the
    hand-built test DLLs don't exercise; each lands with its own synthetic
    test:
    - **import by ordinal** — *done.* `LoadedModule` now models the export
      table (`eat` by `ordinal - ord_base`, `names` → index); a thunk with the
      ordinal flag resolves via `by_ordinal`. `pe-test` imports
      `thoscrt!thos_mul` by ordinal 2 (`PE dll ordinal OK`).
    - **forwarder exports** — *done.* `LoadedModule.eat` is `Vec<Export>`
      (`Empty` / `Addr` / `Forward("Dll.Func")`); `Loader::resolve_export_idx`
      follows a forwarder (`resolve_module` → recurse), and `nt.rs`'s
      `resolve_slot` does the same at runtime for `GetProcAddress`. `pe-test`'s
      `thoscrt!thos_fwd` forwards to `KERNEL32.GetProcessHeap` (`PE dll forward OK`).
    - **TLS directory** (data dir 9) — *done.* `stage_image` reads the
      `IMAGE_TLS_DIRECTORY` post-reloc; `Loader::tls_add_module` copies each
      module's TLS template into a per-thread block on the `PE_TLS_ADDR` page,
      writes `ThreadLocalStoragePointer[idx]` + the module's `AddressOfIndex`,
      and queues its callbacks. `map_teb_peb` sets `TEB+0x58`; the ring-3
      bootstrap runs TLS callbacks before `DllMain`s. `pe-test` verifies via
      `gs:[0x58]` (`PE TLS OK`). Still static-only — no `TlsAlloc`, one thread.
    - **`DllMain` returning `FALSE`** — *done.* The bootstrap loop tests `eax`
      after each `DllMain` (not TLS callbacks) and, on `FALSE`, calls
      `ExitProcess(0x135)` inline instead of jumping to the exe entry.
      `pe-test`'s `failcrt.dll` aborts init so `pe-dllfail.exe`'s entry never
      runs (`THOS: pe dllfail ok`).
  - **`ntdll` lower boundary — started.** `NTDLL_EXPORTS` is now framed as
    THOS's **SSDT**: the stub index *is* the service number, `dispatch_ntdll`
    the table-driven switch. First query primitives a real `ntdll` startup
    touches: `NtQueryInformationProcess(ProcessBasicInformation)` (→ PEB, the
    bootstrap call — `pe-test` `PE NtQIP OK`), `NtQueryVirtualMemory`
    (`MemoryBasicInformation`, placeholder region), `NtSetInformationThread` /
    `NtSetInformationProcess` (accept-all).
  - **Waitable objects + unified HANDLE table — started.** `Task.fds` now holds
    a `HandleObject` per slot — `File(Arc<dyn FileOps>)` or
    `Event(Arc<wait::Event>)` — so a POSIX fd and a Win32 `HANDLE` are the same
    integer into the same table (the "one object, many views" model; no tag
    bits). `NtCreateEvent` allocates in that table; `NtClose` / `CloseHandle`
    close any handle; `NtWaitForSingleObject` (NULL timeout → block, `0` →
    poll), `NtSetEvent` / `NtResetEvent` (`PreviousState`) resolve through
    `current_event`. `pe-test`: create → poll `TIMEOUT` → set → poll `WAIT_0` →
    block (already set) `WAIT_0` → close (`PE event OK`).
  - **Auto-reset + relative timed wait.** `wait::Event` gains
    `EventMode { Manual, Auto }`; auto-reset releases one waiter per signal and
    self-clears, via the race-free `WaitQueue::wake_one_or`. `NtCreateEvent`
    honours `EventType`. `NtWaitForSingleObject` with a negative `*Timeout`
    spins a bounded number of yields (a PE syscall runs IF=0, so no tick clock
    / no safe block yet) and returns `STATUS_TIMEOUT` (`pe-test` `PE evt2 OK`).
  - **SEH ↔ trap dispatch.** `crate::seh`: a ring-3 CPU fault in a PE process
    is delivered like on Windows. Two GPR-saving stub shapes —
    no-error-code (`#UD`, `#DE`) and error-code (`#GP`, `#PF`) — converge on
    `thos_fault_common` → `thos_fault_dispatch`, which (if the process armed a
    vectored handler) has `deliver` write an `EXCEPTION_RECORD` + x64 `CONTEXT`
    onto the user stack and re-point execution at a `KiUserExceptionDispatcher`
    stub page; `#PF` fills the record's params (access type + faulting VA from
    CR2). The dispatcher calls the handler
    (`RtlAddVectoredExceptionHandler`, one slot for now) and either
    `NtContinue`s (kernel rebuilds the frame, `iretq`, IF kept 0) or
    `NtTerminateProcess`es; no handler ⇒ the process is killed. `pe-test` arms
    a handler and resumes through a `ud2` (`PE SEH OK`) and a `mov al,[0]`
    (`PE SEH2 OK`). Frame-based `.pdata` / `.xdata` SEH is then just a smarter
    `KiUserExceptionDispatcher`.
  - **User-mode APC delivery.** `crate::apc`: `NtQueueApcThread` appends a
    per-task `ApcEntry`; `NtTestAlert` (and the `TestAlert` tail of
    `NtContinue`) delivers it by staging a `CONTEXT` on the user stack with the
    APC params in its `P1..P4` home area and redirecting the thread through a
    `KiUserApcDispatcher` stub page (`pe::map_apc_page`). The dispatcher calls
    the routine then `NtContinue(&ctx, TestAlert=TRUE)`, so a run of queued APCs
    unwinds one dispatcher call at a time before the interrupted code resumes.
    `pe-test` queues an APC to itself, `NtTestAlert`s, checks the handler ran
    (`PE APC OK`). Kernel-mode APCs / alertable `NtWaitForSingleObject` land
    with the timer wheel. `QueueUserAPC` (Win32) layers straight on top.
  - **Configuration registry, grown to persisted hives.** `crate::registry`:
    one global key tree of typed values addressed by `\`-separated path,
    seeded with `\Registry\Machine` + `\Registry\User` on first use. Backs
    `NtCreateKey`, `NtOpenKey`, `NtSetValueKey`, `NtQueryValueKey`
    (`KeyValuePartialInformation`), `NtDeleteKey`, and **`NtEnumerateKey` /
    `NtEnumerateValueKey`** (`KeyBasicInformation` / `KeyValueBasicInformation`,
    index-based, `STATUS_NO_MORE_ENTRIES` past the end — the real
    `RegEnumKey`/`RegEnumValue` two-call pattern) on the SSDT;
    `OBJECT_ATTRIBUTES.ObjectName` (+ `RootDirectory`) resolves to a path. A key
    HANDLE is a `HandleObject::RegKey(path)` in the unified table.
    **Each top-level root (`machine\software`, `machine\system`, `user`) is a
    hive** — its own backing file under `/etc/thos/registry/*.hiv` on ext2,
    loaded once at boot before anything can touch the registry, rewritten on
    every mutation under it (a from-scratch text format, not
    regf-binary-compatible — THOS only reads its own hives). `pe-test`
    round-trips create → set → close → reopen → query → delete → reopen-fails
    (`PE registry OK`); `registry_enum_check()` (every boot) exercises
    enumeration order + `STATUS_NO_MORE_ENTRIES` directly. Verified with a
    real two-boot round trip on one disk image (boot 1 writes the hive, boot 2
    loads it back). Still not transactional (a crash mid-write can lose that
    one write) and no change-notify / per-key security — those come with the
    security phase.
  - **Mutant + semaphore + `NtWaitForMultipleObjects`.** `wait.rs` gains a
    counting `Semaphore` and a recursive thread-owned `Mutant`; `process.rs` a
    polymorphic `Waitable` (event / semaphore / mutant) with
    `try_take`/`wait`/`is_signaled`. SSDT: `NtCreateMutant` / `NtReleaseMutant` /
    `NtCreateSemaphore` / `NtReleaseSemaphore` / `NtWaitForMultipleObjects`
    (WaitAny → `STATUS_WAIT_0 + i`; WaitAll acquires all when all signalled).
    `NtWaitForSingleObject` now waits on any `Waitable`. `pe-test` `PE sync OK`.
  - **Executive timer wheel + `NtDelayExecution`.** `crate::timer`: a monotonic
    tick clock (CPU-0-driven, ~100 Hz) + a wheel. `sleep_until` does a clean
    `block_current()` from a cooperative PE syscall; the timer IRQ fires on
    whatever runs next with `IF=1` (another thread / a CPU's `sti;hlt` idle) and
    wakes the sleeper. `NtDelayExecution` (negative interval) is a real block;
    `NtWaitFor*` relative timeouts use a real wall-clock deadline. `PE delay OK`.
  - **Thread creation.** `pe::spawn_thread` makes one ring-3 worker per PE
    process: shares the address space + `Task`, own TEB (`PE_TEB2_ADDR`) + 32 KiB
    stack + kernel stack, enters a `PE_THREADSTART_ADDR` stub with `[routine]
    [arg]` on its stack → calls the routine → `NtTerminateThread`. `current_tid`
    (`sched::current().id`) distinguishes threads within a process; a `THREAD_EXITS`
    map gives each worker a manual exit event, so `NtWaitForSingleObject` on the
    returned handle completes on exit. `PE thread ran` / `PE thread OK`.
  - **Section objects.** `HandleObject::Section` — backed by real physical
    frames (`process::Section`), not a copy: `NtMapViewOfSection` maps a
    section's own frames into the caller's page tables, so every view of one
    section (same process or a different one) shares the same memory — a
    write through one view is visible through all the others immediately, at
    the MMU, with no copy. Anonymous sections start zeroed; file-backed ones
    seed from the file's bytes at create time and keep the file open so
    `NtFlushVirtualMemory` (or an implicit flush on `NtUnmapViewOfSection`)
    can write the current bytes back to it. `Process::unmap_view` tears the
    PTEs down (`vmm::unmap_page_in`, `invlpg`'d) without freeing the shared
    frames — a section's frames outlive any one view. `PE section OK`;
    `section_sharing_check` proves cross-view write visibility and genuine
    ext2 writeback on real boot (`ext2-test`).
  - **`Ext2File`** (`file.rs`): the missing piece that makes file-backed
    writeback real — a `FileOps` impl for actual ext2 regular files
    (read/write, whole-file-rewrite-on-write via `Ext2::write_path`, same
    "rewrite it all, synchronously" pattern as the registry hives). Replaces
    the old read-only `MemFile` at the one call site (`syscall::open_resolved`,
    shared by POSIX `open`/`openat` and `CreateFileA`) — every open handle,
    including the ones `NtCreateSection` seeds a section from, is now
    genuinely writable.
  - **Done since:** the ring-3 IRQ `swapgs` shim (PE threads run `IF=1`,
    preemptible — `3aabdbd`); a fully-blocking timed object wait (dual-enqueue:
    object queue + timer wheel — `33a5c79`); a real **multi-object wait-block**
    for `NtWaitForMultipleObjects` (`wait::wait_any_until` parks the thread on
    every involved object's queue at once, locked in fixed address order so an
    overlapping wait set can't deadlock, instead of the old re-poll-every-tick
    loop — `b42e8eb`; `multi_wait_milestone` proves WaitAny/WaitAll both block
    and wake correctly with two threads on an overlapping set concurrently);
    a scheduler stale-`ctx`/phantom-ready-queue race found and fixed
    (`db9e59f`, `1c2358a` — smp-test 46/46 incl. under host load); the
    **registry** grown to persisted hives (three `.hiv` files under
    `/etc/thos/registry`, loaded once at boot, rewritten synchronously on
    every mutation — verified with a genuine two-boot load-from-disk
    round-trip — `6cb2c10`); **shared-writeback sections** (above).
  - **Decided: the Wine/`ntdll` boundary.** Staying on the from-scratch
    `ntdll` — not Wine's `__wine_unix_call` unixlib + a wineserver-equivalent.
    Wine isn't a component that bolts on: it brings its own object model, its
    own wineserver IPC protocol, its own view of processes/handles/sync
    objects, all of which would need reimplementing on THOS's executive
    anyway — no less work than the from-scratch path, just against someone
    else's moving target instead of THOS's own design, and it would mean
    discarding a `ntdll` that already works (SSDT dispatch, PE loader,
    sections, hive registry, multi-object wait, real mingw-CRT programs
    running to exit).
  - **GDI32/User32 skeleton** (new synthetic `gdi32.dll`/`user32.dll`,
    alongside `kernel32`/`ntdll`/`msvcrt`, same trampoline-page mechanism):
    `GetDC`/`ReleaseDC`/`GetSystemMetrics`, `GetStockObject`/
    `CreateSolidBrush`/`SelectObject`, `SetPixel`/`GetPixel`/`Rectangle` —
    real pixel writes into the boot framebuffer (mapped into THOS's own
    tables via `vmm::map_mmio`, same VA Limine used), not a stub. One DC (the
    whole screen), a brush is just a colour — no window objects, no
    compositor, no WndProc callback yet. `gdi_paint_check` (kernel-internal,
    same rationale as `registry_enum_check`/`section_sharing_check`) verifies
    fill bounds, the SetPixel/GetPixel round-trip, and brush/select-object
    semantics against the real framebuffer on boot.
  - **The ring-3 callback mechanism.** The piece real windows need —
    calling from a kernel syscall handler into arbitrary ring-3 code and
    getting a return value back *within that same syscall* — is built and
    proven: `CallWindowProcA` (`dispatch_user32`) stashes the calling
    syscall's own `UserFrame` per-thread (`process::push/pop_callback_frame`,
    LIFO — a callback that triggers another nests correctly), builds a fresh
    ring-3 call frame on the caller's own (otherwise idle) stack — real
    args in `rcx`/`rdx`/`r8`/`r9`, return address pointing at a small new
    trampoline page (`pe::PE_CALLBACK_RETURN_ADDR`) — and resumes into it
    via `seh::thos_exc_resume` (the same IRETQ-based primitive `NtContinue`
    already uses to resume an arbitrary saved context). The trampoline runs
    *after* the ring-3 function returns, on the caller's stack, and calls a
    new `NtCallbackReturn` (`ntdll`, index 41), which pops the stashed frame,
    writes the callback's result into its `rax`, and resumes *that* via
    `syscall::thos_user_resume` — so the original syscall (`CallWindowProcA`)
    ends up returning the callback's value, exactly as if it had done the
    call itself. One real bug found and fixed en route: a stashed
    `syscall::UserFrame` carries garbage in `.cs`/`.ss` (dead slots on the
    normal `sysretq` fast path — `thos_user_resume`'s IRETQ needs both, and
    without filling them in first, resuming faulted with `#GP`). Verified
    with a genuine ring-3 round-trip (`CallWindowProcA` calling an inline
    callback in the test PE's own code, computing `msg+wParam-lParam` and
    handing the result back) — this is inherently a real-CPU-privilege-
    transition test, not something a kernel-internal Rust check can stand
    in for, so (unlike sections/registry/multi-wait) it's a hand-assembled
    `pe-test` scenario, printing `PE callback OK`.
  - **Real windows** (`window.rs`, new): `RegisterClassA` (class name →
    `WndProc`), `CreateWindowExA` (→ `HWND`, queues its own `WM_CREATE`),
    `PostMessageA`/`PostQuitMessage` and a genuine per-thread message queue
    (`GetMessageA` really blocks on it — `WaitQueue`, same pattern as the
    console's input queue), `DispatchMessageA`/`UpdateWindow` driving the
    target `WndProc` for real, in ring 3, through the callback mechanism
    above (`UpdateWindow` *sends* — a direct call, bypassing the queue,
    matching real Win32). `ShowWindow`/`DefWindowProcA`/`TranslateMessage`
    are still close to no-ops (no compositor, no default painting, no
    keyboard→message pipeline yet) — deliberately: this increment was the
    message-loop plumbing, not window rendering.
    Verified with a real message-loop round trip in `pe-test`
    (`RegisterClassA` → `CreateWindowExA` → `PostMessageA` a custom message
    → a real `GetMessageA`/`DispatchMessageA` loop drives the test's own
    `WndProc`, which calls `PostQuitMessage` with the message's `wParam` →
    the loop exits on `WM_QUIT` → the `MSG`'s `wParam` is checked to have
    survived the whole round trip) — prints `PE window OK`.
  - **Window-relative GDI.** `GetDC(hwnd)` for a real window now returns a
    DC tagged with that `hwnd` (`gdi::WINDOW_DC_TAG`, distinct from the
    fixed screen DC `1` `GetDC(0)` still returns) instead of always meaning
    "the whole screen". `gdi::resolve_dc` turns a tagged DC into an origin
    (the window's `(x, y)`, from `window::rect_of`) and a clip rectangle
    (the window's rect intersected with the screen) that `SetPixel`/
    `GetPixel`/`Rectangle` all go through — client-relative coordinates in,
    real (offset, clipped) screen pixels out, same as real Win32. Each DC
    also got its own current-brush colour (`gdi::BRUSHES`, keyed by DC
    handle) instead of the one global the whole-screen-only version had —
    needed the moment more than one DC (screen + any window) can exist at
    once. Verified by extending `gdi_paint_check` (kernel-internal, same
    rationale as the earlier GDI/registry/section checks) with a real
    window: drawing through its DC lands at the window's screen origin, not
    the screen DC's own origin or the window's own drawing leaking outside
    its rect — passes on real boot.
  - **Process address-space teardown**, closing a gap `process.rs`'s own
    module doc used to flag ("no address-space teardown (a reaper frees the
    frames later)"): every process's PML4/page-table frames and image/stack/
    heap pages now genuinely go back to `FRAME_ALLOC` when it exits or
    `execve`s away, instead of leaking for the rest of the boot. A dead
    process's memory not lingering (rather than sitting there, freeable by
    accident, potential fodder for a use-after-free elsewhere) is itself
    part of what "process isolation" means, alongside the per-process page
    tables THOS already had.
    - `Process::teardown`: unmaps every section view first (giving back the
      PTEs, never the frames — those belong to the `Section`, still possibly
      live elsewhere), then walks PML4[0..256] (the user half only) freeing
      every present leaf frame plus the PT/PD/PDPT frames that mapped them,
      then the PML4 itself.
      **Caller's responsibility, not the function's**: the process's
      `pml4_phys` must not be the live CR3 on *any* CPU, now or ever again.
      Two call sites establish that: `execve`, right after its own explicit
      `Cr3::write` off the old space (`Task::swap_space`, not `set_space` —
      keeps the old `Process` alive until *this* code chooses to drop it,
      not whenever the assignment happens to run); `sched::reap`, once
      `Task::thread_exited` reports a task's last thread gone (not the
      `Task`'s own `Arc` strong count — always >= 2 for its whole life,
      since `TASKS` holds a permanent reference until some parent `wait4`s
      it, which plenty of THOS's test-spawned processes never get; a new
      `active_threads` counter, incremented by every `sched::spawn_user*`,
      is the real signal for "no thread of this process can still be
      running anywhere").
    - **A real, pre-existing scheduler bug found and fixed along the way**:
      `sched::exit()` held the thread it was switching *into* (`next`) in a
      local variable across `thos_ctx_switch`, which never returns for that
      call (the stack is dead the instant it jumps away) — so `next`'s Drop
      glue never ran, permanently leaking one `Arc<Thread>` reference onto
      whichever thread `next` happened to be. Invisible before (nothing
      depended on an exact refcount), but it silently defeated `reap()`'s
      own `Arc::strong_count == 1` liveness check for roughly half of every
      batch of exited threads — always dropping to the exact same frame
      count with 8 test processes and never budging across 40 retries. Found
      by bisecting with a debug trace of `reap()`'s graveyard, `finish_switch`,
      and `SCHED`'s per-CPU state until the missing drop was visible
      directly. Fixed with one explicit `drop(next)` before the point of no
      return.
    - Also fixed along the way: an earlier attempt drove `reap()` from every
      idle CPU's hot loop for automatic background reclaim — reverted after
      it produced a real, timing-dependent hang (registry load one run,
      mid-fork/exec the next) under concurrent idle-loop lock traffic on
      `SCHED`. `reap()` stays explicitly-driven for now (a real reaper
      thread, paced sanely rather than spinning every idle iteration, is
      future work); the discipline that caught this — real boot tests, not
      "it compiles" — held again.
    - Verified with a genuine before/after frame-count check
      (`process_teardown_check`, kernel-internal): spawn and exit a batch of
      real user processes, force `reap()` (interleaved with `yield_now` so
      an exiting thread's own CPU gets to run `finish_switch` first — same
      pattern the stress milestone's reap-loop already used), and assert
      `FRAME_ALLOC`'s free count is back exactly where it started. Passes on
      real boot; full regression sweep green (ext2-test, pe-test, smp-test —
      the concurrency-heaviest one, unaffected — login-test, kbd-test).
  - **Real W^X / per-page memory protection** — the other half of "Security
    Core: … W^X + memory protection" from the security-architecture section
    below, and the other piece `nt.rs` used to flag as a stub
    (`VirtualProtect`/`VirtualFree`: "no teardown / per-page protection yet"
    — `VirtualAlloc` granted RW/NX regardless of the `flProtect` a caller
    asked for). `Process::protect` (`vmm::protect_page_in`, `update_flags` +
    an immediate `invlpg` — the caller may keep running on this CR3 right
    after and must not still see the old permissions) now really flips the
    `WRITABLE`/`NO_EXECUTE` PTE bits, wired into both `VirtualProtect`
    (Win32) and `NtProtectVirtualMemory` (native), including the
    previous-protection readback (`lpflOldProtect`/`OldProtect`) both report.
    Real `VirtualProtect` semantics: the whole region must already be
    committed or the call fails entirely, unchanged (`vmm::page_present_in`
    pre-validates every page before touching any of them).
    Known simplification, stated rather than silently wrong:
    `PAGE_NOACCESS` isn't supported (THOS has no true mapped-but-inaccessible
    page state yet — rejected rather than granting access anyway); the
    `_WRITECOPY` variants collapse to plain read-write (no real
    copy-on-write yet, same gap `process.rs`'s own module doc already names).
    Verified with genuine hardware-level enforcement in `pe-test`, not just
    bookkeeping: `VirtualAlloc` a page (RW/NX by default), hand-write a tiny
    function into it, `VirtualProtect` it to `PAGE_EXECUTE_READ` (checking
    the reported old protection is `PAGE_READWRITE`), then actually *call*
    into it — only reachable if the NX bit genuinely got cleared, not merely
    recorded — then flip to `PAGE_READONLY` and check that old-protection
    readback too. Passes on real boot first try, prints `PE protect OK`;
    full regression sweep green (ext2-test, pe-test, smp-test, login-test).
  - **Then (the phase):** the rest of process isolation / integrity for the
    security phase — the NT personality's remaining phase-3 items (below)
    are mostly done; a compositor / real window rendering is a plausible
    detour but not required to get there.
- **NT personality**: SSDT dispatch; `Nt*` core (`NtCreateFile` / `NtReadFile` /
  `Nt*VirtualMemory` / `NtWaitForSingleObject` …) onto executive primitives;
  **`\Device\` namespace** + drive letters as a VFS view; a minimal **registry** as a
  transactional key-value store; **SEH** ↔ trap dispatch; **APC** delivery.
- Write the `ntdll` lower boundary; layer **Wine PE-built DLLs** (`kernel32` /
  `kernelbase` / `user32` core) on top.
- **Milestone 3:** a statically linked Win32 **console** `.exe` (`CreateFile`,
  `WriteFile(stdout)`, `WaitForSingleObject`) runs through the THOS NT path — **with no
  wine process in the tree**. ELF and PE processes appear in one `ps` output.
  - Status: **met.** `xtask/testdata/wincon.c` — a real `x86_64-w64-mingw32-gcc`
    console `.exe` (`-nostdlib` + own entry, only `KERNEL32.dll` imports; a
    genuine toolchain PE with a real import table + `.pdata`) — opens
    `C:\pe-read.txt` (`CreateFileA`/`ReadFile`), writes stdout
    (`GetStdHandle`/`WriteFile`), `WaitForSingleObject`s a `CreateEventA` event,
    then `ExitProcess`. Runs on the synthetic `kernel32`, no Wine.
    `process::ps_dump` prints every task with an ELF/PE kind. `cargo xtask
    pe-test`. **Full mingw CRT path also done:** a synthetic `msvcrt.dll`
    (third `nt::dispatch` personality) — own `printf` formatter,
    `__getmainargs` + a ring-3 `_initterm` stub (so `argc`/`argv` work),
    `malloc`/`memcpy`/... — runs `/crt.exe`, a normal 244 KB `int main` mingw
    build, to exit. `ps`: `2 ELF + 4 PE`.

### Product goal — "download it and it runs", zero user config

The compatibility layers are a **first-class part of the OS**, not something the
user installs or configures. The loader inspects the binary and picks the
runtime itself — no "select Proton", no config file, no per-title tweaking.
The model is **Rosetta on macOS**: the translation/compat path is invisible.

| Input | Loader routes to | User does |
|---|---|---|
| ELF (Linux) | native Linux personality | nothing |
| PE `.exe` / `.dll` | native NT personality + Wine-sourced DLLs in `C:\Windows\System32` | nothing |
| Steam / Direct3D title | Proton profile (Wine + DXVK / vkd3d) auto-applied | nothing |
| `.apk` | Android profile (Waydroid-class: `binder` + Bionic + ART on the Linux personality) | nothing |

**"Download → runs, zero setup" is the target for:** native Linux binaries;
Win32 apps; **games without kernel anti-cheat** (the bulk of Steam);
EAC / BattlEye titles where the developer enabled the vendor's Proton mode.

**Honest boundary — kernel anti-cheat / device attestation.** THOS can deliver
the *zero-setup* half for these (bundle the vendor runtime, auto-configure the
environment). Whether the game then *runs* is decided by a third party,
server-side: EAC / BattlEye / ACE (NTE) / Vanguard / Ricochet / EA Javelin
attest the environment and can reject or ban. Best realistic case for the
"Running" tier (e.g. NTE via ACE): works *while the vendor's checks tolerate
the environment*, and can break with any anti-cheat patch — never a guaranteed
"just works". **No circumvention of attestation** — the vendor-sanctioned
Proton mode or nothing (see the anti-cheat notes in [`feasibility.md`](feasibility.md)).
Vanguard-class (kernel driver required at boot, no Proton mode) stays fully
out of scope pending vendor cooperation.

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
    password is rejected. **File-owner enforcement and a first `elevate`
    are done too now** — see "Capability policy" under Security Core below
    (DAC owner/group/other + `O_CREAT`/`chmod`/`chown` + real gid; and
    `elevate(path, argv, password)`, admin-only + re-authenticated, spawns
    a brand-new uid-0 process — no trusted path / secure-attention key
    yet). Still stub: PBKDF2 not argon2id, no `Principal` object proper
    (uid/gid stand in for it), no NT SID / token / DACL side, password
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
  - **Measured boot — done, in `loaders/thos-boot`** (the boot picker, not the
    kernel — measurement only means anything if it happens *before* the next
    stage runs, so it belongs to whatever hands off to that stage). Right
    before `StartImage` on the chosen loader, `measure()` hashes its
    already-`LoadImage`d bytes into the TPM via `EFI_TCG2_PROTOCOL` and logs
    the event, into PCR 4 ("Boot Manager Code and Boot Attempts" per the TCG
    PC Client spec — the right PCR for a boot manager extending whatever it's
    about to hand control to). No TPM/TCG2 present (most dev/test machines,
    plenty of real ones) → silently skipped, same boot either way —
    measuring is additive, never a requirement. **Secure boot** (signing) is
    a separate, mostly non-code concern noted where the boot picker's own
    risks are listed (chainloading Microsoft's loader is fine; loading our
    own *unsigned* kernel needs Secure Boot off or our keys enrolled on the
    target board) — not attempted here.
    Verified against a **real TPM 2.0**, not just "compiles": a new
    `cargo xtask bootpick-tpm-test` attaches a real `swtpm` (TCG2, via QEMU's
    `tpm-crb` device) to the same 3-disk OVMF picker test `bootpick-test`
    already uses, and asserts the serial log shows the actual measurement
    event (`measured \`THOS\` into TPM PCR 4`) — not a mocked success path.
    Skips (not fails) if `swtpm` isn't on `PATH`, since it's optional test
    tooling. `bootpick-test` itself (no TPM attached) still passes unchanged,
    confirming the TPM path is genuinely optional. Both pass on real boot.
  - **File-integrity baselines — done, `kernel/src/integrity.rs`.** SHA-256
    (already vendored for `cred.rs`'s PBKDF2 — no new crypto dependency)
    hashes of `integrity::BASELINE_FILES` (`/init`, `/rusthello` today — the
    files every boot configuration is guaranteed to have; grows as more of
    the system becomes something every boot can rely on being there),
    recorded once to `/etc/thos/integrity.baseline` (the registry hives' /
    credential store's own "own text format, hex-encoded fields"
    convention) on the first boot that finds none stored, and recomputed +
    compared against on every boot after. **Detection, not prevention** —
    nothing here stops a write to a baselined file, it only notices
    afterward that one happened; the building block a later on-access
    scanner or boot-attestation flow reads, not a scanner itself.
    Verified with a genuine three-boot round trip (`cargo xtask
    integrity-test`), including real tamper detection, not a mocked one:
    boot 1 (fresh disk) records the baseline; boot 2 (same disk, untouched)
    verifies clean; `/init` is then overwritten *directly on the disk image
    from the host* (`debugfs`, simulating an external tamper THOS itself
    never did); boot 3 must report the mismatch — and does
    (`THOS: integrity FAIL   /init does not match its baseline hash`),
    genuinely before the kernel goes on to legitimately panic trying to
    load the now-corrupt `/init` as an ELF (a real consequence of the
    tamper, not a test bug — further proof the check ran, not skipped).
    Full regression sweep otherwise unaffected: ext2-test, pe-test,
    smp-test, login-test all still pass.
  - **Capability policy — first slice done: real DAC file permissions.**
    Before this, `ext2::Inode` didn't even parse `i_uid`/`i_gid` and no
    syscall checked `mode` against the calling task's identity at all —
    `open()` granted access regardless of owner or permission bits. Now
    `Inode::access_ok(uid, want_write)` (owner/other tiers — no group tier
    yet, THOS's identity model has no real group membership beyond the
    `gid` a file carries; that gap is exactly what the wider capability-
    policy work above this slice is meant to fill in, and uid `0` — the
    system account, never an interactive session, see `cred.rs` — always
    passes, matching real Unix root semantics) is the DAC check both
    `open`/`openat` (finally reading the `O_ACCMODE` bits out of `flags`,
    silently dropped before) and the NT personality's `CreateFileA`
    (decided from `DesiredAccess` instead) now go through before handing
    back a handle — `EACCES`/`ERROR_ACCESS_DENIED` otherwise.
    Verified two ways: a real interactive-shell round trip (`cargo xtask
    kbd-test`, extended) — logged in as the uid-1000 admin session,
    `echo x > /etc/thos/admin.cred` against that mode-644-root-owned file
    (every file `write_path` creates is `0`-owned today) genuinely fails at
    the kernel's DAC check, and BusyBox's own shell reports it:
    `sh: can't create /etc/thos/admin.cred: Permission denied` — not a
    mocked denial. Full regression sweep otherwise green (ext2-test,
    pe-test, smp-test, login-test, integrity-test) — every existing test
    runs pre-login as uid `0`, so the bypass path keeps them all passing
    unchanged.
  - **Capability policy — second slice done: real file creation (`O_CREAT`)
    + `chmod`/`chown`.** The first slice's own gap — `open()` could deny an
    existing file, but there was no way to *create* one through a syscall
    at all, and a file's owner/mode were permanent from the moment
    `write_path` first wrote it. `open_resolved`'s `O_CREAT` now creates an
    empty file owned by the calling task, gated on that task having write
    permission on the *parent directory* (creating an entry is a write to
    the directory, not the not-yet-existing file) — the same check
    `SYS_MKDIR` now applies (a gap found live: a first pass had `mkdir`
    skip it entirely, and a test `mkdir /newdir` at the real, root-owned
    `/` silently succeeded before the fix). `chmod` requires owner-or-root;
    `chown` is root-only — stricter, matching modern Unix, where not even
    the owner can give a file away. A freshly provisioned uid otherwise had
    no writable location anywhere on disk (every directory ever created was
    system-owned mode 755) — `cred::save` now also creates `/home/<name>`
    for the new account, closing that off, same as `useradd` making a real
    Linux account a home directory.
    Verified two ways: a real interactive-shell round trip (`cargo xtask
    kbd-test`, extended) — logged in as `thos`, `cd /home/thos && touch
    newfile && mkdir newdir && ls` shows both, genuine new inodes through
    the syscall ABI (this is also what caught BusyBox `touch` needing a
    real `SYS_UTIMENSAT` — it tries that first and only falls back to its
    own `open(O_CREAT)` on `ENOENT`, so an unhandled syscall there silently
    broke `touch` even after `O_CREAT` itself worked). `chmod`/`chown` have
    no BusyBox applet to drive live, so they're proven at the ext2-layer +
    permission-policy-logic level instead (`posix_owner_check`,
    kernel-internal, same shape as `execgate_check`) — real, just not yet
    exercised through the live syscall ABI by a dedicated test, same honest
    scoping as `execve`'s exec-gate wiring below. Full regression sweep
    green: ext2-test, pe-test, smp-test, login-test, integrity-test,
    kbd-test.
  - **Capability policy — third slice done: real group-tier DAC.** The
    first slice's own stated gap — `gid` parsed and stamped on every file
    but never actually checked, so a non-owner always landed in "other"
    regardless of group. `Task` now carries a `gid` (== its `uid` — no
    supplementary groups yet, the "user private group" scheme `cred::save`
    already assumed naming a new account's home directory) and
    `Inode::access_ok(uid, gid, want_write)` is a real three-tier check:
    owner, then group (caller's gid matches the file's gid), then other.
    `SYS_GETGID`/`SYS_GETEGID` return the real gid instead of quietly
    aliasing `SYS_GETUID`.
    Verified kernel-internal (`group_tier_check`, main.rs — THOS is still a
    single-admin-account system, so there's no second uid/gid to drive this
    live through a real shell, same scoping as chmod/chown's own
    verification): a real file owned `(1000, 1000)`, chmod'd to `0640` via
    the real `chmod_path`; the owner gets read+write, a *different* uid
    sharing the file's gid gets read only, a uid matching neither gets
    nothing, uid 0 always passes. Full regression sweep green: ext2-test,
    pe-test, smp-test, login-test, integrity-test, kbd-test.
  - **`elevate()` — first slice done.** The first real step off "no root
    login — admin elevates" (Identity, privilege & login, above): a new
    THOS-native syscall range (`THOS_BASE`, same shape as `nt::NT_BASE`)
    carries `elevate(path, argv, password)`. Two real checks — admin-only
    (`task.uid != cred::ADMIN_UID` is `EPERM` before the password is even
    looked at; THOS has exactly one principal that can ever be admin) and
    re-authentication (the freshly typed password checked against the real
    credential store, not "you're already logged in") — then
    `process::spawn_elevated` starts a **brand-new process** with uid/gid
    forced to 0. THOS has no in-place token upgrade, so "scoped to that
    process" falls out of the design for free: there is no elevated shell
    or standing token to leak or reuse for a later action. Not yet built,
    stated rather than skipped: the **trusted path** (a secure-attention
    key so no app can draw a fake password prompt) — needs a global
    keyboard-capture mechanism; today the password is just whatever the
    calling process handed the kernel, trusted only because THOS has
    exactly one interactive session and nothing else that could impersonate
    the prompt yet.
    Verified with a genuine round trip, not "the syscall returned 0": a new
    test binary `/do-elevate` calls the real syscall via a raw `syscall`
    instruction (no libc) with the actual admin password `drive_login`
    sets up; on success it has spawned `/elevated-check`, which itself
    calls `getuid()` and prints the result — proof the *spawned* process
    really has uid 0. `cargo xtask kbd-test`: `elevate returned 22` (a real
    pid) followed by `elevated-check uid=0`. Full regression sweep green.
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

**First slice done — `kernel/src/execgate.rs`, the kernel-side skeleton.**
M3 landed, so the gate's sequencing condition is met; the real YARA /
heuristics / quarantine-store work stays the isolated-userspace Security
Service's job — "the scanner never runs in the kernel" is the whole point of
that split — this slice is only `format detect → parse headers` (already
`pe::load`/`elf::load`'s job, right after) `→ hash/signature check → policy
engine`. `execgate::check(bytes)`: the EICAR Standard Anti-Virus Test File
string anywhere in the buffer, or a SHA-256 match against a (today: empty)
known-bad list, both `Verdict::Quarantine`; anything else `Allow`. Wired into
both native-process entry points: `spawn_pe` (`Result`-based, same shape as
an already-existing malformed-PE rejection — `Err`, kernel alive) and
`execve` (no `Result` to hand back through that ABI — the calling thread's
own image is what's being replaced — so quarantine there ends the calling
thread cleanly instead, exit code 126, the shell convention for "found but
not executable").
Verified two ways: `execgate::check`'s detection logic directly
(kernel-internal — ordinary bytes pass, an EICAR string buried in an
otherwise arbitrary buffer doesn't); and a real PE round trip in `pe-test` —
`/pe-hello.exe`'s own already-proven-runnable bytes (`THOS: pe exited` earlier
in the same boot) with the EICAR string appended are quarantined before
`pe::load` ever parses a header, so a rejection here can only be the gate, not
a malformed-file fluke. `execve`'s wiring is the same shape but not yet
exercised by a dedicated live test — no test binary `execve`s malicious
content today.
**A real, pre-existing kernel bug found and fixed along the way** (`seh.rs`):
`thos_fault_dispatch` unconditionally read the PE-only vectored-exception-
handler slot (`PE_EXC_ADDR`) for *every* user-mode fault, PE or plain ELF —
that page is only ever mapped for a PE process, so any ELF program's fault
(e.g. an ordinary segfault) took the fault dispatcher itself down with a
second, unhandled `#PF` instead of just killing the faulting process, right
as `kbd-test` was extended to add a command past the exec-gate work. Fixed
with a new `process::current_is_pe()` gating the read. Full regression sweep
green afterward: ext2-test, pe-test, smp-test, login-test, integrity-test,
kbd-test.

### In-system AI

THOS grows its own AI, built from scratch (*user, 2026-09-04*): a small
**decoder-only language model** trained off-device (PyTorch, CPU) and run by a
hand-written `#![no_std]` Rust engine (`ml/thos-lm`) over a first-party `.tlm`
weight format — no external model runtime, no API. Training data is **only** open
/ permissively-licensed / public-domain. Start ~1M params byte-level, grow step by
step. Full plan + phases (P0–P6) and open decisions:
[`ai.md`](ai.md); code + datasets: [`../../ml/`](../../ml/) /
[`../../ml/DATASETS.md`](../../ml/DATASETS.md).

The former standalone "in-kernel exec-gate classifier" is **folded in** as
Application B of this stack — a classifier head reusing the LM's tensor + loader
code to score PE/ELF statically for the native-exec gate above. Running any of
this *inside* THOS is gated on kernel work not yet present (userland file writes,
a kernel↔userspace channel, RAM budget) and is a later phase, not near-term.

Two research tracks hang off this, both unscheduled: **a ~20–30 B open model
quantised to 2-bit and held resident** in the 16 GB target
([`ai-large.md`](ai-large.md) — streaming a bigger model from the SATA disk is
not viable), and **a from-scratch rework of the context mechanism** (learned
active memory so a small resident footprint behaves like a huge context,
[`ai-context.md`](ai-context.md)). The consolidated build order is in
[`ai.md`](ai.md).

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
6. **In-system AI** — model class beyond the v0 byte GPT, tokenizer for P1,
   `f32` vs fixed-point for the in-kernel exec-gate head, weight delivery
   (`include_bytes!` vs ext2 file), the kernel↔userspace query channel, and the
   `ml/` licence. Enumerated in [`ai.md`](ai.md) → Open decisions
   (*user, 2026-09-04*).
