# THOS – Large model on little RAM (research track)

**Status:** research track, **not scheduled**. First-pass literature map + where
the open problem is. No code, no milestone. Feeds [`ai.md`](ai.md); the small
from-scratch LM there is unaffected and proceeds independently.
*(direction: user, 2026-09-04 — "develop our own way to run a big model in very
little RAM; excessive research".)*

## The goal, and why it's hard

Run a 10–20B-class model on THOS with a small resident footprint (target
discussion: 6–8 GB RAM + the rest paged to SSD), interactively (low latency,
single stream), on the target machine's **CPU** — THOS has no GPU driver
(Phase 4, years out).

The naïve "MoE: keep active experts in RAM, rest in swap" does **not** work:
routing is per-token and near-random across a sequence, so a few tokens touch
most experts. Measured reality from the offload literature: SSD-mapped layers hit
**~1 s/token latency, ~80 % of it storage→RAM transfer, ~0 % compute**; offload
I/O is dominated by small (~128 KiB) reads. MoE saves **compute**, not resident
memory.

Baselines to beat:
- **Fully resident** gpt-oss-20b-class (MXFP4, ~12–13 GB, 3.6B active/token): on a
  desktop CPU with dual-channel DDR5 (~80 GB/s), ~1.8 GB read/token ⇒ **~15–25
  tok/s** ceiling. Usable — but needs the whole model in RAM.
- **Layer streaming** (AirLLM-style: load layer, compute, discard): runs 70B on
  4 GB, but **5–30× slower**, I/O-bound, "batch not interactive".
- So the question is the middle: *most* of a big model on disk, *usable*
  latency, on CPU.

## What the field already has (build on, don't reinvent)

### A. Weight streaming / offload scheduling
- **FlexGen** — 175B on one 16 GB GPU via a block schedule that overlaps I/O and
  compute; throughput-oriented. [arxiv 2303.06865](https://arxiv.org/abs/2303.06865)
- **DeepSpeed ZeRO-Inference** — layer streaming NVMe→CPU→GPU, prefetch layer N+1
  during N.
- **PRIMA.CPP** — 70B on low-resource home setups: `mmap` weights + *piped-ring
  parallelism with prefetching* to hide disk latency.
  [arxiv 2504.08791](https://arxiv.org/abs/2504.08791)
- **AirLLM** — pure layer-at-a-time streaming; the honest worst case for latency.
- **Endor** — hardware-friendly sparse weight format for offloaded inference (big
  sequential reads instead of scattered).
  [arxiv 2406.11674](https://arxiv.org/abs/2406.11674)
- I/O-characterisation work on offloading weights + KV to NVMe: the small-read
  problem, and that compute is nearly free next to transfer.

### B. Activation sparsity — the real memory-reduction lever
For a given token most FFN neurons contribute ~nothing (~80 % inactive in
OPT-30B). Keep "hot" weights resident, stream/skip "cold" ones.
- **PowerInfer** / **PowerInfer-2** — hot/cold neuron split by power-law
  activation; PowerInfer-2 runs a 47B MoE on a **phone**.
  [arxiv 2312.12456](https://arxiv.org/abs/2312.12456) ·
  [arxiv 2406.06282](https://arxiv.org/abs/2406.06282)
- **LLM in a flash** (Apple) — params on flash, on-demand load, with *windowing*
  (reuse recently-active neurons) + *row–column bundling* (contiguous flash
  reads). ~2× DRAM model size at usable speed.
  [arxiv 2312.11514](https://arxiv.org/abs/2312.11514)
- **CoreInfer**, **DynamicInfer** (runtime-aware sparse offload on a consumer
  GPU), **Q-Infer** (sparsity-aware GPU–CPU scheduling), **"Sparsing Law"**
  (train for *more* activation sparsity).
  [arxiv 2411.02335](https://arxiv.org/abs/2411.02335)

### C. MoE expert prefetching / prediction
- **Mixtral-offloading** — LRU expert cache + speculative expert loading.
  [arxiv 2312.17238](https://arxiv.org/abs/2312.17238)
- Recent: **caching + prefetching analysis** for MoE offload (LFU beats LRU under
  expert imbalance); **spatio-temporal / speculative expert prefetch** — predict
  next-token experts before the router runs (single-layer lookahead ~84–91 %
  accuracy; shadow-network speculation >99 %).
  [arxiv 2511.05814](https://arxiv.org/abs/2511.05814)

### D. Extreme quantisation (shrink the thing you're paging)
- **BitNet b1.58** — ternary weights, but must be *pre-trained* that way.
  [arxiv 2402.17764](https://arxiv.org/abs/2402.17764)
- **ParetoQ** — unified 1 / 1.58 / 2 / 3 / 4-bit; gains "particularly pronounced"
  at 1–2 bit. [arxiv 2502.02631](https://arxiv.org/abs/2502.02631)
- **PTQ1.61**, **AQLM**, **QuIP#** — post-training sub-2-bit without retraining.
  20B @ ~2 bit ≈ **5 GB**. Note: "2-bit reaches higher speed than 4-bit at equal
  accuracy *with optimised CPU kernels*" — relevant, we're CPU-only.

### E. KV cache (often bigger than the weights at long context)
- **KIVI** (per-channel keys / per-token values), **KVQuant**, **MiKV**
  (evicted tokens kept at low precision), **CAKE** / **SAGE-KV** eviction,
  **QEvict** (recoverable quantised eviction). Needed so context doesn't eat the
  RAM budget the weights just freed.

### F. Architectural / model-side
- **Matryoshka / cascade / speculative decoding** — a small always-resident model
  handles the easy 90 %; page in the big model only for hard queries. This is the
  most pragmatic reconciliation of "own small model" + "big model when needed",
  and it maps onto the two tiers already in [`ai.md`](ai.md).
- Cross-layer weight sharing, SSM/Mamba (constant KV in sequence length).

## Where the actual research gap is (the "our own thing")

No published system combines, on a **driver-less CPU OS, latency-first,
single-stream**:

1. **Sub-2-bit weights** (paging 5 GB not 13 GB), plus
2. **activation-sparsity prediction** (resident set = a fraction of even that),
   plus
3. **speculative expert/neuron prefetch** overlapped with compute (hide the SSD),
   plus
4. an **OS that is co-designed for it** — the model's access pattern is *known*,
   so THOS can give it exact prefetch, pinning, and eviction control instead of
   fighting a generic page cache.

Point 4 is the THOS-specific angle: everyone else fights `mmap` + a general page
cache from userspace. THOS can expose a purpose-built interface (the model tells
the kernel "layer N+2's experts {…} next, evict layer N-1") and schedule NVMe
reads against the already-present **AHCI NCQ depth 32**.

## THOS kernel prerequisites (each its own MM project, none exist today)

| Need | Today |
|---|---|
| Demand-paged, **file-backed** `mmap` (weights read-only, no writeback) | `mmap` is anon + bump-allocator only |
| Page-replacement policy (CLOCK / LRU) + a pin set | none |
| Prefetch / drop hints (`madvise(WILLNEED/DONTNEED)` equivalent) + a model-driven variant | none |
| Async, deep block I/O usable *during* compute | AHCI NCQ 32 exists; not wired to a pager |
| A swap device *or* just eviction of clean file pages (simpler — weights are clean) | none |
| Large non-bump allocations for the working set + KV cache | bump `mmap_anon`, no teardown |
| (userland) file writes, a kernel↔userspace channel | missing — see [`ai.md`](ai.md) P3 |

## Rough research agenda (order, not schedule)

- **R0 — measure.** On the target machine, off THOS: NVMe seq/random read
  bandwidth + latency at 4K/128K/1M; DDR5 bandwidth; CPU matmul throughput at
  int2/int4/int8 (AVX2). Establishes the real budget every later number depends
  on.
- **R1 — quantisation choice.** Reproduce a sub-2-bit PTQ (AQLM / PTQ1.61 / QuIP#)
  on a small open model; measure quality vs. bits vs. CPU-kernel speed. Decide the
  target bit-width and format (must be *contiguous-read-friendly*, cf. Endor).
- **R2 — sparsity predictor.** Reproduce PowerInfer/DejaVu-style activation
  prediction on an open dense model; measure hit rate and the resident-set
  fraction it buys. For MoE, reproduce speculative expert prefetch.
- **R3 — access-pattern spec.** Turn R1+R2 into a concrete, deterministic-ish
  memory access trace: what must be pinned, what is prefetchable how far ahead,
  eviction order. This is the contract with the kernel.
- **R4 — THOS pager spike.** Minimal file-backed demand-paged `mmap` + CLOCK
  eviction + a `prefetch(range)` / `pin(range)` syscall, NVMe reads via NCQ.
  Benchmark against the R3 trace with a stub "model" that just touches memory.
- **R5 — end to end.** The R1 quantised + R2-sparse model, its R3 access pattern,
  on the R4 pager. Target: a 10–20B-class open model, ≥ a few tok/s interactive,
  ≤ ~8 GB resident. Go / no-go on the whole approach.

## Open decisions

1. **Target-machine RAM** — still unknown, and it decides whether 20B is even
   plausible resident-with-headroom vs. capped at ~7–13B. *(need from user)*
2. **Own weights vs. open weights** — training a 10–20B from scratch on CPU is
   impossible; "like gpt-oss" ⇒ run its (Apache-2.0) open weights, which relaxes
   the [`ai.md`](ai.md) "own weights from zero" rule for *this* track. The
   from-scratch small LM keeps that rule. Confirm the split. *(need from user)*
3. Bit-width / weight format (R1).
4. Dense + sparsity vs. MoE + expert-prefetch as the primary structure (R2/R3) —
   or both, via the cascade in F.
5. How much of the pager is general-purpose MM (useful anyway) vs. AI-specific.

## Honesty

- This is **12–24 months** of research + systems work sitting on top of kernel MM
  that also has to be built, on the slowest possible hardware path (CPU, no GPU
  driver). It is defensible, not scheduled.
- If R0/R1/R2 show the numbers don't close on this hardware, the fallback is the
  cascade (F): a small resident model does most of the work, the big model is an
  occasional, slow, deliberate call — which is still "using the RAM sensibly",
  just honest about the big model being cold.

See [`ai.md`](ai.md) · [`feasibility.md`](feasibility.md) · [`roadmap.md`](roadmap.md).
