# THOS — in-system AI (plan, not yet built)

Status: **planning only.** No code. This is a near-separate subproject; it gets a
plan before a line is written so scope doesn't leak into the kernel work.

## Hard constraints (set by the project owner)

- **No external dependency at runtime.** No API keys, no network calls, no
  "talk to a server". THOS must do this offline, forever.
- **No running third-party model.** Not llama.cpp, not an ONNX runtime, not a
  bundled GGUF. The inference code is *ours*, written from scratch.
- **Own logic, from zero.** Hand-rolled math, weights we can read and explain.
- **Deterministic and verifiable** wherever it touches a security decision.

These rule out an LLM-in-the-kernel. What's left is small classical ML, which is
a good fit for the jobs THOS actually has.

## Two tiers

| Tier | Where | What | Size |
|---|---|---|---|
| **1 — in-kernel** | `#![no_std]` crate `thos-ml`, pure `fn infer(&Features) -> Scores` | tiny deterministic model (MLP / decision tree / logistic regression / naïve Bayes / n-gram). Frozen weights baked in via `include_bytes!`. **Inference only.** | KiB–low-MiB of weights, microseconds per call |
| **2 — post-boot userspace** | a normal THOS process started by init after the system is up | the heavier analysis that can't or shouldn't run in ring 0. Talks to the kernel over a syscall / device interface. Can be larger, can use `alloc`, can be restarted, can be updated independently. | bounded by policy, not by the kernel |

Tier 1 is the gate that must never block boot and must always answer fast. Tier 2
is where anything ambitious lives, sandboxed like any other process.

## First use case (decides the feature pipeline)

Leading candidate: the **exec-gate / AV scorer** from
`memory: thos-security-architecture` — score a PE/ELF as benign / suspicious from
*static* features (import set, section layout, entropy, header anomalies,
overlay, TLS-callback abuse, entry-point section) before it runs. It's already on
the roadmap, it's a classification problem (small models do this well), and a
wrong answer degrades to "ask the admin", not "crash".

Alternatives considered: syscall-sequence anomaly detection (Tier 2, n-gram /
Markov), compat-layer heuristics for the zero-config goal, scheduler/IO
prediction (low payoff). Not decided yet.

## Training (off-device)

- Training harness is a **separate `std` Rust (or Python) tool** in the repo,
  run on the dev box. Never in the kernel.
- Output is a frozen weight blob + a versioned feature-schema. Committed as data.
- Kernel does the forward pass only. Optional bounded on-device adaptation is a
  later, explicit decision — not the default.
- **Dataset provenance is a clean-room concern.** Malware corpora, EMBER-style
  feature sets, labels — each source's licence and redistribution terms get
  recorded next to the blob, same discipline as `THIRD_PARTY/`. No scraping of
  proprietary AV verdicts as ground truth.

## Open decisions

1. First use case (exec-gate scorer vs. something else).
2. Model class (MLP vs. tree vs. linear vs. sequence model).
3. Feature schema for the chosen job.
4. Fixed-point vs. `f32` in the kernel (f32 is simpler; SSE is available; no
   kernel FPU-save story yet for ring-0 SIMD — check).
5. Where Tier 2 gets its inputs (a `\Device\ThosML` interface? a syscall?).
6. Whether Tier 1 ever adapts on-device.

## Phased plan

- **P0 — spike (1–2 wk).** `thos-ml` crate skeleton; a linear model with
  hand-picked weights; wire one call into the exec-gate behind a feature flag;
  prove the no_std forward pass + `include_bytes!` weight load works and is fast.
- **P1 — real features.** Static PE/ELF feature extractor in the kernel (reuses
  the loader's parsing). Feature-schema v1. Off-device training harness lands.
- **P2 — trained Tier 1.** Train the chosen small model off-device, freeze,
  ship. Exec-gate consults it; verdicts logged, not yet enforced.
- **P3 — enforcement + Tier 2.** Turn on default-deny for high scores with
  admin override (matches the age-gate / security-architecture model). Stand up
  the post-boot userspace service for the heavier pass.
- **P4+ — more jobs.** Syscall-anomaly (Tier 2), compat heuristics, etc., each
  with its own schema + model.

## Non-goals

Chatbot, natural-language shell, code generation, anything that wants billions of
parameters or a GPU. If it needs that, it's not this subproject.
