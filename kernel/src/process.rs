// SPDX-License-Identifier: GPL-2.0-or-later
//! Phase 2 — the process / address-space object.
//!
//! A `Process` owns a private top-level page table: a full copy of the kernel
//! PML4 (so the kernel half + HHDM + identity map are shared, by pointer, with
//! every process) plus its own user-half entries. A user thread carries the
//! physical base of its process's PML4; the scheduler loads it into CR3 on the
//! switch.
//!
//! No `fork` sharing / COW yet, no address-space teardown (a reaper frees the
//! frames later) — this is here to give ELF programs real isolation.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use spin::Mutex;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::{PageTable, PageTableFlags, PhysFrame};
use x86_64::PhysAddr;

use crate::elf::{self, Image};
use crate::file::{ConsoleFile, FileOps, KeyboardFile};
use crate::mm::{hhdm_offset, phys_to_virt, FRAME_ALLOC};
use crate::wait::{Event, Mutant, Semaphore};
use crate::syscall::{self, UserFrame};
use crate::{gdt, sched, vmm};

/// One address space. Shared kernel half (by pointer) + private user half.
pub struct Process {
    pml4_phys: u64,
    /// Next free user virtual address for `mmap` / stacks.
    next_user_va: AtomicU64,
    /// Program break for `brk`.
    brk: AtomicU64,
}

/// User virtual space for `mmap` / stacks, clear of typical ELF load addresses.
const USER_ALLOC_BASE: u64 = 0x0000_7000_0000_0000;
const USER_STACK_SIZE: u64 = 64 * 1024;
const BRK_BASE: u64 = 0x0000_6800_0000_0000;
const BRK_MAX: u64 = BRK_BASE + 256 * 1024 * 1024;

