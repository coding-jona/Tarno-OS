// SPDX-License-Identifier: GPL-2.0-or-later
//! Proof-of-Life demo: load a `.tlm` and sample text. Host-only.
//!
//!   cargo run -p thos-lm --example generate --target x86_64-unknown-linux-gnu -- \
//!       --weights spike-1m.tlm --prompt "The " --max-tokens 200 --temp 0.8 --seed 1
#![cfg(not(target_os = "none"))]

use std::io::Read;

use thos_lm::{decode_bytes, encode_bytes, Model, Sampler, SamplerConfig};

fn main() {
    let mut weights = String::new();
    let mut prompt = String::from("The ");
    let mut cfg = SamplerConfig::default();
    let mut seed: u64 = 1;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--weights" => weights = args.next().expect("--weights <path>"),
            "--prompt" => prompt = args.next().expect("--prompt <str>"),
            "--max-tokens" => cfg.max_tokens = args.next().unwrap().parse().unwrap(),
            "--temp" => cfg.temperature = args.next().unwrap().parse().unwrap(),
            "--top-k" => cfg.top_k = args.next().unwrap().parse().unwrap(),
            "--seed" => seed = args.next().unwrap().parse().unwrap(),
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

    let mut toks = Vec::new();
    encode_bytes(prompt.as_bytes(), &mut toks);
    Sampler::new(cfg, seed).generate(&model, &mut toks);

    let text = decode_bytes(&toks);
    println!("{}", String::from_utf8_lossy(&text));
}
