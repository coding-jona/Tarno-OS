# THOS – Rethinking context (research, on the small-LM track)

**Status:** research direction, **not scheduled**. Belongs on the **from-scratch
small-LM track** ([`ai.md`](ai.md)) — a new memory mechanism must be *trained in*,
and only a small model is CPU-trainable on this hardware. Not applicable to the
big open model ([`ai-large.md`](ai-large.md)), which keeps a standard transformer
and just manages its KV.
*(direction: user, 2026-09-04 — "don't stack context hacks; rework how context
works, for efficiency *and* performance".)*

## The premise to break

Today "context" = a flat, append-only log of tokens; every token gets a verbatim
KV vector per layer; attention compares a query against all (retained) keys; KV
is immutable once written — you can only evict, quantise, or retrieve it. Cost is
**O(n)** memory and **O(n)** (or O(n²) at prefill) compute. Every "long context"
method today is a patch on this: drop some KV, shrink some KV, or fetch some KV.

The bet: a context mechanism designed as **active, learned memory** can be
**O(1)–O(log n)** in the resident footprint *and* recall better than
eviction-based patches — because it decides what to keep by learned importance,
not by recency or a fixed heuristic.

## Levers (with closest prior art)

1. **Mutable memory, not an append-log.** Model *writes* into a fixed-size
   associative memory and *updates* it with learned read/write/forget ops.
   → NTM / DNC; Infini-attention; RMT; **Titans** ("learning to memorize at test
   time" — memory adapted at inference by a surprise signal). The mainstream is
   already moving here; it is early and open.
2. **Multi-resolution.** Recent tokens verbatim; older ones progressively
   compressed (word → phrase → paragraph → chapter gist); attend at the query's
   resolution; **decompress on demand** back to detail.
   → Compressive Transformer; hierarchical attention; dynamic chunking (H-Net).
3. **Semantic (content) addressing, not positional.** The KV store *is* a vector
   DB the model was trained to query; position is metadata. Removes "context
   length" as a concept — just a memory of arbitrary size with sublinear
   retrieval.
   → Memorizing Transformer; Unlimiformer; InfLLM.
4. **Working memory ≠ long-term store, trained jointly.** Small fast attention
   over a few-K working set + a **learned controller** that promotes to / recalls
   from a large slow store (on disk). The controller — trained end-to-end to
   manage its own memory — is the novel part.
   → MemGPT-as-architecture; the direction Titans gestures at.
5. **Cache understanding, not KV.** Once a document is "understood", store the
   compressed *state*, not raw KV; reload it instantly next time. Context becomes
   **composable**: state(A) + state(B) + query.
   → prompt-cache / cache-augmented generation, made first-class + compositional.
6. **Verbatim only where it matters — learned.** The model learns which spans
   need exact retention (code, numbers, names, quotes) vs. lossy (prose). Adaptive
   per-span bit budget, learned during training.

## Recommended v1 architecture (the concrete pick)

Smallest thing that exercises the core idea and is CPU-trainable:

```
  ┌─ sliding-window self-attention over the last W tokens (W ≈ 512–1024)   ── hot, verbatim, O(W)
  ├─ a learned gated memory module (Titans/RMT-style):                     ── the "gist of everything"
  │     M_t = update(M_{t-1}, segment_summary_t, surprise_t)               ── fixed size, O(1)
  │     read: q attends into M_t
  └─ kNN retrieval over an on-disk exact-KV store (lever 3):               ── precise recall, O(k log n)
        top-k key blocks fetched per query; nothing discarded
```

- **Levers used:** 1 (gated memory), 3 (retrieval), 4 (window + store split).
- **Deferred to v2+:** 2 (multi-resolution), 5 (composable state), 6 (learned
  verbatim budget).
- **Why this combo:** the window gives exact recent recall cheaply; the gated
  memory gives bounded-footprint "everything" for free; retrieval gives lossless
  deep recall when the memory's gist isn't enough — and its I/O is *tiny* (top-k
  small blocks, MB not GB), so it works even on the Kingston A400
  ([`ai-large.md`](ai-large.md)).
- **Efficiency + performance both:** resident cost ≈ O(W) + |M| + k·block, flat
  in total context; recall should beat H2O/SnapKV-style eviction because the
  memory is learned and the exact store loses nothing.

## The experiment (runs on `ml/`)

- `ml/train/model_mem.py` — the architecture above, config-driven, sharing the
  tokenizer + training loop with the vanilla GPT.
- Train both (vanilla vs. mem) on the same corpus, same params budget, same CPU
  time.
- **Eval:** (a) val perplexity; (b) a synthetic **needle-in-a-haystack** at
  1K / 8K / 64K / 512K "logical" context — can the mem model recall a fact placed
  arbitrarily deep with a ≤ 32K resident footprint?; (c) tokens/sec and peak RSS
  vs. vanilla at matched context.
- Ship the winner's inference path into `thos-lm` as an alternate model kind in
  the `.tlm` format.

## Honest framing

- Multi-year research bet with real failure risk — the architecture has to
  actually work, and small-model results may not transfer to scale.
- But the **entry is cheap**: one extra model file in `ml/train/`, trained on the
  same 10 MB corpus on the same CPU, measured against a baseline. A real
  experiment, not a manifesto.
- If v1 doesn't beat the eviction baseline, the fallback is the stack in
  [`ai-large.md`](ai-large.md) §"Long context on a small KV budget" — quantised
  KV + eviction + retrieval, no new architecture.

See [`ai.md`](ai.md) · [`ai-large.md`](ai-large.md) · [`roadmap.md`](roadmap.md).
