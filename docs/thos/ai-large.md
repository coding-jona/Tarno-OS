# THOS – Large model on little RAM (research track)

**Status:** research track, **not scheduled**. First-pass literature map + where
the open problem is. No code, no milestone. Feeds [`ai.md`](ai.md); the small
from-scratch LM there is unaffected and proceeds independently.
*(direction: user, 2026-09-04 — "develop our own way to run a big model in very
little RAM; excessive research".)*

## Target machine (fixes the numbers)

- **16 GB RAM, DDR4-3600 (OC), dual channel** ⇒ ~57.6 GB/s peak, ~45 GB/s
  sustained. This is the resident-inference ceiling.
- CPU: i7-13700KF (AVX2, no AVX-512), 24 threads.
- No GPU driver in THOS ⇒ CPU inference only.
- **Boot/root disk: Kingston A400 240 GB SATA SSD** (`KINGSTON SA400S37240G`,
  per [`hw-target.md`](hw-target.md)). SATA 6 Gb/s, **DRAM-less controller**, seq
  read ~450–500 MB/s *peak*, random / sustained far worse. This is near the
  slowest SSD still sold — and it is the decisive number for the streaming tier.

### What 16 GB actually allows

| Model | Bits | Weights | Fits resident? | CPU speed (est.) |
|---|---|---|---|---|
| ~7–8 B | 4-bit | ~4 GB | yes, easily | ~6–10 tok/s |
| ~13 B | 4-bit | ~7 GB | yes | ~4–7 tok/s |
| **~20 B** | **2-bit** | **~5–6 GB** | **yes, ~10 GB headroom** | **~3–5 tok/s** |
| ~30 B | 2-bit | ~8 GB | yes, tight-ish | ~2–4 tok/s |
| gpt-oss-20b | MXFP4 | ~13 GB | barely, no headroom | ~3–5 tok/s |
| 70 B+ | 2-bit | ~20–35 GB | **no → must stream** | SSD-bound (below) |

**Key consequences:**

1. With sub-2-bit quantisation a **20–30 B model is resident on this box** — no
   paging, no AirLLM. The SSD is touched only for the one-time weight load
   (~6 GB ÷ ~0.45 GB/s ≈ 13 s). This tier needs only **R1 (quant) + a
   big-allocation allocator + KV compression** — *not* the pager / sparsity /
   prefetch stack. It is achievable comparatively soon and a 20–30 B open model
   at 2-bit is genuinely capable, not a toy.
2. The **streaming tier (70 B+) is not viable on the A400.** ~20 GB streamed per
   token ÷ ~0.45 GB/s (and worse — the access pattern is not purely sequential,
   and the controller is DRAM-less) ⇒ **~40–90 s/token → a 200-token answer is
   1.5–5 h**. That is not a "deep think" call, it is a batch job you submit and
   read tomorrow. Documented, but it needs a **different disk (NVMe)** to be
   worth building.

So on the actual target hardware the plan is the **resident 20–30 B tier**, plus
the cascade (F): the always-resident small from-scratch LM handles the
interactive 90 %, the resident 20–30 B model is the "think harder" step. A third
huge-streamed tier is deferred until/unless the disk changes.

## The goal

Run the largest useful open model THOS can on this exact machine (16 GB
DDR4-3600, i7-13700KF, Kingston A400 SATA), CPU-only, as the "think harder" step
behind the always-resident small from-scratch LM.

Two ideas the user raised and where they land:
- **"MoE: active experts in RAM, rest in swap"** — does **not** work: routing is
  per-token and near-random, so a few tokens touch most experts. MoE saves
  *compute*, not resident memory. Offload literature: SSD-mapped layers hit
  ~1 s/token, ~80 % of it storage→RAM transfer, ~0 % compute, dominated by small
  (~128 KiB) reads. On the A400 (slower than those NVMe setups) it is worse.
- **"Tokens don't matter because the small local model handles the fast stuff"**
  — correct, and it is what makes a *slow* big model acceptable. But on the A400
  the streamed 70 B+ path is 1.5–5 h per answer (above), which is past
  "acceptable" into "different disk required". So the big model has to be one
  that **fits in RAM** — which, at 2-bit, is 20–30 B.

Reference point: fully-resident gpt-oss-20b-class (MXFP4, ~13 GB) on DDR5
(~80 GB/s) gets ~15–25 tok/s; on this box's DDR4-3600 (~45 GB/s sustained) and at
2-bit for ~20 B, expect **~3–5 tok/s**. Slow, but it is a deliberate deep-think
step, not the interactive surface.

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

Primary path = the **resident 20–30 B tier** (R0–R2). The pager/sparsity/prefetch
work (R3–R5) is only for a later, NVMe-dependent streaming tier.

- **R0 — measure.** On the target machine, off THOS: DDR4-3600 sustained
  bandwidth; CPU matmul throughput at int2/int4/int8 (AVX2, 24 threads); A400 seq
  + random read (informs load time, not a hot path). Establishes the real budget.
- **R1 — quantisation choice.** Reproduce a sub-2-bit PTQ (AQLM / PTQ1.61 /
  QuIP# / ParetoQ) on a small open model; measure quality vs. bits vs. CPU-kernel
  speed. Decide the target bit-width so a ~20–30 B open model lands in ~5–8 GB
  with acceptable quality.
- **R2 — CPU inference engine.** A `no_std`-friendly forward pass for the chosen
  open architecture at the chosen bit-width (int2/int4 matmul kernels, GQA, RoPE,
  RMSNorm, SwiGLU; MoE routing if the chosen model is MoE). Plus KV-cache
  quantisation (KIVI-style) so context doesn't eat the headroom. Target on this
  box: ~3–5 tok/s, ≤ ~8 GB. **This is the go/no-go for a usable big local model.**
- **R2.5 — activation sparsity (optional).** PowerInfer/DejaVu-style prediction
  to cut the per-token FLOPs/bandwidth further — pure speedup, still resident.
- **R3–R5 — streaming tier (deferred, needs NVMe).** Access-pattern spec →
  THOS file-backed demand-paged `mmap` + CLOCK eviction + `prefetch`/`pin`
  hints + NCQ async reads → 70 B+ end to end. Only if the disk changes.

## Open decisions

1. ~~Target-machine RAM~~ — **16 GB DDR4-3600** (*user, 2026-09-04*). ⇒ 20–30 B
   resident at 2-bit; streaming only forced above ~30 B (see table above).
2. ~~SSD generation~~ — **Kingston A400 SATA** (*user, 2026-09-04*), one of the
   slowest SSDs sold ⇒ the 70 B+ streaming tier is impractical on this box
   (hours/answer). Resident 20–30 B is the plan; revisit streaming only if the
   disk becomes NVMe.
3. **Own weights vs. open weights** — training a 20–30B from scratch on CPU is
   impossible; this tier runs an **open** model (Apache-2.0 / similar), which
   relaxes the [`ai.md`](ai.md) "own weights from zero" rule for *this* track.
   The from-scratch small LM keeps that rule. Confirm the split. *(need from
   user)*
4. Which open base model (R1) — a dense ~24–30 B (simpler engine) vs. an MoE like
   gpt-oss-20b (needs routing, ~13 GB at MXFP4 leaves little headroom at 16 GB).
5. Bit-width / weight format (R1); KV-cache quantisation scheme (R2).
6. Whether R2.5 activation sparsity is worth the extra machinery for the speedup.

## Honesty

- The **resident 20–30 B tier** (R0–R2) is a bounded engineering project — a
  quant pipeline + a CPU inference engine + KV compression. No new kernel MM. A
  2-bit ~24 B open model at ~3–5 tok/s on this box is realistic and genuinely
  capable. This is the plan.
- The **streaming tier** (70 B+, R3–R5) is 12–24 months of research + kernel MM
  and is **not viable on the Kingston A400** (hours per answer). Deferred until
  the disk is NVMe, or dropped.
- Either way the interactive surface is the always-resident small from-scratch
  LM; the big model is a deliberate "think harder" call. That is "using the RAM
  sensibly", honestly scoped.

See [`ai.md`](ai.md) · [`feasibility.md`](feasibility.md) · [`roadmap.md`](roadmap.md).
