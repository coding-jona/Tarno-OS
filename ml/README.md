# THOS — in-system AI (`ml/`)

A from-scratch small **language model** for THOS. Train off-device in PyTorch on
the dev box; ship a hand-written `#![no_std]` Rust inference engine with frozen
weights. Grow from small to large step by step. Design + rationale:
[`docs/thos/ai.md`](../docs/thos/ai.md).

**Status:** P0 "Proof of Life". The pipeline (fetch → tokenize → train → export →
Rust inference) is wired end to end and cross-checked; no usable model is trained
yet.

## Layout

| Path | What |
|---|---|
| `thos-lm/` | Rust crate (workspace member), `#![no_std]` + `alloc` + `libm`. Parses `.tlm` weights, runs the forward pass, samples. The only part that ever ships in THOS. |
| `train/` | Python. Resumable corpus downloader, byte tokenizer, from-scratch GPT, CPU training loop, `.tlm` exporter, numpy reference forward + sampler. Never runs in THOS. |
| `DATASETS.md` | Every corpus, its licence, and the obligations it carries. |

Weight blobs, downloaded corpora and checkpoints are **git-ignored**; only the
code + the resumable recipe are tracked. The one committed blob is the < 64 KB
`thos-lm/tests/fixtures/toy.tlm` golden-test fixture.

## Constraints this repo is built around

- **CPU-only training** on an i7-13700KF (24 threads, AVX2). No usable GPU, no
  cloud. → small models, byte-level tokens, patience.
- **Internet drops at 00:00** (02:00 Fri→Sat and Sat→Sun). `train/fetch.py` is
  fully resumable; the v0 corpus is kept small enough for one window.
- **THOS runtime limits** (kernel has no FPU-state save across a preemption;
  512 MiB RAM; userland can't write files; no kernel↔userspace channel). Running
  the model *inside* THOS is a later phase gated on kernel work — see
  `docs/thos/ai.md` phases P3–P4.

## End-to-end (P0)

One orchestrator, `ml/train/run.sh`, drives every step. All steps are idempotent
and resumable — safe to re-run after the nightly internet cut-off, a killed
training run, or a reboot.

```sh
ml/train/run.sh all                 # setup -> data -> train -> export -> sample
```

or step by step:

```sh
ml/train/run.sh setup               # .venv + torch(CPU) + numpy + tqdm
ml/train/run.sh data                # fetch corpus (resumable) + tokenize/pack
ml/train/run.sh train-bg            # train on CPU in the background
ml/train/run.sh status              # progress (loss, step, tok/s)
ml/train/run.sh export              # out/latest.pt -> spike-1m.tlm
ml/train/run.sh sample "The "       # build the Rust engine + generate
```

`run.sh help` lists everything. Overridable via env: `CONFIG=` (default
`config/spike-1m.toml`), `TLM=`, `PROMPT=`, `MAXTOK=`.

The `train/*.py` scripts still run standalone if you prefer; `run.sh` just wires
them together with the right paths and resume flags.

## Tests

```sh
ml/train/run.sh test    # no_std build + golden cross-check + regenerate fixture
```

