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
//! /seed /stops /reset /multi /save /reload /lang /train /exit
#![cfg(not(target_os = "none"))]

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
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
    /// When set (e.g. "de"), converse in this language: your input is
    /// translated to English before the model sees it, its English output is
    /// translated back before you see it. The model itself stays English-only
    /// — see `Translator`.
    lang: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            temperature: 0.9,
            top_k: 40,
            max_tokens: 400,
            seed: rand_seed(),
            stops: vec![], // no default stop — a small model hits blank lines fast; /stops to opt in
            ctx_bytes: 4096,
            lang: None,
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
    let mut lang: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--weights" | "--model" | "-m" => weights = args.next().unwrap_or_default(),
            "--system" | "-s" => sys_prompt = args.next().unwrap_or_default(),
            "--lang" | "-l" => lang = args.next().filter(|c| c != "en" && c != "off"),
            "--help" | "-h" => {
                eprintln!("usage: thos-shell --weights <path.tlm> [--system <text>] [--lang de]");
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
    set.lang = lang;
    banner(&weights, model_size, &model, &set);
    if model_mtime.is_some() {
        println!(
            "  {C_DIM}live: this file is checked before every turn — a checkpoint that\n  \
             'run.sh watch-export' refreshes mid-training is picked up automatically.{C_RESET}\n"
        );
    }
    let mut translator: Option<Translator> = None;

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
                    converse(&model, &set, &mut ctx, &mut translator, &line);
                }
            }
            continue;
        }

        reload(&weights, &mut model, &mut model_mtime, &mut model_size, false);
        converse(&model, &set, &mut ctx, &mut translator, &line);
    }
}

/// A long-lived coprocess wrapping `ml/train/translate.py` (argos-translate:
/// Apache-2.0, fully offline, no API key). Not part of the model — a text
/// in/out utility layer, spoken to over JSON lines so the translation models
/// load once instead of per call.
struct Translator {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Translator {
    fn spawn() -> Option<Translator> {
        let py = first_existing(&[
            "ml/train/.venv/bin/python",
            "../train/.venv/bin/python",
            "../../ml/train/.venv/bin/python",
        ])
        .unwrap_or_else(|| "python3".to_string());
        let script = first_existing(&[
            "ml/train/translate.py",
            "../train/translate.py",
            "../../ml/train/translate.py",
            "translate.py",
        ])?;
        let mut child = Command::new(&py)
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = BufReader::new(child.stdout.take()?);
        let mut t = Translator { child, stdin, stdout };
        // First call also lazy-loads the translation models inside the
        // coprocess, so it's slow (a few seconds) — that's expected.
        if t.call(r#"{"ping": true}"#).is_none() {
            return None;
        }
        Some(t)
    }

    fn call(&mut self, request: &str) -> Option<String> {
        writeln!(self.stdin, "{request}").ok()?;
        self.stdin.flush().ok()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).ok()?;
        if line.is_empty() {
            None
        } else {
            Some(line)
        }
    }

    fn translate(&mut self, from: &str, to: &str, text: &str) -> Option<String> {
        if from == to || text.trim().is_empty() {
            return Some(text.to_string());
        }
        let req = format!(
            r#"{{"from": "{}", "to": "{}", "text": "{}"}}"#,
            json_escape(from),
            json_escape(to),
            json_escape(text)
        );
        json_extract_text(&self.call(&req)?)
    }
}

