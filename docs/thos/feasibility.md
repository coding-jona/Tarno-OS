# THOS – Feasibility (honest version)

This document exists so that scope decisions are made with eyes open. THOS is achievable
in layers; some layers are large-but-known, one is genuinely a research risk.

## Tiers

| Tier | Scope | Reality |
|---|---|---|
| **A — large but known** | Executive core, both personalities (userspace level), ELF + PE loaders, VFS, AHCI/xHCI, POSIX libc | Prior art exists (Wine PE build, historical Longene, standard OS-dev). Bounded, well-understood. Est. **5–15 person-years** to production quality. |
| **B — very hard, finite** | Kernel-side NT semantics done properly (Ob/Ps/Io/Se, SEH, APC, `\Device` namespace, registry), P/E-aware scheduler with HFI | Requires care and a lot of surface, but no unknown physics. Folded into Tier A phases. |
| **C — research risk** | GPU driver for Navi 23 (KMS + accel) **from scratch**; real WDDM interception; loading third-party `.sys` drivers | The GPU driver is unavoidable and huge — mitigated by porting Mesa/amdgpu. WDDM interception and `.sys` loading are deferred to a research track with explicit Go/No-Go. |

## The GPU driver (Phase 4) — why it's the pole

Writing a Vulkan driver + shader compiler for RDNA2 from scratch is ~10+ person-years on
its own (see the history of Mesa RADV + ACO). Therefore:

- **Userspace: port Mesa RADV.** Do not reimplement.
- **Kernel: Path A** (port Linux `amdgpu` KMS — a big C port, in tension with the
  clean-slate goal) **or Path B** (write a thin RDNA2 KMS: DCN modeset, GPUVM, PM4 rings,
  SMU power; add a DRM-ioctl compat layer so unmodified RADV runs on it). Decide after a
  2–4 week spike at the start of Phase 4.
- No iGPU on the i7-13700KF ⇒ **no fallback display path**. UEFI GOP gives a linear
  framebuffer for free at boot; a real KMS driver is only needed once GOP is
  insufficient (mode changes, acceleration, multiple surfaces).

## `.sys` drivers (Phase 6) — documented blockers

Go/No-Go gate before Phase 6. Known blockers:

- **No signature / code-integrity chain.** Microsoft does not sign `.sys` for a foreign
  kernel; KMCI does not apply. Loading binary blobs into ring 0 is pure attack surface.
- **Struct-layout dependency.** WDM/KMDF drivers read fields of `_ETHREAD`, `_KPCR`,
  `_EPROCESS`, `_IRP`, `_DEVICE_OBJECT` at fixed offsets. THOS must reproduce these
  layouts bit-exactly for a given target Windows build.
- **IRQL semantics.** `DISPATCH_LEVEL` permits things forbidden in a Linux/THOS atomic
  context and vice versa. THOS's own preemption model can be designed to express IRQL
  from day one — an advantage over patching Linux — but it is still new code under
  every driver.
- **PnP / power.** The full IRP state machine, `IoCompletion`, power IRPs — hundreds of
  `ntoskrnl`/`hal` exports — must exist before a real driver initializes.
- **Environment probing.** Anti-cheat and DRM `.sys` deliberately detect non-genuine
  kernels and refuse to run.
- **Scope creep.** Generalizing beyond one device class trends toward 20–100+
  person-years. Phase 6 is therefore hard-limited to **NDIS networking** as a proof of
  concept; GPU/storage/USB via `.sys` are out of scope permanently.

## What "done" means per milestone

See [`roadmap.md`](roadmap.md). Each milestone has a concrete, automatable pass/fail
check in the plan's verification section.

## If the project stops early

- After **Phase 2**: a small, fast, single-machine Linux-ABI-compatible OS with its own
  clean kernel. Useful as an appliance / learning platform.
- After **Phase 3**: the above, plus console Win32 programs run natively — a genuinely
  novel hybrid userland.
- After **Phase 4**: GUI Windows apps and D3D games run. This is the point where THOS
  replaces a dual-boot for the target machine's owner.
