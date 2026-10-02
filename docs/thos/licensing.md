# THOS – Licensing strategy (DECIDED 2026-08-29, confirmed 2026-10-02)

**Decision: the whole project is GPL-2.0-or-later.** Every source file carries
`SPDX-License-Identifier: GPL-2.0-or-later`; the full text is in
[`LICENSES/GPL-2.0-or-later.txt`](../../LICENSES/GPL-2.0-or-later.txt) and the root
[`LICENSE`](../../LICENSE) (GPL-2.0). `.reuse/dep5` covers files that cannot carry a
header (docs, configuration, data, binaries).

**Why GPL-2.0 and not AGPL-3.0** (the project was AGPL-3.0 until 2026-08-29): the
Linux `amdgpu` GPU driver is **GPL-2.0-only**, which is *incompatible* with AGPL-3.0
and GPL-3.0. Choosing GPL-2.0-or-later keeps the door open to port `amdgpu` (GPU
path A, see below) without a second relicensing round — and `-or-later` still lets
the code be used under GPL-3. The licence of the root `LICENSE` file was switched
from AGPL-3.0 to GPL-2.0 on 2026-08-29 (commit `4830a08`, made through the GitHub web
UI) as part of this decision.

**Exceptions**
- `third_party/*` keeps its upstream licence, vendored unmodified.
- `kernel/font/Lat15-Terminus16.psf` is the Terminus Font under the **SIL OFL 1.1**
  (`LICENSES/OFL-1.1.txt`, attribution in `kernel/font/README.md`).

**History / the frozen Devuan parts.** Before 2026-08-29 the whole repository was
AGPL-3.0 (`LICENSE`, commit `a68ffef`, 2026-08-22). An earlier version of this document
said the frozen Devuan/Go components "keep AGPL-3.0" and that the root `LICENSE` was
AGPL; that was wrong — the `LICENSE` file had already been changed to GPL-2.0 45 minutes
before this document was written, and the frozen components had no SPDX headers. As of
2026-10-02 they carry `GPL-2.0-or-later` like everything else (the earlier AGPL
reasoning — its network clause only matters for network-facing services — was not
worth a second licence in one tree).

**Contributors.** Two commits by a third contributor (`kirby`, 2026-08-22, 68 lines:
`tarno/mistral.go`, `tarno/provider.go`, `tarno/tarno.go`, `go.mod`, `go.sum`,
`.gitignore`, a lint workflow) were made while the repository was AGPL-3.0. Relicensing
those lines formally needs that contributor's consent; the lines are small and in the
frozen part. **Open action:** consent is being obtained (the contributor is a personal contact of the maintainer); the request text and the record of the answer are in [`relicensing-consent.md`](relicensing-consent.md). Until it says "agreed", rewrite is the fallback.

**Copyright holder line.** `.reuse/dep5` currently says "THOS contributors (see the git
history)"; replace it with the maintainer's chosen name if wanted.

---

### Does hosting on GitHub complicate this?

No. Pushing a public repo grants other GitHub users a view/fork right (GitHub ToS D.4);
your chosen open-source license governs everything else. A public repo with a
GPL-2.0-or-later tree (with a vendored OFL font and upstream-licensed `third_party/`) is a normal, valid setup. The only
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