impl Drop for Translator {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Pulls the `"text"` field out of a `{"text": "..."}` response line. Not a
/// general JSON parser — we control both ends of this protocol.
fn json_extract_text(line: &str) -> Option<String> {
    let key = "\"text\": \"";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let mut out = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// Finds `run.sh` and its directory (so config paths resolve regardless of
/// where thos-shell itself was launched from).
fn run_sh_and_config_dir() -> Option<(String, String)> {
    let run_sh = first_existing(&[
        "ml/train/run.sh",
        "../train/run.sh",
        "../../ml/train/run.sh",
        "train/run.sh",
        "run.sh",
    ])?;
    let dir = std::path::Path::new(&run_sh)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some((run_sh, dir))
}

/// Runs a command with inherited stdio, so run.sh's own messages print
/// straight into the shell — no separate output-capturing plumbing needed.
fn run_inherit(mut cmd: Command) {
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => println!("  {C_YELLOW}exited with {s}{C_RESET}"),
        Err(e) => println!("  {C_YELLOW}failed to run: {e}{C_RESET}"),
    }
}

/// `/train start|staged|stop|pause|resume|status [config-stem|hours]` — lets
/// the shell double as a control surface for the same training run the
/// dashboard and `run.sh ctl` talk to (same control.json).
fn handle_train(sub: &str, extra: Option<&str>) {
    let Some((run_sh, dir)) = run_sh_and_config_dir() else {
        println!("  {C_YELLOW}can't find ml/train/run.sh from here{C_RESET}");
        return;
    };
    match sub {
        "" => println!(
            "  {C_YELLOW}usage: /train start|staged|stop|pause|resume|status [config-stem|hours]{C_RESET}"
        ),
        "start" => {
            let mut cmd = Command::new("bash");
            cmd.arg(&run_sh).arg("train-bg");
            if let Some(stem) = extra {
                cmd.env("CONFIG", format!("{dir}/config/{stem}.toml"));
                cmd.env("TLM", format!("{stem}.tlm"));
            }
            run_inherit(cmd);
        }
        "staged" => {
            let mut cmd = Command::new("bash");
            cmd.arg(format!("{dir}/staged.sh")).arg("start");
            if let Some(hours) = extra {
                cmd.arg("--hours").arg(hours);
            }
            run_inherit(cmd);
        }
        "stop" | "pause" | "resume" => {
            // Default to whichever run is most recently active (same rule as
            // /train status) rather than run.sh's static default config —
            // otherwise this would silently target the wrong job once more
            // than one config has ever been trained.
            let stem = extra.map(str::to_string).or_else(|| most_recent_run(&dir));
            let mut cmd = Command::new("bash");
            cmd.arg(&run_sh).arg("ctl").arg(sub);
            match &stem {
                Some(s) => {
                    cmd.env("CONFIG", format!("{dir}/config/{s}.toml"));
                }
                None => println!("  {C_YELLOW}no active run found — defaulting to run.sh's own default config{C_RESET}"),
            }
            run_inherit(cmd);
        }
        "status" => print_train_status(&dir),
        other => println!(
            "  {C_YELLOW}unknown /train {other} — start|staged|stop|pause|resume|status{C_RESET}"
        ),
    }
}

/// Picks whichever out/<config>/ under `train_dir` has the most recently
/// updated log.csv — i.e. whatever's actively (or most recently) training.
fn most_recent_run(train_dir: &str) -> Option<String> {
    let out_base = format!("{train_dir}/out");
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for e in std::fs::read_dir(&out_base).ok()?.flatten() {
        let log = e.path().join("log.csv");
        if let Ok(mtime) = std::fs::metadata(&log).and_then(|m| m.modified()) {
            let stem = e.file_name().to_string_lossy().into_owned();
            if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                best = Some((mtime, stem));
            }
        }
    }
    best.map(|(_, s)| s)
}

