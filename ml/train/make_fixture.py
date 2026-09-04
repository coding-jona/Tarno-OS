# SPDX-License-Identifier: GPL-2.0-or-later
"""Generate the committed golden-test fixture for `thos-lm`.

Writes a tiny random-weight `.tlm` and the numpy reference output for a fixed
prompt, so the Rust golden test can prove its forward pass + argmax match the
reference without needing PyTorch or a trained model.

    python ml/train/make_fixture.py

Outputs (committed, < 64 KB total):
    ml/thos-lm/tests/fixtures/toy.tlm
    ml/thos-lm/tests/fixtures/toy.golden.txt
"""

from __future__ import annotations

import os

import numpy as np

from tlm import Config, numpy_forward, write_tlm

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.normpath(os.path.join(HERE, "..", "thos-lm", "tests", "fixtures"))

# Fixed toy config — small enough that toy.tlm is well under 64 KB.
CFG = Config(n_layer=2, n_head=2, n_embd=16, block_size=32, vocab_size=256, norm_eps=1e-5)
PROMPT = b"THOS"           # 4 byte tokens
SEED = 20260904


def random_tensors(cfg: Config, seed: int) -> dict[str, np.ndarray]:
    rng = np.random.default_rng(seed)
    out: dict[str, np.ndarray] = {}
    for key, shp in cfg.shapes().items():
        if key.endswith("_b") or key in ("lnf_b",):
            out[key] = np.zeros(shp, dtype=np.float32)
        elif key.endswith("ln1_w") or key.endswith("ln2_w") or key == "lnf_w":
            out[key] = np.ones(shp, dtype=np.float32)
        else:
            out[key] = (rng.standard_normal(shp) * 0.02).astype(np.float32)
    return out


def main() -> None:
    os.makedirs(FIX, exist_ok=True)
    tensors = random_tensors(CFG, SEED)
    tlm_path = os.path.join(FIX, "toy.tlm")
    write_tlm(tlm_path, CFG, tensors)
    size = os.path.getsize(tlm_path)
    assert size < 64 * 1024, f"toy.tlm too big: {size} bytes"

    tokens = list(PROMPT)
    logits = numpy_forward(CFG, tensors, tokens)
    argmax = int(np.argmax(logits))  # first-max on ties, matches Rust

    golden = os.path.join(FIX, "toy.golden.txt")
    with open(golden, "w") as fh:
        fh.write("# thos-lm golden fixture — regenerate with ml/train/make_fixture.py\n")
        fh.write(f"prompt {' '.join(str(b) for b in PROMPT)}\n")
        fh.write(f"argmax {argmax}\n")
        fh.write(f"vocab {CFG.vocab_size}\n")
        for x in logits:
            fh.write(f"{x:.8e}\n")

    print(f"wrote {tlm_path} ({size} bytes)")
    print(f"wrote {golden} (argmax={argmax}, top logit={logits[argmax]:.5f})")


if __name__ == "__main__":
    main()
