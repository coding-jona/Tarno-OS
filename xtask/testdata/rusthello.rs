// SPDX-License-Identifier: GPL-2.0-or-later
// THOS static-musl test: a real Rust std binary. Exercises TLS setup, args,
// heap, formatting, stdout, exit.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("hello from a static musl Rust binary");
    println!("argc={} argv0={}", args.len(), args.get(0).map(|s| s.as_str()).unwrap_or("?"));
    let sum: u64 = (1..=100).sum();
    println!("sum 1..=100 = {sum}");
    std::process::exit(3);
}
