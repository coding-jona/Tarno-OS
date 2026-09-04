# THOS – In-system AI

**Status:** planning + a P0 "Proof of Life" spike (`ml/`). No kernel code. A
near-separate subproject — it gets a plan before scope leaks into the OS work.
*(direction set by user, 2026-09-04)*

## Decision

Build a **real small language model from scratch**, and grow it step by step —
not a one-off classifier. Concretely:

- **Own architecture, own weights, own inference.** PyTorch is only the training
  compute frame; the model design, the trained weights, and the shipped
  inference engine are all first-party.
- **Train off-device**, CPU-only, on the dev box (i7-13700KF, 24 threads, AVX2).
  No usable GPU, no cloud.
- **Ship a hand-written `#![no_std]` Rust engine** (`ml/thos-lm`): parses the
  `.tlm` weight format, runs the forward pass and sampling with `core` + `alloc`
  + `libm` — no `std`, no external tensor library, no SIMD intrinsics.
- **Only open / permissively-licensed / public-domain training data.** Every
  source recorded in [`../../ml/DATASETS.md`](../../ml/DATASETS.md). No
  proprietary lab mixes, no pirated book corpora, no scraped commercial verdicts.
- **`ml/` subtree**; weight blobs are **not** committed (code + resumable
  downloader + configs reproduce them).

This rules out an LLM *inside the kernel* and rules out a chatbot-class model in
the near term — see Non-goals.

## Constraints

| Area | Reality |
|---|---|
| Compute | CPU only (i7-13700KF, AVX2, no AVX-512). RX 6600 / ROCm not worth it. → small models, byte-level tokens, long CPU runs. |
| Internet | Drops daily at 00:00 (02:00 Fri→Sat, Sat→Sun). `ml/train/fetch.py` is fully resumable; the v0 corpus fits one window. Training needs no internet. |
| Kernel | Zero `f32`/`f64` today; SSE enabled but **no FPU/SIMD-state save across a preemption or the syscall boundary** (`sched.rs` saves 6 regs). 32 MiB heap. → LM inference cannot run in the kernel; one `forward` call must run to completion. |
| THOS userland | Linux x86-64 subset. **No** sockets / named pipes / shared memory / device nodes / custom syscall; **userland can't create or write files** (`open` has no `O_CREAT`). 512 MiB RAM, 64 KiB stacks, `mmap` anon-only, no address-space teardown. → a THOS-hosted model needs new kernel surface first (phase P3). |

## Architecture

**Train / infer split.** `ml/train/` (Python) owns data, tokenizer, model
definition, the CPU training loop, and the exporter. `ml/thos-lm/` (Rust,
`no_std`) owns *only* loading `.tlm` + forward + sampling. A numpy reference
(`ml/train/tlm.py:numpy_forward`) is the oracle the Rust engine is checked
against; the golden test (`ml/thos-lm/tests/golden.rs`) enforces agreement to
< 1e-4 (currently ~4e-8).

**`.tlm` weight format** — little-endian: a 44-byte header (magic `TLM1`,
version, `n_layer/n_head/n_embd/block_size/vocab_size`, flags, `norm_eps`) then
raw `f32` tensors in a fixed declared order. Documented in
`ml/thos-lm/src/tlm.rs`; produced by `ml/train/export.py`. Loadable from a file
(host / eventual THOS userland) or `include_bytes!` (eventual kernel use for
Application B).

**Model (v0).** Decoder-only "GPT-style": byte tokenizer (vocab 256), learned
positional embeddings, pre-norm LayerNorm, fused QKV, causal MHA, tanh-approx
GELU MLP, tied output projection. Deliberately plain so the three
implementations stay in lockstep.

**"Own logic from zero" boundary.** PyTorch trains; it is not the model. The
architecture, the weights, and every line of the shipped forward pass are ours.
`libm` (soft-float `expf`/`tanhf`/`sqrtf`) is the one dependency — the same
"soft, no CPU-feature dispatch" stance as the kernel's `sha2 force-soft`.

## Applications

- **A — text generation / completion.** The LM itself. P0–P2, P6.
- **B — exec-gate / AV scorer.** *(was the standalone `ai.md` plan; now folded
  in as an application of this stack.)* A small classifier head reusing
  `thos-lm`'s tensor + loader code, scoring a PE/ELF as benign/suspicious from
  **static** features (import set, section layout, entropy, header anomalies,
  overlay, TLS-callback abuse, entry-point section) *before* it runs. Ties into
  the [`roadmap.md`](roadmap.md) security-architecture section and
  `memory: thos-security-architecture`. Default-deny high scores with admin
  override, matching the age-gate model. Phase P5.
- **C — syscall / behaviour anomaly detection.** Later; likely a sequence model
  in a Tier-2 userspace service.