impl Process {
    pub fn new() -> Arc<Self> {
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame for process PML4");
        let pml4_phys = frame.start_address().as_u64();

        // Copy every entry of the kernel PML4 so the kernel half + HHDM are
        // shared with this process, then drop PML4[0] — the kernel's low-4 GiB
        // identity map. Kernel-CR3 threads keep it; a process must have its
        // entire low half free so an ELF (e.g. static musl at 0x400000) can map
        // there without colliding with a 1 GiB identity huge page.
        unsafe {
            let dst = phys_to_virt(PhysAddr::new(pml4_phys)).as_mut_ptr::<u8>();
            core::ptr::copy_nonoverlapping(
                phys_to_virt(PhysAddr::new(vmm::kernel_pml4_phys())).as_ptr::<u8>(),
                dst,
                4096,
            );
            *(dst as *mut u64) = 0; // PML4[0]
        }

        Arc::new(Self {
            pml4_phys,
            next_user_va: AtomicU64::new(USER_ALLOC_BASE),
            brk: AtomicU64::new(BRK_BASE),
        })
    }

    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    fn copy_alloc_state_from(&self, other: &Process) {
        self.next_user_va
            .store(other.next_user_va.load(Ordering::Relaxed), Ordering::Relaxed);
        self.brk.store(other.brk.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    /// Visit every present 4 KiB user page: `(virt, phys, writable, exec)`.
    fn for_each_user_page(&self, mut f: impl FnMut(u64, u64, bool, bool)) {
        let hhdm = hhdm_offset();
        let tbl = |phys: u64| unsafe { &*((phys + hhdm) as *const PageTable) };
        // 0..256 = the whole user half. Index 0 matters: a static-musl ELF
        // loads at 0x400000, so its text/data live under PML4[0]. (The kernel's
        // low identity map was already dropped from PML4[0] in `Process::new`.)
        for i4 in 0..256u64 {
            let e4 = &tbl(self.pml4_phys)[i4 as usize];
            if !e4.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            for i3 in 0..512u64 {
                let e3 = &tbl(e4.addr().as_u64())[i3 as usize];
                if !e3.flags().contains(PageTableFlags::PRESENT)
                    || e3.flags().contains(PageTableFlags::HUGE_PAGE)
                {
                    continue;
                }
                for i2 in 0..512u64 {
                    let e2 = &tbl(e3.addr().as_u64())[i2 as usize];
                    if !e2.flags().contains(PageTableFlags::PRESENT)
                        || e2.flags().contains(PageTableFlags::HUGE_PAGE)
                    {
                        continue;
                    }
                    for i1 in 0..512u64 {
                        let e1 = &tbl(e2.addr().as_u64())[i1 as usize];
                        let fl = e1.flags();
                        if !fl.contains(PageTableFlags::PRESENT)
                            || !fl.contains(PageTableFlags::USER_ACCESSIBLE)
                        {
                            continue;
                        }
                        let virt = (i4 << 39) | (i3 << 30) | (i2 << 21) | (i1 << 12);
                        f(
                            virt,
                            e1.addr().as_u64(),
                            fl.contains(PageTableFlags::WRITABLE),
                            !fl.contains(PageTableFlags::NO_EXECUTE),
                        );
                    }
                }
            }
        }
    }

    /// Map one 4 KiB user page into this address space.
    pub fn map(&self, virt: u64, phys: u64, writable: bool, exec: bool) {
        vmm::map_page_in(self.pml4_phys, virt, phys, writable, true, exec);
    }

    fn map_zeroed(&self, virt: u64) {
        let frame = FRAME_ALLOC.lock().alloc().expect("no frame");
        unsafe {
            core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
        }
        self.map(virt, frame.start_address().as_u64(), true, false);
    }

    /// `brk(0)` returns the current break; `brk(addr)` grows/sets it.
    pub fn brk(&self, req: u64) -> u64 {
        let cur = self.brk.load(Ordering::Relaxed);
        if req < BRK_BASE || req > BRK_MAX {
            return cur;
        }
        let mut v = (cur + 0xFFF) & !0xFFF;
        let end = (req + 0xFFF) & !0xFFF;
        while v < end {
            self.map_zeroed(v);
            v += 4096;
        }
        self.brk.store(req, Ordering::Relaxed);
        req
    }

    /// Anonymous `mmap`: bump-allocate + map `len` bytes RW, return the base.
    pub fn mmap_anon(&self, len: u64) -> u64 {
        let len = (len + 0xFFF) & !0xFFF;
        let base = self.next_user_va.fetch_add(len + 0x1000, Ordering::Relaxed);
        let mut v = base;
        while v < base + len {
            self.map_zeroed(v);
            v += 4096;
        }
        base
    }

    /// Allocate + map a fresh user stack; returns the (page-aligned) stack top.
    pub fn new_user_stack(&self) -> u64 {
        let base = self.next_user_va.fetch_add(USER_STACK_SIZE + 0x1000, Ordering::Relaxed);
        let pages = USER_STACK_SIZE / 4096;
        for i in 0..pages {
            let frame = FRAME_ALLOC.lock().alloc().expect("no frame for user stack");
            unsafe {
                core::ptr::write_bytes(phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(), 0, 4096);
            }
            self.map(base + i * 4096, frame.start_address().as_u64(), true, false);
        }
        base + USER_STACK_SIZE
    }

    /// Walk this address space's page tables (via HHDM) to a physical address.
    pub fn translate(&self, virt: u64) -> Option<u64> {
        let hhdm = hhdm_offset();
        let idx = [
            (virt >> 39) & 0x1FF,
            (virt >> 30) & 0x1FF,
            (virt >> 21) & 0x1FF,
            (virt >> 12) & 0x1FF,
        ];
        let mut table_phys = self.pml4_phys;
        for (level, &i) in idx.iter().enumerate() {
            let table = unsafe { &*((table_phys + hhdm) as *const PageTable) };
            let e = &table[i as usize];
            if !e.flags().contains(PageTableFlags::PRESENT) {
                return None;
            }
            if level < 3 && e.flags().contains(PageTableFlags::HUGE_PAGE) {
                return None;
            }
            table_phys = e.addr().as_u64();
        }
        Some(table_phys + (virt & 0xFFF))
    }

    /// Copy bytes into this address space at `virt` (page by page, via HHDM).
    pub fn write_user(&self, mut virt: u64, mut data: &[u8]) {
        let hhdm = hhdm_offset();
        while !data.is_empty() {
            let phys = self.translate(virt).expect("write_user: unmapped page");
            let off = (virt & 0xFFF) as usize;
            let n = (4096 - off).min(data.len());
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), (phys + hhdm) as *mut u8, n);
            }
            virt += n as u64;
            data = &data[n..];
        }
    }

    /// Lay out the SysV AMD64 initial process stack (argc, argv, envp, auxv,
    /// AT_RANDOM) at the top of a fresh user stack. Returns the entry `rsp`.
    pub fn init_stack(&self, stack_top: u64, argv: &[&str], envp: &[&str], img: &Image) -> u64 {
        let mut cur = stack_top;
        let push = |cur: &mut u64, bytes: &[u8]| -> u64 {
            *cur -= bytes.len() as u64;
            self.write_user(*cur, bytes);
            *cur
        };

        let rand_addr = push(&mut cur, &[0x5Au8; 16]); // AT_RANDOM material
        let cstr = |cur: &mut u64, s: &str| -> u64 {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            push(cur, &b)
        };
        let arg_ptrs: Vec<u64> = argv.iter().map(|s| cstr(&mut cur, s)).collect();
        let env_ptrs: Vec<u64> = envp.iter().map(|s| cstr(&mut cur, s)).collect();
        let execfn = arg_ptrs.first().copied().unwrap_or(0);

        // auxv (type, value) pairs — AT_NULL last.
        let aux: [(u64, u64); 8] = [
            (3, img.phdr),   // AT_PHDR
            (4, img.phent),  // AT_PHENT
            (5, img.phnum),  // AT_PHNUM
            (6, 4096),       // AT_PAGESZ
            (9, img.entry),  // AT_ENTRY
            (25, rand_addr), // AT_RANDOM
            (31, execfn),    // AT_EXECFN
            (0, 0),          // AT_NULL
        ];

        let words = 1                       // argc
            + (arg_ptrs.len() + 1)          // argv + NULL
            + (env_ptrs.len() + 1)          // envp + NULL
            + aux.len() * 2; // auxv pairs
        let block_bytes = (words * 8) as u64;

        // Align so that the final rsp is 16-byte aligned.
        cur -= (cur - block_bytes) % 16;
        let rsp = cur - block_bytes;

        let mut block: Vec<u8> = Vec::with_capacity(words * 8);
        let mut w = |v: u64| block.extend_from_slice(&v.to_le_bytes());
        w(argv.len() as u64);
        for p in &arg_ptrs {
            w(*p);
        }
        w(0);
        for p in &env_ptrs {
            w(*p);
        }
        w(0);
        for (t, v) in aux {
            w(t);
            w(v);
        }
        self.write_user(rsp, &block);
        rsp
    }
}

