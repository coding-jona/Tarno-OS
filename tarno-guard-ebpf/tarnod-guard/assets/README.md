# `tarnod-guard.bpf.o`

Vorkompiliertes eBPF-Objekt (Ergebnis von `tarnod-guard-ebpf`, gebaut für
`bpfel-unknown-none`), das `tarnod-guard/src/lib.rs` per
`aya::include_bytes_aligned!` einbettet.

**Warum ein Build-Artefakt im Repo statt Build-Zeit-Kompilierung:** siehe
[`../../docs/architecture.md`](../../docs/architecture.md#buildroot-integration-tarno-br2-external).
Kurzfassung: weder `tarnod` selbst noch der spätere Buildroot-Cross-Build
sollen eine Host-BPF-Toolchain (nightly Rust + bpf-linker) brauchen — nur wer
das eBPF-Programm ändert, baut es neu.

## Neu bauen

```sh
cd tarno-guard-ebpf/
./build.sh
```

Voraussetzungen und bekannte Stolpersteine: siehe
[`../BPF_LINKER.md`](../BPF_LINKER.md).

## Verifikation

Nach dem Neu-Bauen empfiehlt sich ein manueller Test mit dem
Standalone-Loader, bevor das Objekt committet wird:

```sh
cd tarno-guard-ebpf/
cargo build --release
sudo RUST_LOG=info ./target/release/tarnod-guard-standalone
# in einem zweiten Terminal: irgendein Programm ausführen, z.B. `true`
```

Erwartete Ausgabe: eine Zeile pro `execve()` mit `pid`, `uid`, `comm`,
`filename`.
