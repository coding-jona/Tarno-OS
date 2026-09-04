# SPDX-License-Identifier: GPL-2.0-or-later
"""From-scratch decoder-only transformer (PyTorch).

Deliberately plain so it maps 1:1 onto `tlm.py:numpy_forward` and the Rust
`thos-lm` crate: learned positional embeddings, pre-norm LayerNorm (weight+bias),
a single fused QKV Linear, tanh-approximation GELU, tied output projection.
"""

from __future__ import annotations

from dataclasses import dataclass

import torch
import torch.nn as nn
import torch.nn.functional as F


@dataclass
class ModelConfig:
    n_layer: int
    n_head: int
    n_embd: int
    block_size: int
    vocab_size: int
    dropout: float = 0.0
    norm_eps: float = 1e-5


class Block(nn.Module):
    def __init__(self, c: ModelConfig):
        super().__init__()
        self.ln1 = nn.LayerNorm(c.n_embd, eps=c.norm_eps)
        self.qkv = nn.Linear(c.n_embd, 3 * c.n_embd)
        self.proj = nn.Linear(c.n_embd, c.n_embd)
        self.ln2 = nn.LayerNorm(c.n_embd, eps=c.norm_eps)
        self.fc = nn.Linear(c.n_embd, 4 * c.n_embd)
        self.mlpproj = nn.Linear(4 * c.n_embd, c.n_embd)
        self.drop = nn.Dropout(c.dropout)
        self.n_head = c.n_head

    def forward(self, x):
        b, t, c = x.shape
        h = self.ln1(x)
        q, k, v = self.qkv(h).split(c, dim=2)
        hd = c // self.n_head
        q = q.view(b, t, self.n_head, hd).transpose(1, 2)
        k = k.view(b, t, self.n_head, hd).transpose(1, 2)
        v = v.view(b, t, self.n_head, hd).transpose(1, 2)
        att = (q @ k.transpose(-2, -1)) * (hd ** -0.5)
        mask = torch.tril(torch.ones(t, t, device=x.device, dtype=torch.bool))
        att = att.masked_fill(~mask, float("-inf"))
        att = F.softmax(att, dim=-1)
        y = (att @ v).transpose(1, 2).reshape(b, t, c)
        x = x + self.drop(self.proj(y))
        h2 = self.ln2(x)
        f = F.gelu(self.fc(h2), approximate="tanh")
        x = x + self.drop(self.mlpproj(f))
        return x


class GPT(nn.Module):
    def __init__(self, c: ModelConfig):
        super().__init__()
        self.cfg = c
        self.wte = nn.Embedding(c.vocab_size, c.n_embd)
        self.wpe = nn.Embedding(c.block_size, c.n_embd)
        self.drop = nn.Dropout(c.dropout)
        self.blocks = nn.ModuleList(Block(c) for _ in range(c.n_layer))
        self.lnf = nn.LayerNorm(c.n_embd, eps=c.norm_eps)
        self.apply(self._init)

    def _init(self, m):
        if isinstance(m, nn.Linear):
            nn.init.normal_(m.weight, mean=0.0, std=0.02)
            if m.bias is not None:
                nn.init.zeros_(m.bias)
        elif isinstance(m, nn.Embedding):
            nn.init.normal_(m.weight, mean=0.0, std=0.02)

    def forward(self, idx, targets=None):
        b, t = idx.shape
        assert t <= self.cfg.block_size
        pos = torch.arange(t, device=idx.device)
        x = self.drop(self.wte(idx) + self.wpe(pos))
        for blk in self.blocks:
            x = blk(x)
        x = self.lnf(x)
        logits = x @ self.wte.weight.T          # tied output
        loss = None
        if targets is not None:
            loss = F.cross_entropy(logits.view(-1, logits.size(-1)), targets.view(-1))
        return logits, loss

    @torch.no_grad()
    def export_tensors(self) -> dict[str, "torch.Tensor"]:
        """Map to the `.tlm` TENSOR ORDER key names (see tlm.py)."""
        out = {"wte": self.wte.weight, "wpe": self.wpe.weight}
        for i, blk in enumerate(self.blocks):
            p = f"h{i}."
            out[p + "ln1_w"] = blk.ln1.weight
            out[p + "ln1_b"] = blk.ln1.bias
            out[p + "qkv_w"] = blk.qkv.weight
            out[p + "qkv_b"] = blk.qkv.bias
            out[p + "proj_w"] = blk.proj.weight
            out[p + "proj_b"] = blk.proj.bias
            out[p + "ln2_w"] = blk.ln2.weight
            out[p + "ln2_b"] = blk.ln2.bias
            out[p + "fc_w"] = blk.fc.weight
            out[p + "fc_b"] = blk.fc.bias
            out[p + "mlpproj_w"] = blk.mlpproj.weight
            out[p + "mlpproj_b"] = blk.mlpproj.bias
        out["lnf_w"] = self.lnf.weight
        out["lnf_b"] = self.lnf.bias
        return {k: v.detach().float().cpu().numpy() for k, v in out.items()}

    def num_params(self) -> int:
        # tied output shares wte, so count parameters once.
        return sum(p.numel() for p in self.parameters())