// ===========================================================================
//  Tasks — the process-tree layer (pid, parent, exit status). Threads point
//  here; `Task` owns the (execve-swappable) address space.
// ===========================================================================

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
static TASKS: Mutex<BTreeMap<u64, Arc<Task>>> = Mutex::new(BTreeMap::new());
/// Woken whenever any task records its exit status — `wait4` sleeps on it
/// instead of polling.
static CHILD_EXIT: crate::wait::WaitQueue = crate::wait::WaitQueue::new();

/// The logged-in session identity, stamped onto every task created after login.
/// `(uid, name)`. Grows into the executive `Principal`.
static SESSION_UID: AtomicU64 = AtomicU64::new(0);
static SESSION_NAME: Mutex<String> = Mutex::new(String::new());

/// Record the identity resolved by `login` (see `login::establish`).
#[allow(dead_code)] // only the `interactive` build has a login flow
pub fn set_session(name: &str, uid: u32) {
    SESSION_UID.store(uid as u64, Ordering::Relaxed);
    *SESSION_NAME.lock() = name.into();
}

/// This task's user id (from the login session; 0 for tasks spawned pre-login).
pub fn current_uid() -> u32 {
    sched::current().task().map(|t| t.uid).unwrap_or(0)
}

/// What a HANDLE / file descriptor points at. Both personalities share one
/// per-process table: a POSIX fd and a Win32 `HANDLE` are the same integer
/// into the same `Vec` — a file, or an executive object.
#[derive(Clone)]
pub enum HandleObject {
    File(Arc<dyn FileOps>),
    Event(Arc<Event>),
    Semaphore(Arc<Semaphore>),
    Mutant(Arc<Mutant>),
    /// A registry key — the canonical `\`-joined path into [`crate::registry`]'s
    /// global tree (ops re-walk under that module's lock).
    RegKey(String),
}

/// A polymorphic view of the dispatcher objects a `NtWaitFor*` call can wait on.
/// `tid` is threaded through only for the mutant's ownership check.
#[derive(Clone)]
pub enum Waitable {
    Event(Arc<Event>),
    Semaphore(Arc<Semaphore>),
    Mutant(Arc<Mutant>),
}

