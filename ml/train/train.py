# SPDX-License-Identifier: GPL-2.0-or-later
"""CPU training loop for the spike model.

    python ml/train/train.py --config config/spike-1m.toml [--resume]

Checkpoints to out/ every ckpt_interval steps; --resume picks up the latest.
Training needs no internet, so it can run through the nightly cut-off.
"""

from __future__ import annotations

import argparse
import math
import os
import time

try:
    import tomllib
except ModuleNotFoundError:  # py < 3.11
    import tomli as tomllib

import numpy as np
import torch

from model import GPT, ModelConfig

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "out")


def load_cfg(path: str) -> dict:
    with open(path, "rb") as fh:
        return tomllib.load(fh)


def get_batch(data: np.ndarray, block: int, bs: int, rng: np.random.Generator):
    ix = rng.integers(0, len(data) - block - 1, size=bs)
    x = np.stack([data[i : i + block] for i in ix]).astype(np.int64)
    y = np.stack([data[i + 1 : i + 1 + block] for i in ix]).astype(np.int64)
    return torch.from_numpy(x), torch.from_numpy(y)


def lr_at(step: int, tc: dict) -> float:
    if step < tc["warmup_steps"]:
        return tc["lr"] * (step + 1) / tc["warmup_steps"]
    if step >= tc["max_steps"]:
        return tc["min_lr"]
    prog = (step - tc["warmup_steps"]) / (tc["max_steps"] - tc["warmup_steps"])
    return tc["min_lr"] + 0.5 * (1 + math.cos(math.pi * prog)) * (tc["lr"] - tc["min_lr"])


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default=os.path.join(HERE, "config", "spike-1m.toml"))
    ap.add_argument("--resume", action="store_true")
    args = ap.parse_args()

    cfg = load_cfg(args.config)
    mc, tc, dc = cfg["model"], cfg["train"], cfg["data"]
    os.makedirs(OUT, exist_ok=True)

    if tc.get("num_threads", 0):
        torch.set_num_threads(int(tc["num_threads"]))
    torch.manual_seed(tc["seed"])
    rng = np.random.default_rng(tc["seed"])

    train_data = np.fromfile(os.path.join(HERE, dc["train_bin"]), dtype=np.uint16)
    val_data = np.fromfile(os.path.join(HERE, dc["val_bin"]), dtype=np.uint16)
    print(f"data: train {len(train_data):,} / val {len(val_data):,} tokens, {torch.get_num_threads()} threads")

    model = GPT(ModelConfig(
        n_layer=mc["n_layer"], n_head=mc["n_head"], n_embd=mc["n_embd"],
        block_size=mc["block_size"], vocab_size=mc["vocab_size"],
        dropout=mc.get("dropout", 0.0), norm_eps=mc.get("norm_eps", 1e-5),
    ))
    print(f"model: {model.num_params()/1e6:.2f}M params")
    opt = torch.optim.AdamW(model.parameters(), lr=tc["lr"],
                            weight_decay=tc["weight_decay"], betas=(0.9, 0.95))

    step0 = 0
    ckpt = os.path.join(OUT, "latest.pt")
    if args.resume and os.path.exists(ckpt):
        blob = torch.load(ckpt, map_location="cpu")
        if blob.get("cfg", {}).get("model") != mc:
            print(f"ignoring {ckpt}: checkpoint config differs from {os.path.basename(args.config)} "
                  "— starting fresh")
        else:
            model.load_state_dict(blob["model"])
            opt.load_state_dict(blob["opt"])
            step0 = blob["step"]
            print(f"resumed from step {step0}")

    log_path = os.path.join(OUT, "log.csv")
    if step0 == 0:
        with open(log_path, "w") as fh:
            fh.write("step,train_loss,val_loss,lr,tok_per_s\n")

    block, bs, accum = mc["block_size"], tc["batch_size"], tc["grad_accum"]
    t_last = time.time()
    for step in range(step0, tc["max_steps"]):
        lr = lr_at(step, tc)
        for g in opt.param_groups:
            g["lr"] = lr

        model.train()
        opt.zero_grad(set_to_none=True)
        loss_acc = 0.0
        for _ in range(accum):
            x, y = get_batch(train_data, block, bs, rng)
            _, loss = model(x, y)
            (loss / accum).backward()
            loss_acc += loss.item() / accum
        torch.nn.utils.clip_grad_norm_(model.parameters(), tc["grad_clip"])
        opt.step()

        if (step + 1) % tc["eval_interval"] == 0 or step == 0:
            model.eval()
            with torch.no_grad():
                vl = np.mean([
                    model(*get_batch(val_data, block, bs, rng))[1].item()
                    for _ in range(tc["eval_batches"])
                ])
            now = time.time()
            tps = tc["eval_interval"] * accum * bs * block / (now - t_last) if step else 0
            t_last = now
            print(f"step {step+1:>6} | train {loss_acc:.4f} | val {vl:.4f} | lr {lr:.2e} | {tps:,.0f} tok/s")
            with open(log_path, "a") as fh:
                fh.write(f"{step+1},{loss_acc:.5f},{vl:.5f},{lr:.3e},{tps:.0f}\n")

        if (step + 1) % tc["ckpt_interval"] == 0:
            torch.save({"model": model.state_dict(), "opt": opt.state_dict(),
                        "step": step + 1, "cfg": cfg}, ckpt)

    torch.save({"model": model.state_dict(), "opt": opt.state_dict(),
                "step": tc["max_steps"], "cfg": cfg}, ckpt)
    print(f"done — checkpoint at {ckpt}; export with:  python ml/train/export.py --ckpt {ckpt} --out spike-1m.tlm")


if __name__ == "__main__":
    main()
