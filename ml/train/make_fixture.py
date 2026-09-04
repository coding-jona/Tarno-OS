# SPDX-License-Identifier: GPL-2.0-or-later
"""Generate the committed golden-test fixture for `thos-lm`.

Writes a tiny random-weight `.tlm` and the numpy reference output for a fixed
prompt, so the Rust golden test can prove its forward pass + argmax match the
reference without needing PyTorch or a trained model.

    python ml/train/make_fixture.py

Outputs (committed, < 64 KB total):
    ml/thos-lm/tests/fixtures/toy.tlm          byte tokenizer, forward golden
    ml/thos-lm/tests/fixtures/toy.golden.txt
    ml/thos-lm/tests/fixtures/bpe.tlm          BPE tokenizer, encode golden
    ml/thos-lm/tests/fixtures/bpe.golden.txt
"""

from __future__ import annotations

import os

import numpy as np

from bpe import Tokenizer, train_bpe
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

    # --- BPE tokenizer fixture: a tiny deterministic merge set + golden
    #     encodings, so the Rust Tokenizer stays byte-identical to bpe.py. ---
    bpe_corpus = (
        b"the quick brown fox jumps over the lazy dog. "
        b"the dog sleeps. the fox runs. quick quick brown brown.\n"
    ) * 40
    merges = train_bpe(bpe_corpus, 320, verbose=False)
    tok = Tokenizer(merges)
    bcfg = Config(n_layer=2, n_head=2, n_embd=16, block_size=32,
                  vocab_size=tok.vocab_size, norm_eps=1e-5,
                  tokenizer_kind=1, merges=merges)
    bpe_tlm = os.path.join(FIX, "bpe.tlm")
    write_tlm(bpe_tlm, bcfg, random_tensors(bcfg, SEED))
    bsize = os.path.getsize(bpe_tlm)
    assert bsize < 64 * 1024, f"bpe.tlm too big: {bsize} bytes"

    cases = [b"the quick brown fox", b"\nThe DOG.", b"  x  yy\tz", b"quickbrown"]
    bpe_golden = os.path.join(FIX, "bpe.golden.txt")
    with open(bpe_golden, "w") as fh:
        fh.write("# thos-lm BPE golden — regenerate with ml/train/make_fixture.py\n")
        fh.write(f"vocab {tok.vocab_size}\n")
        for s in cases:
            ids = tok.encode(s)
            assert tok.decode(ids) == s, s
            fh.write(f"case {s.hex()} => {' '.join(map(str, ids))}\n")
    print(f"wrote {bpe_tlm} ({bsize} bytes, vocab {tok.vocab_size}, {len(merges)} merges)")


if __name__ == "__main__":
    main()
