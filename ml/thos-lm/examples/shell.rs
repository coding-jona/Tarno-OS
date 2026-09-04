// SPDX-License-Identifier: GPL-2.0-or-later
//! `thos-shell` — an interactive local REPL for a THOS `.tlm` language model.
//!
//! It is deliberately built to feel like a small standalone product: a banner
//! with the loaded model's vitals, a coloured prompt, slash-commands, live
//! token-by-token streaming, and a rolling context so turns build on each other.
//! Zero extra dependencies — line editing is whatever the terminal's cooked
//! mode provides.
//!
//!   cargo run -q -p thos-lm --example shell --target x86_64-unknown-linux-gnu -- \
//!       --weights spike-1m.tlm
//!
//! Slash-commands (type `/help` inside): /help /params /temp /topk /tokens
//! /seed /stops /reset /multi /save /exit
#![cfg(not(target_os = "none"))]

use std::io::{self, BufRead, Write};
use std::time::Instant;

use thos_lm::{Model, Sampler, SamplerConfig};

const C_RESET: &str = "\x1b[0m";
const C_DIM: &str = "\x1b[2m";
const C_BOLD: &str = "\x1b[1m";
const C_CYAN: &str = "\x1b[36m";
const C_GREEN: &str = "\x1b[32m";
const C_YELLOW: &str = "\x1b[33m";
const C_MAGENTA: &str = "\x1b[35m";

struct Settings {
    temperature: f32,
    top_k: usize,
    max_tokens: usize,
    seed: u64,
    /// Stop generation early when the running output ends with one of these.
    stops: Vec<String>,
    /// Keep at most this many bytes of rolling context fed back into the model.
    ctx_bytes: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            temperature: 0.9,
            top_k: 40,
            max_tokens: 200,
            seed: rand_seed(),
            stops: vec!["\n\n".into()],
            ctx_bytes: 4096,
        }
    }
}

/// A cheap non-crypto seed so each session differs without pulling in `rand`.
fn rand_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    (d.as_nanos() as u64) ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn main() {
    let mut weights = String::new();
    let mut sys_prompt = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--weights" | "--model" | "-m" => weights = args.next().unwrap_or_default(),
            "--system" | "-s" => sys_prompt = args.next().unwrap_or_default(),
            "--help" | "-h" => {
                eprintln!("usage: thos-shell --weights <path.tlm> [--system <text>]");
                return;
            }
            other => {
                eprintln!("thos-shell: unknown argument {other:?}");
                std::process::exit(2);
            }
        }
    }
    if weights.is_empty() {
        weights = first_existing(&[
            "spike-1m.tlm",
            "small-30m.tlm",
            "../spike-1m.tlm",
            "../../spike-1m.tlm",
        ])
        .unwrap_or_else(|| {
            eprintln!("thos-shell: no --weights given and no *.tlm found nearby");
            std::process::exit(2);
        });
    }

    let bytes = std::fs::read(&weights).unwrap_or_else(|e| {
        eprintln!("thos-shell: cannot read {weights}: {e}");
        std::process::exit(1);
    });
    let mut model = Model::load(&bytes).unwrap_or_else(|e| {
        eprintln!("thos-shell: {weights} is not a valid .tlm ({e:?})");
        std::process::exit(1);
    });
    let mut model_size = bytes.len();
    let mut model_mtime = mtime(&weights);

    let mut set = Settings::default();
    banner(&weights, model_size, &model, &set);
    if model_mtime.is_some() {
        println!(
            "  {C_DIM}live: this file is checked before every turn — a checkpoint that\n  \
             'run.sh watch-export' refreshes mid-training is picked up automatically.{C_RESET}\n"
        );
    }

    // Rolling context: everything said so far, model output included.
    let mut ctx = String::new();
    if !sys_prompt.is_empty() {
        ctx.push_str(&sys_prompt);
        ctx.push_str("\n\n");
        println!("{C_DIM}(system prompt primed, {} chars){C_RESET}", sys_prompt.len());
    }

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        prompt_line(&set);
        let Some(Ok(mut line)) = lines.next() else {
            println!("\n{C_DIM}bye.{C_RESET}");
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(cmd) = trimmed.strip_prefix('/') {
            match handle_command(cmd, &mut set, &mut ctx, &weights, &model) {
                Cmd::Continue => {}
                Cmd::Quit => {
                    println!("{C_DIM}bye.{C_RESET}");
                    break;
                }
                Cmd::Reload => {
                    reload(&weights, &mut model, &mut model_mtime, &mut model_size, true);
                }
                Cmd::MultiLine => {
                    line = read_multiline(&mut lines);
                    if line.trim().is_empty() {
                        continue;
                    }
                    reload(&weights, &mut model, &mut model_mtime, &mut model_size, false);
                    run_turn(&model, &set, &mut ctx, &line);
                }
            }
            continue;
        }

        reload(&weights, &mut model, &mut model_mtime, &mut model_size, false);
        run_turn(&model, &set, &mut ctx, &line);
    }
}