fn print_train_status(train_dir: &str) {
    let Some(stem) = most_recent_run(train_dir) else {
        println!("  {C_YELLOW}no training run found under {train_dir}/out{C_RESET}");
        return;
    };
    let dir = std::path::Path::new(train_dir).join("out").join(&stem);
    let last_line = std::fs::read_to_string(dir.join("log.csv"))
        .ok()
        .and_then(|s| s.lines().last().map(|l| l.to_string()));
    let ctl = std::fs::read_to_string(dir.join("control.json")).unwrap_or_default();
    let state = if ctl.contains("\"pause\": true") {
        "PAUSED"
    } else if ctl.contains("\"stop\": true") {
        "STOP REQUESTED"
    } else {
        "running"
    };
    println!("  {C_CYAN}config{C_RESET} {stem}   {C_CYAN}state{C_RESET} {state}");
    match last_line {
        Some(l) => {
            let p: Vec<&str> = l.split(',').collect();
            if p.len() == 5 {
                println!(
                    "  {C_CYAN}step{C_RESET} {}   {C_CYAN}train{C_RESET} {}   {C_CYAN}val{C_RESET} {}   \
                     {C_CYAN}lr{C_RESET} {}   {C_CYAN}tok/s{C_RESET} {}",
                    p[0], p[1], p[2], p[3], p[4]
                );
            }
        }
        None => println!("  {C_DIM}no eval rows yet{C_RESET}"),
    }
}

/// Ensures a translator is running if `set.lang` is set, then runs one turn:
/// translate the user's text to English, generate, translate the answer back.
/// The model itself only ever sees/produces English.
fn converse(
    model: &Model,
    set: &Settings,
    ctx: &mut String,
    translator: &mut Option<Translator>,
    text: &str,
) {
    let Some(code) = set.lang.clone() else {
        run_turn(model, set, ctx, text);
        return;
    };
    if translator.is_none() {
        println!("{C_DIM}starting local translator (argos-translate, offline) ...{C_RESET}");
        *translator = Translator::spawn();
        if translator.is_none() {
            println!(
                "{C_YELLOW}couldn't start the translator — is argostranslate installed \
                 (ml/train/translate.py --install)? Falling back to English.{C_RESET}"
            );
        }
    }
    let Some(tr) = translator else {
        run_turn(model, set, ctx, text);
        return;
    };
    let english = tr.translate(&code, "en", text).unwrap_or_else(|| text.to_string());
    if english != text {
        println!("{C_DIM}[en] {english}{C_RESET}");
    }
    let out = run_turn(model, set, ctx, &english);
    match tr.translate("en", &code, &out) {
        Some(back) => println!("{C_MAGENTA}[{code}] {back}{C_RESET}"),
        None => println!("{C_YELLOW}(translation back to {code} failed — see the English above){C_RESET}"),
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
        "train" => handle_train(arg, it.next()),
        "lang" => {
            if arg.is_empty() {
                match &set.lang {
                    Some(c) => println!("  {C_DIM}lang{C_RESET} {c}  {C_DIM}(/lang off to disable, /lang <code> to switch){C_RESET}"),
                    None => println!("  {C_DIM}lang{C_RESET} off  {C_DIM}(model speaks English; /lang de to converse in German){C_RESET}"),
                }
            } else if arg == "off" || arg == "en" {
                set.lang = None;
                println!("  {C_GREEN}lang off — talking to the model directly in English{C_RESET}");
            } else {
                set.lang = Some(arg.to_string());
                println!(
                    "  {C_GREEN}lang = {arg}{C_RESET}  {C_DIM}(translated via argos-translate, offline; \
                     first use starts the translator, a few seconds){C_RESET}"
                );
            }
        }
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
        ("/lang <code|off>", "converse in <code> (e.g. de) via a local offline translator"),
        ("/train start", "start CPU training in the background (bg -> run.sh train-bg)"),
        ("/train staged [h]", "start the gentle-then-full-throttle pipeline (staged.sh)"),
        ("/train stop|pause|resume", "control the active run (writes the same control.json as run.sh ctl)"),
        ("/train status", "step / loss / lr / tok/s of whichever run is most recently active"),
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
fn run_turn(model: &Model, set: &Settings, ctx: &mut String, user: &str) -> String {
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

    let trimmed = text_so_far.trim_end().to_string();
    ctx.push_str(&trimmed);
    ctx.push('\n');
    trimmed
}