impl Waitable {
    /// Non-blocking: take/consume the signal if present.
    pub fn try_take(&self, tid: u64) -> bool {
        match self {
            Waitable::Event(e) => e.try_take(),
            Waitable::Semaphore(s) => s.try_take(),
            Waitable::Mutant(m) => m.try_acquire(tid),
        }
    }
    /// Block until signalled, then consume it.
    pub fn wait(&self, tid: u64) {
        match self {
            Waitable::Event(e) => e.wait(),
            Waitable::Semaphore(s) => s.wait(),
            Waitable::Mutant(m) => m.acquire(tid),
        }
    }
    /// Would `try_take` succeed right now? (No consume.)
    pub fn is_signaled(&self, tid: u64) -> bool {
        match self {
            Waitable::Event(e) => e.is_signaled(),
            Waitable::Semaphore(s) => s.is_signaled(),
            Waitable::Mutant(m) => m.is_signaled(tid),
        }
    }
}

/// One table slot: the object plus its close-on-exec flag (per-descriptor, not
/// per open-file-description).
#[derive(Clone)]
pub struct FdEntry {
    pub obj: HandleObject,
    pub cloexec: bool,
}
type Fd = Option<FdEntry>;

/// One queued user-mode APC (see [`crate::apc`]). `routine` is the
/// `PKNORMAL_ROUTINE`; `arg1..arg3` are `NtQueueApcThread`'s `ApcArgument1..3`
/// (NormalContext / SystemArgument1 / SystemArgument2).
#[derive(Clone, Copy)]
pub struct ApcEntry {
    pub routine: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
}

pub struct Task {
    pub pid: u64,
    pub ppid: u64,
    pub uid: u32,
    space: Mutex<Arc<Process>>,
    exit_status: Mutex<Option<i32>>,
    exited: AtomicBool,
    /// File descriptor table. 0/1/2 seeded with the console.
    fds: Mutex<Vec<Fd>>,
    /// Current working directory, always a normalised absolute path.
    cwd: Mutex<String>,
    /// Pending user-mode APCs, delivered when the thread next goes alertable.
    apcs: Mutex<VecDeque<ApcEntry>>,
}

fn seed_fds() -> Vec<Fd> {
    let stdin: Arc<dyn FileOps> = Arc::new(KeyboardFile);
    let out: Arc<dyn FileOps> = Arc::new(ConsoleFile { writable: true });
    let e = |f: Arc<dyn FileOps>| Some(FdEntry { obj: HandleObject::File(f), cloexec: false });
    alloc::vec![e(stdin), e(out.clone()), e(out)]
}

impl Task {
    fn new(ppid: u64, space: Arc<Process>) -> Arc<Self> {
        let t = Arc::new(Self {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            ppid,
            uid: SESSION_UID.load(Ordering::Relaxed) as u32,
            space: Mutex::new(space),
            exit_status: Mutex::new(None),
            exited: AtomicBool::new(false),
            fds: Mutex::new(seed_fds()),
            cwd: Mutex::new(String::from("/")),
            apcs: Mutex::new(VecDeque::new()),
        });
        TASKS.lock().insert(t.pid, t.clone());
        t
    }

    pub fn space(&self) -> Arc<Process> {
        self.space.lock().clone()
    }

    fn set_space(&self, s: Arc<Process>) {
        *self.space.lock() = s;
    }

    pub fn fd_get(&self, fd: i32) -> Option<Arc<dyn FileOps>> {
        match &self.fds.lock().get(fd as usize)?.as_ref()?.obj {
            HandleObject::File(f) => Some(f.clone()),
            _ => None,
        }
    }

    /// The `Event` a HANDLE names, if it is one.
    pub fn handle_event(&self, h: i32) -> Option<Arc<Event>> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::Event(e) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn fd_alloc(&self, file: Arc<dyn FileOps>) -> i32 {
        self.fd_alloc_flags(file, false)
    }
    pub fn fd_alloc_flags(&self, file: Arc<dyn FileOps>, cloexec: bool) -> i32 {
        self.handle_alloc(HandleObject::File(file), cloexec)
    }
    pub fn handle_alloc_event(&self, ev: Arc<Event>) -> i32 {
        self.handle_alloc(HandleObject::Event(ev), false)
    }
    pub fn handle_alloc_semaphore(&self, s: Arc<Semaphore>) -> i32 {
        self.handle_alloc(HandleObject::Semaphore(s), false)
    }
    pub fn handle_alloc_mutant(&self, m: Arc<Mutant>) -> i32 {
        self.handle_alloc(HandleObject::Mutant(m), false)
    }