fn mtime(path: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Re-read `path` if it changed on disk (or if `force`) — this is what makes
/// "chat with the model while it trains" work: `run.sh watch-export` rewrites
/// the .tlm on disk every time a fresh checkpoint lands, and the shell just
/// notices before the next turn.
fn reload(
    path: &str,
    model: &mut Model,
    last_mtime: &mut Option<std::time::SystemTime>,
    last_size: &mut usize,
    force: bool,
) {
    let current = mtime(path);
    if !force && current == *last_mtime {
        return;
    }
    match std::fs::read(path) {
        Ok(bytes) => match Model::load(&bytes) {
            Ok(m) => {
                *model = m;
                *last_size = bytes.len();
                *last_mtime = current;
                println!(
                    "{C_DIM}↻ reloaded {path} ({:.1} MB) — newer checkpoint on disk{C_RESET}",
                    *last_size as f64 / 1e6
                );
            }
            Err(e) if force => println!("{C_YELLOW}reload failed: not a valid .tlm ({e:?}){C_RESET}"),
            Err(_) => {} // mid-write of a partial file — keep the old model, try again next turn
        },
        Err(e) if force => println!("{C_YELLOW}reload failed: {e}{C_RESET}"),
        Err(_) => {}
    }
}

fn first_existing(cands: &[&str]) -> Option<String> {
    cands.iter().find(|p| std::path::Path::new(p).is_file()).map(|s| s.to_string())
}

fn banner(path: &str, size: usize, model: &Model, set: &Settings) {
    let c = &model.cfg;
    let params = approx_params(model);
    let kind = if c.vocab_size > 256 { "byte-BPE" } else { "raw-byte" };
    println!();
    println!("{C_BOLD}{C_MAGENTA}  ┌─────────────────────────────────────────────┐{C_RESET}");
    println!("{C_BOLD}{C_MAGENTA}  │{C_RESET}  {C_BOLD}THOS · local language model shell{C_RESET}          {C_BOLD}{C_MAGENTA}│{C_RESET}");
    println!("{C_BOLD}{C_MAGENTA}  └─────────────────────────────────────────────┘{C_RESET}");
    println!(
        "  {C_DIM}model  {C_RESET}{path}  {C_DIM}({:.1} MB on disk){C_RESET}",
        size as f64 / 1e6
    );
    println!(
        "  {C_DIM}arch   {C_RESET}{} layers · {} heads · d{} · ctx {} · vocab {} {C_DIM}({kind}){C_RESET}",
        c.n_layer, c.n_head, c.n_embd, c.block_size, c.vocab_size
    );
    println!("  {C_DIM}params {C_RESET}≈ {params}");
    println!(
        "  {C_DIM}sample {C_RESET}temp {:.2} · top-k {} · {} tok/turn · seed {}",
        set.temperature, set.top_k, set.max_tokens, set.seed
    );
    println!("  {C_DIM}type your text and press enter · /help for commands · Ctrl-D to quit{C_RESET}");
    println!();
}

fn approx_params(model: &Model) -> String {
    let total = model.cfg.total_elems(); // exact f32 count (tied output reuses wte)
    if total >= 1_000_000 {
        format!("{:.1}M", total as f64 / 1e6)
    } else {
        format!("{:.0}k", total as f64 / 1e3)
    }
}

fn prompt_line(_set: &Settings) {
    print!("{C_BOLD}{C_CYAN}thos ▸ {C_RESET}");
    io::stdout().flush().ok();
}

enum Cmd {
    Continue,
    Quit,
    MultiLine,
    Reload,
}

fn handle_command(
    cmd: &str,
    set: &mut Settings,
    ctx: &mut String,
    weights: &str,
    model: &Model,
) -> Cmd {
    let mut it = cmd.split_whitespace();
    let name = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("");
    match name {
        "help" | "?" => print_help(),
        "params" | "info" => {
            println!(
                "  {C_DIM}temp{C_RESET} {:.2}  {C_DIM}top-k{C_RESET} {}  {C_DIM}tokens{C_RESET} {}  \
                 {C_DIM}seed{C_RESET} {}  {C_DIM}ctx{C_RESET} {} B  {C_DIM}stops{C_RESET} {:?}",
                set.temperature, set.top_k, set.max_tokens, set.seed, set.ctx_bytes, set.stops
            );
            println!("  {C_DIM}context held{C_RESET} {} chars", ctx.len());
            println!("  {C_DIM}model{C_RESET} {weights}  {C_DIM}vocab{C_RESET} {}", model.cfg.vocab_size);
        }
        "temp" | "temperature" => set_f32(&mut set.temperature, arg, "temp", 0.0, 5.0),
        "topk" | "top-k" => set_usize(&mut set.top_k, arg, "top-k", 1, model.cfg.vocab_size),
        "tokens" | "max" => set_usize(&mut set.max_tokens, arg, "tokens", 1, 8192),
        "seed" => match arg.parse() {
            Ok(v) => {
                set.seed = v;
                println!("  {C_GREEN}seed = {v}{C_RESET}");
            }
            Err(_) => println!("  {C_YELLOW}usage: /seed <u64>{C_RESET}"),
        },
        "stops" => {
            if arg.is_empty() {
                println!("  {C_DIM}stops{C_RESET} {:?}  {C_DIM}(/stops none  |  /stops \"<text>\"){C_RESET}", set.stops);
            } else if arg == "none" {
                set.stops.clear();
                println!("  {C_GREEN}stop sequences cleared{C_RESET}");
            } else {
                let seq = cmd.trim_start_matches("stops").trim();
                let seq = seq.trim_matches('"').replace("\\n", "\n");
                set.stops = vec![seq];
                println!("  {C_GREEN}stop on {:?}{C_RESET}", set.stops);
            }
        }
        "reset" | "clear" => {
            ctx.clear();
            println!("  {C_GREEN}context cleared{C_RESET}");
        }
        "multi" => {
            println!("  {C_DIM}multi-line input — finish with a single line containing only '.'{C_RESET}");
            return Cmd::MultiLine;
        }
        "save" => {
            let path = if arg.is_empty() { "thos-shell.transcript.txt" } else { arg };
            match std::fs::write(path, ctx.as_bytes()) {
                Ok(_) => println!("  {C_GREEN}context ({} chars) -> {path}{C_RESET}", ctx.len()),
                Err(e) => println!("  {C_YELLOW}save failed: {e}{C_RESET}"),
            }
        }
        "reload" => return Cmd::Reload,
        "exit" | "quit" | "q" => return Cmd::Quit,
        other => println!("  {C_YELLOW}unknown command /{other} — try /help{C_RESET}"),
    }
    Cmd::Continue
}

fn set_f32(slot: &mut f32, arg: &str, label: &str, lo: f32, hi: f32) {
    match arg.parse::<f32>() {
        Ok(v) if v >= lo && v <= hi => {
            *slot = v;
            println!("  {C_GREEN}{label} = {v}{C_RESET}");
        }
        _ => println!("  {C_YELLOW}usage: /{label} <{lo}..{hi}>{C_RESET}"),
    }
}

fn set_usize(slot: &mut usize, arg: &str, label: &str, lo: usize, hi: usize) {
    match arg.parse::<usize>() {
        Ok(v) if v >= lo && v <= hi => {
            *slot = v;
            println!("  {C_GREEN}{label} = {v}{C_RESET}");
        }
        _ => println!("  {C_YELLOW}usage: /{label} <{lo}..{hi}>{C_RESET}"),
    }
}

fn print_help() {
    let rows = [
        ("<text>", "generate a continuation; the exchange is kept as context"),
        ("/multi", "enter a multi-line prompt (end with a lone '.')"),
        ("/temp <f>", "sampling temperature (0 = greedy/argmax)"),
        ("/topk <n>", "top-k cutoff"),
        ("/tokens <n>", "max new tokens per turn"),
        ("/seed <n>", "fix the RNG seed"),
        ("/stops \"..\"", "stop when output ends with this (\\n allowed); /stops none"),
        ("/params", "show current settings + context size"),
        ("/reset", "forget the rolling context"),
        ("/save [file]", "write the context transcript to a file"),
        ("/reload", "force-reload the weights file now (auto-checked every turn anyway)"),
        ("/exit", "quit (also Ctrl-D)"),
    ];
    println!("  {C_BOLD}commands{C_RESET}");
    for (k, v) in rows {
        println!("    {C_CYAN}{k:<14}{C_RESET} {C_DIM}{v}{C_RESET}");
    }
}

fn read_multiline<I: Iterator<Item = io::Result<String>>>(lines: &mut I) -> String {
    let mut buf = String::new();
    for l in lines.by_ref() {
        match l {
            Ok(s) if s.trim() == "." => break,
            Ok(s) => {
                buf.push_str(&s);
                buf.push('\n');
            }
            Err(_) => break,
        }
    }
    buf
}

/// Feed `user` + rolling context into the model and stream the continuation.
fn run_turn(model: &Model, set: &Settings, ctx: &mut String, user: &str) {
    if !ctx.is_empty() && !ctx.ends_with('\n') {
        ctx.push('\n');
    }
    ctx.push_str(user);
    if !ctx.ends_with('\n') {
        ctx.push('\n');
    }

    // Clamp the context we actually feed to the model, on a char boundary.
    let want = ctx.len().saturating_sub(set.ctx_bytes);
    let start = (want..ctx.len()).find(|&i| ctx.is_char_boundary(i)).unwrap_or(0);
    let fed = &ctx[start..];

    let mut toks = model.encode(fed.as_bytes());
    let cfg = SamplerConfig {
        temperature: set.temperature,
        top_k: set.top_k,
        max_tokens: set.max_tokens,
    };
    let mut sampler = Sampler::new(cfg, set.seed);
    let bs = model.cfg.block_size;

    let out = io::stdout();
    let mut lock = out.lock();
    write!(lock, "{C_GREEN}").ok();

    let mut produced: Vec<u16> = Vec::new();
    let mut pending: Vec<u8> = Vec::new(); // bytes not yet on a UTF-8 boundary
    let mut text_so_far = String::new();
    let start = Instant::now();
    let mut stopped = "length";

    for _ in 0..set.max_tokens {
        let ctx_start = toks.len().saturating_sub(bs);
        let mut logits = model.forward(&toks[ctx_start..]);
        let next = sampler.pick(&mut logits);
        toks.push(next);
        produced.push(next);

        pending.extend_from_slice(&model.decode(&[next]));
        // Flush the longest valid UTF-8 prefix of `pending`.
        let good = match std::str::from_utf8(&pending) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        if good > 0 {
            let chunk = String::from_utf8_lossy(&pending[..good]).into_owned();
            write!(lock, "{chunk}").ok();
            lock.flush().ok();
            text_so_far.push_str(&chunk);
            pending.drain(..good);
        }

        if set.stops.iter().any(|s| !s.is_empty() && text_so_far.ends_with(s.as_str())) {
            stopped = "stop-seq";
            break;
        }
    }
    if !pending.is_empty() {
        let chunk = String::from_utf8_lossy(&pending).into_owned();
        write!(lock, "{chunk}").ok();
        text_so_far.push_str(&chunk);
    }
    write!(lock, "{C_RESET}").ok();

    let dt = start.elapsed().as_secs_f64();
    let n = produced.len();
    writeln!(
        lock,
        "\n{C_DIM}— {n} tok · {:.2}s · {:.1} tok/s · {}{C_RESET}",
        dt,
        n as f64 / dt.max(1e-6),
        stopped
    )
    .ok();

    ctx.push_str(text_so_far.trim_end());
    ctx.push('\n');
}
