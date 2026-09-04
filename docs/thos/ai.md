# THOS – In-system AI

**Status:** P0 "Proof of Life" done (`ml/` — pipeline trained + cross-checked).
A near-separate subproject. *(direction set by user, 2026-09-04)*

## The three tracks

This doc is the entry point. The AI effort has three tracks that share the `ml/`
stack and the `.tlm` format:

| Track | What | Doc | State |
|---|---|---|---|
| **A — small LM from scratch** | Own architecture + weights + `#![no_std]` Rust inference. Byte→BPE, ~1M→tens of M params, CPU-trained on open data. The interactive surface + the exec-gate/AV head. | *this doc* | P0 done |
| **B — rethink context** | A learned active-memory mechanism (window + gated memory + retrieval over cold KV) so a small resident footprint behaves like a huge context. Trained into Track A's model. | [`ai-context.md`](ai-context.md) | research, not scheduled |
| **C — big open model, resident** | A ~20–30 B *open* model quantised to ~2-bit so it fits in the 16 GB RAM (~3–5 tok/s on CPU). The "think harder" step. Relaxes "own weights" — for this track only. | [`ai-large.md`](ai-large.md) | research, R0–R2 |

**Product shape = a cascade.** Track A's always-resident small LM answers the
interactive 90 %; when it punts, Track C's resident 2-bit ~24 B open model does
the deliberate deep-think. Track B's memory work applies to A first and can later
inform how C's KV is managed. There is **no fourth "stream a 70 B from disk"
tier** — the target's Kingston A400 SATA SSD makes it hours-per-answer
([`ai-large.md`](ai-large.md)).

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

## Consolidated build order — what we actually build

Best pieces from all three tracks, in dependency order. Only the first three are
shovel-ready; the rest are gated on THOS kernel work or research outcomes.

1. **Track A P1 — a real small LM.** *(next)* Byte-level BPE (~8–16 k), ~20–40 M
   params, a larger open corpus (`ml/train/fetch.py` windowed), longer CPU runs,
   a proper eval (perplexity + a needle probe). Ship as `.tlm`. This is the
   spine everything else hangs off.
2. **Track C R0 — measure the box.** *(parallel, cheap)* A benchmark script:
   DDR4-3600 sustained bandwidth, AVX2 int2/int4/int8 matmul throughput on 24
   threads, A400 read profile. No commitment — it makes every later number real.
3. **Track B v1 experiment.** *(after 1)* `ml/train/model_mem.py`: sliding-window
   attention + a learned gated memory (Titans/RMT-style) + kNN retrieval over an
   on-disk KV store. Train vs. the vanilla baseline on the same corpus/CPU-time;
   score on a synthetic needle-in-a-haystack at 1 K–512 K logical context with a
   ≤ 32 K resident footprint. Winner's forward path goes into `thos-lm` as an
   alternate model kind.
4. **Track C R1–R2 — the big-model engine.** A sub-2-bit PTQ (AQLM / QuIP# /
   ParetoQ) on a chosen ~24–30 B open base, plus a `no_std`-friendly CPU
   inference engine (int2/int4 kernels, GQA, RoPE, RMSNorm, MoE routing if
   applicable) + KV-cache quantisation. Target: ~3–5 tok/s, ≤ ~8 GB resident.
5. **Track A P2 — host integration.** `thos-lm` verified `#![no_std]`-clean for
   `x86_64-unknown-none`; `cargo xtask lm-demo` host command.
6. **THOS runtime prerequisites (P3).** Userland `O_CREAT` + write; a
   kernel↔userspace query channel; a RAM budget for a resident model. Each is its
   own roadmap item.
7. **THOS AI service (P4)** and **exec-gate head (P5)** — as below.

**Deferred / dropped:** streaming a 70 B+ model from the A400 (hours/answer —
needs NVMe); training anything ≥ 1 B from scratch (no GPU).

**Large open models — the wider picture** — see [`ai-large.md`](ai-large.md) for
the full analysis (why MoE-in-swap fails, the resident-vs-streaming split, the
KV-budget / long-context stack).

**Milestone AI-0:** `cargo test -p thos-lm --target x86_64-unknown-linux-gnu`
passes (Rust forward matches the numpy reference; sampler deterministic) **and**
`cargo build -p thos-lm --target x86_64-unknown-none` compiles. *(met)*

**Milestone AI-1:** a byte-level ~1M model trained on the CPU spike corpus
reaches val loss well below the uniform-byte baseline (ln 256 ≈ 5.55) and
`ml/thos-lm --example generate` emits recognisable English fragments. *(met —
0.84 M params, 20 k steps in ~2¼ h on 16 threads at ~42 k tok/s, final val loss
**1.197 nats/byte ≈ 1.73 bits/byte**; the `thos-lm` Rust engine generates
grammatical clauses, dialogue with attribution, and training-corpus vocabulary
("Rostóv", "the whale", "Chapter").)*

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
8. Track B: whether the v1 learned-memory architecture beats the eviction
   baseline at small scale, and whether it transfers ([`ai-context.md`](ai-context.md)).
9. Track C: which open base model, and target bit-width ([`ai-large.md`](ai-large.md)).

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
[`ai-context.md`](ai-context.md) · [`../../ml/README.md`](../../ml/README.md).