    /// The dispatcher object a HANDLE names, as a [`Waitable`], if it is one.
    pub fn handle_waitable(&self, h: i32) -> Option<Waitable> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::Event(e) => Some(Waitable::Event(e.clone())),
            HandleObject::Semaphore(s) => Some(Waitable::Semaphore(s.clone())),
            HandleObject::Mutant(m) => Some(Waitable::Mutant(m.clone())),
            _ => None,
        }
    }

    /// The registry-key path a HANDLE names, if it is one.
    pub fn handle_regkey(&self, h: i32) -> Option<String> {
        match &self.fds.lock().get(h as usize)?.as_ref()?.obj {
            HandleObject::RegKey(p) => Some(p.clone()),
            _ => None,
        }
    }
    pub fn handle_alloc_regkey(&self, path: String) -> i32 {
        self.handle_alloc(HandleObject::RegKey(path), false)
    }

    /// Append a user APC to this task's queue.
    pub fn apc_queue(&self, e: ApcEntry) {
        self.apcs.lock().push_back(e);
    }
    /// Dequeue the oldest pending user APC, if any.
    pub fn apc_take(&self) -> Option<ApcEntry> {
        self.apcs.lock().pop_front()
    }
    /// `true` if at least one user APC is queued.
    pub fn apc_pending(&self) -> bool {
        !self.apcs.lock().is_empty()
    }

    /// Install `obj` at the lowest free descriptor, with the given cloexec flag.
    pub fn handle_alloc(&self, obj: HandleObject, cloexec: bool) -> i32 {
        let mut fds = self.fds.lock();
        let entry = Some(FdEntry { obj, cloexec });
        match fds.iter().position(|f| f.is_none()) {
            Some(i) => {
                fds[i] = entry;
                i as i32
            }
            None => {
                fds.push(entry);
                (fds.len() - 1) as i32
            }
        }
    }

    pub fn fd_close(&self, fd: i32) -> bool {
        let mut fds = self.fds.lock();
        match fds.get_mut(fd as usize) {
            Some(slot @ Some(_)) => {
                *slot = None;
                true
            }
            _ => false,
        }
    }

    /// `F_GETFD` / `F_SETFD` on the close-on-exec flag. Returns `0`/`1`, or
    /// `-EBADF`.
    pub fn fd_get_cloexec(&self, fd: i32) -> i32 {
        match self.fds.lock().get(fd as usize).and_then(|f| f.as_ref()) {
            Some(e) => e.cloexec as i32,
            None => -9,
        }
    }
    pub fn fd_set_cloexec(&self, fd: i32, on: bool) -> i32 {
        match self.fds.lock().get_mut(fd as usize).and_then(|f| f.as_mut()) {
            Some(e) => {
                e.cloexec = on;
                0
            }
            None => -9,
        }
    }

    /// execve: drop every descriptor marked close-on-exec.
    fn close_on_exec(&self) {
        for slot in self.fds.lock().iter_mut() {
            if slot.as_ref().map_or(false, |e| e.cloexec) {
                *slot = None;
            }
        }
    }

    /// Duplicate `oldfd` to the lowest free descriptor at or above `min`. The
    /// new descriptor never inherits close-on-exec (`F_DUPFD` semantics).
    pub fn fd_dup(&self, oldfd: i32, min: i32) -> i32 {
        let mut fds = self.fds.lock();
        let Some(Some(mut entry)) = fds.get(oldfd as usize).cloned() else {
            return -9; // EBADF
        };
        entry.cloexec = false;
        let min = min.max(0) as usize;
        if let Some(i) = (min..fds.len()).find(|&i| fds[i].is_none()) {
            fds[i] = Some(entry);
            return i as i32;
        }
        while fds.len() < min {
            fds.push(None);
        }
        fds.push(Some(entry));
        (fds.len() - 1) as i32
    }

    /// `dup2` / `dup3`: force `newfd` to refer to `oldfd`'s file (closing
    /// whatever was there). `cloexec` sets the new descriptor's flag (always
    /// `false` for `dup2`). Returns `newfd`, or `-EBADF` if `oldfd` is invalid.
    pub fn fd_dup2(&self, oldfd: i32, newfd: i32) -> i32 {
        self.fd_dup3(oldfd, newfd, false)
    }
    pub fn fd_dup3(&self, oldfd: i32, newfd: i32, cloexec: bool) -> i32 {
        let mut fds = self.fds.lock();
        let Some(Some(mut entry)) = fds.get(oldfd as usize).cloned() else {
            return -9;
        };
        if oldfd == newfd {
            return newfd; // POSIX: dup2 no-op keeps the fd (dup3 would EINVAL)
        }
        entry.cloexec = cloexec;
        let n = newfd.max(0) as usize;
        while fds.len() <= n {
            fds.push(None);
        }
        fds[n] = Some(entry);
        newfd
    }

    /// fork inherits the parent's open files (shared, like POSIX).
    fn clone_fds(&self) -> Vec<Fd> {
        self.fds.lock().clone()
    }

    pub fn cwd(&self) -> String {
        self.cwd.lock().clone()
    }
    pub fn set_cwd(&self, path: String) {
        *self.cwd.lock() = path;
    }
}

