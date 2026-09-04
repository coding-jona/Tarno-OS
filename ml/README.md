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

```sh
# 0. one-time deps (torch only needed for steps 2–3)
python -m pip install -r ml/train/requirements.txt

# 1. corpus (in an internet window; safe to Ctrl-C and rerun)
python ml/train/fetch.py
python ml/train/prepare.py

# 2. train the ~1M spike on CPU (hours; --resume to continue)
python ml/train/train.py --config config/spike-1m.toml
python ml/train/export.py --ckpt out/latest.pt --out spike-1m.tlm

# 3. run it — Rust engine, no Python
cargo run -p thos-lm --example generate --target x86_64-unknown-linux-gnu -- \
    --weights spike-1m.tlm --prompt "The " --max-tokens 200

# cross-check the Rust engine against the numpy oracle
python ml/train/ref_forward.py --weights spike-1m.tlm --prompt "The " --dump > ref.txt
```

## Tests

```sh
cargo build -p thos-lm --target x86_64-unknown-none          # proves it's no_std
cargo test  -p thos-lm --target x86_64-unknown-linux-gnu     # golden cross-check
python ml/train/make_fixture.py                              # regenerate the fixture
```
