// SPDX-License-Identifier: GPL-2.0-or-later
//! Cross-language correctness gate: the Rust forward pass must match the numpy
//! reference (`ml/train/tlm.py`) captured in `tests/fixtures/toy.golden.txt`,
//! and the sampler must be deterministic. Host-only.
#![cfg(not(target_os = "none"))]

use thos_lm::{Model, Sampler, SamplerConfig};

const TLM: &[u8] = include_bytes!("fixtures/toy.tlm");
const GOLDEN: &str = include_str!("fixtures/toy.golden.txt");

struct Golden {
    prompt: Vec<u16>,
    argmax: usize,
    logits: Vec<f32>,
}

fn parse_golden() -> Golden {
    let mut prompt = Vec::new();
    let mut argmax = 0usize;
    let mut logits = Vec::new();
    for line in GOLDEN.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("prompt ") {
            prompt = rest.split_whitespace().map(|t| t.parse().unwrap()).collect();
        } else if let Some(rest) = line.strip_prefix("argmax ") {
            argmax = rest.parse().unwrap();
        } else if line.starts_with("vocab ") {
            // informational
        } else {
            logits.push(line.parse().unwrap());
        }
    }
    Golden { prompt, argmax, logits }
}

#[test]
fn forward_matches_numpy_reference() {
    let g = parse_golden();
    let model = Model::load(TLM).expect("parse toy.tlm");
    let logits = model.forward(&g.prompt);
    assert_eq!(logits.len(), g.logits.len(), "vocab size");

    let mut max_abs = 0.0f32;
    for (i, (&got, &want)) in logits.iter().zip(&g.logits).enumerate() {
        let d = (got - want).abs();
        if d > max_abs {
            max_abs = d;
        }
        assert!(
            d <= 1e-4 * want.abs().max(1.0),
            "logit {i}: rust={got} ref={want} |d|={d}"
        );
    }
    eprintln!("max |rust - numpy| over {} logits = {max_abs:e}", logits.len());

    // argmax (ties -> lowest index) must agree with the reference.
    let mut best = 0usize;
    for i in 1..logits.len() {
        if logits[i] > logits[best] {
            best = i;
        }
    }
    assert_eq!(best, g.argmax, "greedy next token");
    assert_eq!(model.greedy(&g.prompt) as usize, g.argmax);
}

#[test]
fn sampler_is_deterministic() {
    let model = Model::load(TLM).expect("parse toy.tlm");
    let g = parse_golden();
    let cfg = SamplerConfig { temperature: 0.8, top_k: 40, max_tokens: 24 };

    let run = || {
        let mut toks = g.prompt.clone();
        Sampler::new(cfg, 12345).generate(&model, &mut toks);
        toks
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "same seed -> same sequence");
    assert_eq!(a.len(), g.prompt.len() + cfg.max_tokens);
}

const BPE_TLM: &[u8] = include_bytes!("fixtures/bpe.tlm");
const BPE_GOLDEN: &str = include_str!("fixtures/bpe.golden.txt");

fn hex_to_bytes(h: &str) -> Vec<u8> {
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn bpe_tokenizer_matches_python() {
    let model = Model::load(BPE_TLM).expect("parse bpe.tlm");
    for line in BPE_GOLDEN.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("case ") else { continue };
        let (hex, ids) = rest.split_once(" => ").expect("case syntax");
        let input = hex_to_bytes(hex);
        let want: Vec<u16> = ids.split_whitespace().map(|t| t.parse().unwrap()).collect();
        let got = model.encode(&input);
        assert_eq!(got, want, "encode {input:?}");
        assert_eq!(model.decode(&got), input, "decode·encode round-trip {input:?}");
    }
}