/// The current task's working directory (`"/"` if there is no task).
pub fn current_cwd() -> String {
    sched::current().task().map(|t| t.cwd()).unwrap_or_else(|| String::from("/"))
}

/// Store an already-normalised absolute path as the current task's cwd.
pub fn set_current_cwd(path: String) {
    if let Some(t) = sched::current().task() {
        t.set_cwd(path);
    }
}

/// Resolve `path` against the current task's cwd into a clean absolute path:
/// `.` is dropped, `..` pops a component (never past `/`), and repeated or
/// trailing slashes collapse. No symlink following (we have no symlinks).
pub fn resolve_path(path: &str) -> String {
    let base = if path.starts_with('/') { String::new() } else { current_cwd() };
    let mut comps: Vec<&str> = Vec::new();
    for part in base.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            p => comps.push(p),
        }
    }
    let mut out = String::from("/");
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(c);
    }
    out
}

pub fn user_selectors() -> (u64, u64) {
    let s = gdt::selectors();
    ((s.user_code.0 | 3) as u64, (s.user_data.0 | 3) as u64)
}

/// The current task's file object for `fd`, if open.
pub fn current_fd(fd: i32) -> Option<Arc<dyn FileOps>> {
    sched::current().task().and_then(|t| t.fd_get(fd))
}

/// The current task's `Event` for HANDLE `h`, if it names one.
pub fn current_event(h: i32) -> Option<Arc<Event>> {
    sched::current().task().and_then(|t| t.handle_event(h))
}

/// Install `ev` in the current task's HANDLE table; returns the HANDLE, or -1.
pub fn current_alloc_event(ev: Arc<Event>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_event(ev))
}
pub fn current_alloc_semaphore(s: Arc<Semaphore>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_semaphore(s))
}
pub fn current_alloc_mutant(m: Arc<Mutant>) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_mutant(m))
}

/// The current task's [`Waitable`] for HANDLE `h`, if it names a dispatcher
/// object (event / semaphore / mutant).
pub fn current_waitable(h: i32) -> Option<Waitable> {
    sched::current().task().and_then(|t| t.handle_waitable(h))
}

/// The current task's registry-key path for HANDLE `h`, if it names one.
pub fn current_regkey(h: i32) -> Option<String> {
    sched::current().task().and_then(|t| t.handle_regkey(h))
}

/// Install a registry-key HANDLE (by canonical path) in the current task.
pub fn current_alloc_regkey(path: String) -> i32 {
    sched::current().task().map_or(-1, |t| t.handle_alloc_regkey(path))
}

/// Queue a user APC on the current task; `false` if there is no current task.
pub fn current_queue_apc(e: ApcEntry) -> bool {
    match sched::current().task() {
        Some(t) => {
            t.apc_queue(e);
            true
        }
        None => false,
    }
}

/// Dequeue one pending user APC for the current task.
pub fn current_take_apc() -> Option<ApcEntry> {
    sched::current().task().and_then(|t| t.apc_take())
}

/// `true` if the current task has a user APC queued. (Used by the alertable
/// wait path, which lands with the executive timer wheel.)
#[allow(dead_code)]
pub fn current_apc_pending() -> bool {
    sched::current().task().map(|t| t.apc_pending()).unwrap_or(false)
}

/// This thread's id (distinct per thread within a process — unlike
/// [`current_pid`], which is the shared `Task` id).
pub fn current_tid() -> u64 {
    sched::current().id
}