## Data & provenance

See [`../../ml/DATASETS.md`](../../ml/DATASETS.md). Hard lines: no proprietary
mixes, no pirated corpora, no scraped commercial verdicts. Licence obligations
(e.g. CC BY-SA share-alike on derived text) are tracked per source. The open web
corpora the big labs also use (FineWeb, The Stack v2 with opt-out, Wikipedia,
Gutenberg, arXiv/PMC-OA, StackExchange) are the legitimate growth path.

## Phased plan

- **P0 — Proof of Life** *(done: pipeline wired + cross-checked)*: `ml/` skeleton;
  resumable fetch of a tiny open corpus; byte tokenizer; ~1M-param GPT trainable
  on CPU (`config/spike-1m.toml`); `.tlm` export; `thos-lm` loads + samples;
  committed golden test (Rust `forward` ≈ numpy ref on fixed weights, Rust
  sampler deterministic).
- **P1 — usable tiny LM**: BPE tokenizer (~8–16k); ~10–50M params; larger open
  corpus (windowed multi-day fetch); longer CPU runs; eval (perplexity + probes);
  optional `q8` weights.
- **P2 — host-side THOS integration**: `thos-lm` kept `#![no_std]`-clean for
  `x86_64-unknown-none`; a host `cargo xtask lm-demo` that runs generation;
  decide `include_bytes!` vs ext2-file weight delivery.
- **P3 — THOS runtime prerequisites** *(separate roadmap items, not ML work)*:
  userland `O_CREAT`+write; a kernel↔userspace query channel (`\Device\ThosLM` or
  a syscall); a RAM / `mmap` budget for a ~10–50 MB model.
- **P4 — THOS userspace AI service**: ship `thos-lm` as a host-cross-compiled
  static-musl process, started by init, weights from ext2, queried over the P3
  channel. `cargo xtask lm-test` boots THOS and asserts a serial marker from a
  canned generation.
- **P5 — Application B (exec-gate)**: static PE/ELF feature extractor (reuses the
  loader parsing in `pe.rs` / `elf.rs`); labelled *open* dataset (EMBER-style,
  licensed); classifier head; wire into `elf::load` / `pe::load` behind an
  `execgate` feature; log verdicts, then enforce.
- **P6+ — grow** as compute / RAM allow.

**Large open models on little RAM** — running a 10–20B-class model on the CPU
with most of it paged to SSD (sub-2-bit weights + activation-sparsity prediction
+ speculative prefetch + a purpose-built THOS pager) is its own **research
track**, not scheduled: [`ai-large.md`](ai-large.md). It relaxes "own weights
from zero" (it would run open weights); this small-LM track keeps that rule.

**Milestone AI-0:** `cargo test -p thos-lm --target x86_64-unknown-linux-gnu`
passes (Rust forward matches the numpy reference; sampler deterministic) **and**
`cargo build -p thos-lm --target x86_64-unknown-none` compiles. *(met)*

**Milestone AI-1:** a byte-level ~1M model trained on the CPU spike corpus
reaches val loss well below the uniform-byte baseline (ln 256 ≈ 5.55) and
`ml/thos-lm --example generate` emits recognisable English fragments.

## Open decisions

1. `.tlm` layout details + quantisation (`q8` / `q4`); tensor layout for a KV
   cache.
2. P1 tokenizer algorithm (byte-level BPE vs. unigram).
3. `thos-lm` `f32` vs. fixed-point for the eventual in-kernel Application B.
4. Weight delivery into THOS: `include_bytes!` (kernel `.rodata`) vs. a file on
   the ext2 image.
5. Shape of the kernel↔userspace query channel (its own design doc before P3/P4).
6. `ml/` licence — default `GPL-2.0-or-later` to match the kernel tree (since
   `thos-lm` may be vendored into it); revisit if a permissive licence is wanted.
7. Whether CC BY-SA source text imposes share-alike on distributed *weights*
   (see `ml/DATASETS.md`).

## Non-goals

- No runtime dependency on external services or third-party model runtimes
  (llama.cpp / ONNX / a bundled GGUF).
- No training in the kernel; no GPU assumption for training.
- Not a chatbot product in the near term.
- A billions-of-params LLM trained from scratch is out of scope until the GPU
  driver ([`feasibility.md`](feasibility.md) Phase 4) and the compute exist.
  *Running* a large open model on little RAM is a separate research track
  ([`ai-large.md`](ai-large.md)), also unscheduled.

See [`roadmap.md`](roadmap.md) · [`architecture.md`](architecture.md) ·
[`feasibility.md`](feasibility.md) · [`ai-large.md`](ai-large.md) ·
[`../../ml/README.md`](../../ml/README.md).
