# THOS – Licensing strategy (DECIDED 2026-08-29)

**Decision:** the THOS kernel tree is **GPL-2.0-or-later**. The frozen Linux-distro
components keep **AGPL-3.0**. Vendored code under `third_party/` keeps its upstream
license. Every source file carries an `SPDX-License-Identifier` header.

- `kernel/ hal/ personalities/ loaders/ drivers/ boot/ xtask/` + THOS build files
  → `GPL-2.0-or-later` (keeps the door open for a future `amdgpu` port without a second
  relicensing round, and stays GPL-3 compatible).
- `tarnod/ tarno-guard-ebpf/ tarno-desktop/ tarno-installer/ tarno-ui-theme/
  tarno-br2-external/ scripts/` → unchanged **AGPL-3.0** (see `FROZEN.md`).
- `third_party/*` → upstream licenses, vendored unmodified.
- Root `LICENSE` (AGPL-3.0 full text) stays as the repo default for the frozen parts.
- TODO: add `LICENSES/GPL-2.0-or-later.txt` (full text) and a `.reuse/dep5` map.

### Does hosting on GitHub complicate this?

No. Pushing a public repo grants other GitHub users a view/fork right (GitHub ToS D.4);
your chosen open-source license governs everything else. A public repo with a
GPL-2.0-or-later kernel tree and AGPL-3.0 frozen dirs is a normal, valid setup. The only
real constraint is **license compatibility between files that get linked together** —
handled by the per-directory split above and by keeping `amdgpu` (GPL-2.0-only) out
unless/until Path A is chosen.

---

## Background (analysis that led to the decision)

The repository is currently **AGPL-3.0** (`LICENSE`). THOS wants to reuse several large
third-party codebases. Their licenses interact and must be settled before writing code
that links them.

## Components and their licenses

| Component | License | Interaction with AGPL-3.0 |
|---|---|---|
| **ACPICA** (ACPI) | Intel dual license / BSD-3-Clause-ish permissive | Compatible. Safe to vendor under `third_party/acpica/`. |
| **Mesa / RADV** (Vulkan userspace) | MIT (core), some SGI-B | Compatible. Ships as a separate userland component. |
| **Linux `amdgpu`** (KMS, if Path A) | GPL-2.0-**only** | **Incompatible** with AGPL-3.0 for combined linking. If Path A is chosen, the KMS driver must be an isolated GPL-2.0 component with a clean syscall/ioctl boundary — not statically linked into an AGPL kernel — or Path B must be chosen. |
| **Wine** PE-built DLLs | LGPL-2.1-or-later | Compatible if used as **separately distributed dynamic libraries** loaded by the NT personality, not statically linked into GPL/AGPL code. |
| **ReactOS** DLLs | GPL-2.0-only (some), LGPL-2.1 (some) | GPL-2.0-only parts are **incompatible** with AGPL-3.0. Prefer Wine over ReactOS for any reused DLL. |
| **musl** | MIT | Compatible. Userland component. |
| **smoltcp** | 0BSD/MIT | Compatible. |
| **Limine** | BSD-2-Clause | Compatible. Bootloader, not linked into the kernel. |

## The core problem

AGPL-3.0 (and GPL-3.0) are **incompatible with GPL-2.0-only**. The Linux `amdgpu` driver
is GPL-2.0-only. So:

- **Option 1 — keep AGPL-3.0, choose GPU Path B.** Write the RDNA2 KMS ourselves;
  reuse only permissively licensed userspace (Mesa is MIT). Wine DLLs stay as separate
  LGPL dynamic libraries. Cleanest legally; more kernel work.
- **Option 2 — relicense the THOS kernel tree to GPL-2.0-only (or dual GPL-2.0/-3.0).**
  Enables porting `amdgpu` (Path A) directly. Requires consent of all THOS contributors
  (currently just the repo owner — easy now, hard later). The frozen Tarno-OS components
  can keep their own license.
- **Option 3 — split licensing per directory.** Kernel + executive: permissive or
  GPL-2.0; personality DLLs: their upstream licenses; keep AGPL only for network-facing
  userland services (where AGPL's "provide source to network users" clause actually
  matters — it does **not** meaningfully apply to a kernel).

## Recommendation

**Option 1 + a per-directory `LICENSE` map**, decided now while the contributor set is
one person:

- `kernel/`, `hal/`, `personalities/`, `loaders/`, `drivers/` → **GPL-2.0-or-later**
  (keeps the door open for Path A later without a second relicensing round).
- `third_party/*` → upstream licenses, unmodified, vendored with `LICENSE` files.
- `userland/` THOS-authored services → AGPL-3.0 if desired.
- Add a top-level `LICENSES/` directory and SPDX headers on every source file.

**Action item (Phase 0):** repo owner confirms the relicense of the new `kernel/`-tree
directories to GPL-2.0-or-later in writing (a commit message / `AUTHORS` note is enough
today), and we add SPDX headers to the skeleton.