/// Per-worker-thread exit events, keyed by thread id. A thread's
/// `NtCreateThreadEx` registers one; `NtTerminateThread` signals + removes it,
/// so a `NtWaitForSingleObject` on the returned handle completes on exit.
static THREAD_EXITS: Mutex<BTreeMap<u64, Arc<Event>>> = Mutex::new(BTreeMap::new());

pub fn register_thread_exit(tid: u64, ev: Arc<Event>) {
    THREAD_EXITS.lock().insert(tid, ev);
}

/// Signal + forget `tid`'s exit event. `false` if none was registered (i.e. the
/// caller is the process's original thread).
pub fn signal_thread_exit(tid: u64) -> bool {
    match THREAD_EXITS.lock().remove(&tid) {
        Some(ev) => {
            ev.signal();
            true
        }
        None => false,
    }
}

pub fn current_pid() -> u64 {
    sched::current().task().map(|t| t.pid).unwrap_or(0)
}

pub fn current_ppid() -> u64 {
    sched::current().task().map(|t| t.ppid).unwrap_or(0)
}

/// `dup` / `dup2` / `dup3` / `fcntl` on the current task's fd table.
pub fn current_fd_dup(oldfd: i32, min: i32) -> i32 {
    sched::current().task().map(|t| t.fd_dup(oldfd, min)).unwrap_or(-9)
}
pub fn current_fd_dup2(oldfd: i32, newfd: i32) -> i32 {
    sched::current().task().map(|t| t.fd_dup2(oldfd, newfd)).unwrap_or(-9)
}
pub fn current_fd_dup3(oldfd: i32, newfd: i32, cloexec: bool) -> i32 {
    sched::current().task().map(|t| t.fd_dup3(oldfd, newfd, cloexec)).unwrap_or(-9)
}
pub fn current_fd_get_cloexec(fd: i32) -> i32 {
    sched::current().task().map(|t| t.fd_get_cloexec(fd)).unwrap_or(-9)
}
pub fn current_fd_set_cloexec(fd: i32, on: bool) -> i32 {
    sched::current().task().map(|t| t.fd_set_cloexec(fd, on)).unwrap_or(-9)
}

/// Record an exit status on the current task (called from `exit`/`exit_group`).
///
/// A zombie holds no open files: drop the fd table now so the other end of any
/// pipe sees EOF/`EPIPE` immediately, instead of only once `wait4` reaps the
/// `Task` out of the `TASKS` map.
pub fn set_exit_status(code: i32) {
    if let Some(t) = sched::current().task() {
        *t.exit_status.lock() = Some(code);
        t.fds.lock().clear();
        t.exited.store(true, Ordering::Release);
    }
    CHILD_EXIT.wake_all(); // an interested parent may be blocked in wait4
}

/// Predicate for `wait4` to sleep on: this task has a matching live child and
/// none of its matching children has exited yet.
fn should_block_in_wait4(me: u64, pid: i64) -> bool {
    let tasks = TASKS.lock();
    let mut has_child = false;
    for t in tasks.values() {
        if t.ppid == me && (pid == -1 || t.pid == pid as u64) {
            has_child = true;
            if t.exited.load(Ordering::Acquire) {
                return false;
            }
        }
    }
    has_child
}

/// `spawn` the initial user program: build its address space + entry stack and
/// hand it to the scheduler. Returns the pid.
pub fn spawn_init(bytes: &[u8], argv: &[&str], envp: &[&str]) -> u64 {
    let space = Process::new();
    let img = elf::load(&space, bytes).expect("spawn_init: bad ELF");
    let stack_top = space.new_user_stack();
    let rsp = space.init_stack(stack_top, argv, envp, &img);
    let task = Task::new(0, space);
    sched::spawn_user("init", task.clone(), img.entry, rsp);
    task.pid
}

/// `spawn` a statically linked Win64 `.exe`: map the PE and enter its entry
/// point in ring 3 with a bare 16-aligned stack (no SysV block — Win64 entry
/// points take no stack args). Returns the pid. The NT personality (PEB/TEB,
/// `gs` base) is layered on later; a self-contained `.exe` that only makes
/// syscalls runs on this alone.
#[allow(dead_code)] // only the `petest` milestone calls this so far
pub fn spawn_pe(bytes: &[u8]) -> Result<u64, &'static str> {
    let space = Process::new();
    let stack_top = space.new_user_stack();
    let img = crate::pe::load(&space, bytes, stack_top)?; // malformed .exe -> Err, never panic
    // MS x64 ABI: at the entry instruction RSP+8 must be 16-aligned.
    let rsp = (stack_top & !0xF) - 8;
    let task = Task::new(0, space);
    sched::spawn_user_pe("pe", task.clone(), img.entry, rsp, img.teb);
    Ok(task.pid)
}

