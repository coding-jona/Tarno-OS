// SPDX-License-Identifier: GPL-2.0-or-later
//! Proof-of-Life demo: load a `.tlm` and sample text. Host-only.
//!
//!   cargo run -p thos-lm --example generate --target x86_64-unknown-linux-gnu -- \
//!       --weights spike-1m.tlm --prompt "The " --max-tokens 200 --temp 0.8 --seed 1
//!
//! `--stop <text>` (repeatable) stops generation as soon as the decoded
//! output ends with that text — e.g. `--stop "<|im_end|>"` for an SFT/ChatML
//! model, so a chat reply doesn't run on to --max-tokens every single time.
//! The no_std `thos_lm::Sampler::generate` stays a plain one-shot fill (it
//! has no string/UTF-8 machinery, deliberately — this is host-only glue
//! around it, decoding incrementally to check stops, same approach
//! `thos-shell`'s own generate() loop uses).
#![cfg(not(target_os = "none"))]

use std::io::Read;

use thos_lm::{Model, Sampler, SamplerConfig};

fn main() {
    let mut weights = String::new();
    let mut prompt = String::from("The ");
    let mut cfg = SamplerConfig::default();
    let mut seed: u64 = 1;
    let mut stops: Vec<String> = Vec::new();
    let mut min_tokens: usize = 0;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--weights" => weights = args.next().expect("--weights <path>"),
            "--prompt" => prompt = args.next().expect("--prompt <str>"),
            "--max-tokens" => cfg.max_tokens = args.next().unwrap().parse().unwrap(),
            "--temp" => cfg.temperature = args.next().unwrap().parse().unwrap(),
            "--top-k" => cfg.top_k = args.next().unwrap().parse().unwrap(),
            "--seed" => seed = args.next().unwrap().parse().unwrap(),
            "--stop" => stops.push(args.next().expect("--stop <text>")),
            "--min-tokens" => min_tokens = args.next().unwrap().parse().unwrap(),
            other => panic!("unknown arg {other}"),
        }
    }
    assert!(!weights.is_empty(), "pass --weights <path.tlm>");

    let mut buf = Vec::new();
    std::fs::File::open(&weights)
        .expect("open weights")
        .read_to_end(&mut buf)
        .expect("read weights");
    let model = Model::load(&buf).expect("parse .tlm");
    eprintln!(
        "loaded {weights}: L={} H={} C={} T={} V={}",
        model.cfg.n_layer, model.cfg.n_head, model.cfg.n_embd, model.cfg.block_size,
        model.cfg.vocab_size
    );

    let mut toks = model.encode(prompt.as_bytes());

    if stops.is_empty() {
        // No stop sequences requested — the plain, unmodified one-shot path.
        Sampler::new(cfg, seed).generate(&model, &mut toks);
    } else {
        // Decode incrementally so a stop sequence can end generation before
        // --max-tokens, same idea as thos-shell's own generate() loop.
        let max_tokens = cfg.max_tokens;
        let mut sampler = Sampler::new(cfg, seed);
        let bs = model.cfg.block_size;
        let mut produced = 0usize;
        let mut pending: Vec<u8> = Vec::new(); // bytes not yet on a UTF-8 boundary
        let mut text_so_far = String::new();

        for _ in 0..max_tokens {
            let start = toks.len().saturating_sub(bs);
            let mut logits = model.forward(&toks[start..]);
            let next = sampler.pick(&mut logits);
            toks.push(next);
            produced += 1;

            pending.extend_from_slice(&model.decode(&[next]));
            let good = match std::str::from_utf8(&pending) {
                Ok(s) => s.len(),
                Err(e) => e.valid_up_to(),
            };
            if good > 0 {
                text_so_far.push_str(&String::from_utf8_lossy(&pending[..good]));
                pending.drain(..good);
            }

            if produced >= min_tokens
                && stops.iter().any(|s| !s.is_empty() && text_so_far.ends_with(s.as_str()))
            {
                break;
            }
        }
    }

    let text = model.decode(&toks);
    println!("{}", String::from_utf8_lossy(&text));
}