/// `fork`: eager (non-COW) copy of the caller's address space; the child
/// resumes at the same user instruction with `rax = 0`.
pub fn fork(frame: &UserFrame) -> i64 {
    let parent = sched::current().task().expect("fork: not a user task");
    let pspace = parent.space();

    let cspace = Process::new();
    cspace.copy_alloc_state_from(&pspace);
    pspace.for_each_user_page(|virt, phys, w, x| {
        let f = FRAME_ALLOC.lock().alloc().expect("fork: no frame");
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>(),
                phys_to_virt(f.start_address()).as_mut_ptr::<u8>(),
                4096,
            );
        }
        cspace.map(virt, f.start_address().as_u64(), w, x);
    });

    let child = Task::new(parent.pid, cspace);
    *child.fds.lock() = parent.clone_fds();
    child.set_cwd(parent.cwd());
    let (cs, ss) = user_selectors();
    let mut cf = *frame;
    cf.rax = 0;
    cf.cs = cs;
    cf.ss = ss;
    // The child inherits the parent thread's TLS base — it is CPU state, not
    // memory, so copying the address space alone does not carry it over.
    let fsbase = sched::current().fsbase();
    sched::spawn_user_frame("fork-child", child.clone(), cf, fsbase);
    child.pid as i64
}

/// `execve`: replace the current task's image. Does not return on success.
pub fn execve(bytes: &[u8], argv: &[String], envp: &[String]) -> ! {
    let cur = sched::current();
    let task = cur.task().expect("execve: not a user task");

    let space = Process::new();
    let img = elf::load(&space, bytes).expect("execve: bad ELF");
    let stack_top = space.new_user_stack();
    let av: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let ev: Vec<&str> = envp.iter().map(|s| s.as_str()).collect();
    let rsp = space.init_stack(stack_top, &av, &ev, &img);

    task.close_on_exec(); // drop O_CLOEXEC fds before the new image sees them

    let new_cr3 = space.pml4_phys();
    task.set_space(space);
    cur.set_cr3(new_cr3);
    cur.set_fsbase(0); // fresh image: TLS is re-established by its own arch_prctl

    let (cs, ss) = user_selectors();
    let f = UserFrame {
        rip: img.entry,
        rsp,
        rflags: 0x202,
        cs,
        ss,
        ..Default::default()
    };

    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(0));
    unsafe {
        Cr3::write(
            PhysFrame::from_start_address(PhysAddr::new(new_cr3)).unwrap(),
            Cr3Flags::empty(),
        );
        syscall::thos_user_resume(&f)
    }
}

/// `wait4`: poll for a zombie child (poll + yield; blocking wait comes later).
/// Has task `pid` exited? (`true` also if it has already been reaped away.)
/// For kernel-side milestones that spawn a process and want to await *that*
/// process, not just the first user thread to exit.
#[allow(dead_code)] // only the `pipetest` milestone calls this
pub fn pid_exited(pid: u64) -> bool {
    TASKS.lock().get(&pid).map_or(true, |t| t.exited.load(Ordering::Acquire))
}

pub fn wait4(pid: i64, status_ptr: u64) -> i64 {
    let me = current_pid();
    loop {
        {
            let mut tasks = TASKS.lock();
            let hit = tasks
                .values()
                .find(|t| {
                    t.ppid == me
                        && t.exited.load(Ordering::Acquire)
                        && (pid == -1 || t.pid == pid as u64)
                })
                .map(|t| (t.pid, t.exit_status.lock().unwrap_or(0)));
            if let Some((cpid, status)) = hit {
                tasks.remove(&cpid);
                drop(tasks);
                if status_ptr != 0 {
                    unsafe { *(status_ptr as *mut i32) = (status & 0xFF) << 8 };
                }
                return cpid as i64;
            }
            let has_children = tasks
                .values()
                .any(|t| t.ppid == me && (pid == -1 || t.pid == pid as u64));
            if !has_children {
                return -10; // ECHILD
            }
        }
        CHILD_EXIT.wait_if(|| should_block_in_wait4(me, pid));
    }
}
