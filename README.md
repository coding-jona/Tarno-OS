# Tarno OS

Tarno OS ist ein extrem gestripptes, Linux-basiertes System (Buildroot), das
für Low-RAM-Betrieb, minimalen Gaming-Overhead und einen eigenen
`tarnod`-Sicherheitsdaemon optimiert ist — ohne den Aufwand eines eigenen
Kernels.

- **Plan/Übersicht:** [`ROADMAP.md`](ROADMAP.md)
- **Technische Details je Monat + Architektur:** [`docs/`](docs/)

## Repo-Struktur

| Pfad | Zweck |
|---|---|
| [`tarnod/`](tarnod/) | Root-Daemon (`tarnod`), CLI-Client (`tarnoctl`), natives GUI (`tarnod-ui`) — Rust-Workspace |
| [`tarno-guard-ebpf/`](tarno-guard-ebpf/) | eBPF-Behavioral-Security (Tracepoint auf `execve`, RingBuf-Events, Userspace-Loader) |
| [`scripts/`](scripts/) | Gaming-Mode-Tuning (CPU-Governor, THP, JVM-Start-Wrapper, FPS-Benchmark) |
| [`tarno-br2-external/`](tarno-br2-external/) | Buildroot-`BR2_EXTERNAL`-Tree (Kernel-Config, `tarnod`-Package, Board-Support M6700) |
| [`docs/`](docs/) | Architektur + detaillierte Monatspläne mit Befehlen/Configs/Abnahmekriterien |

## Schnellstart (Entwicklung, ohne Buildroot)

```sh
cd tarnod
cargo build --workspace
cargo test --workspace
./target/debug/tarnod &         # TARNOD_DRY_RUN=1 für Sandboxes ohne echte cpufreq/THP-Pfade
./target/debug/tarnoctl ping
./target/debug/tarnod-ui        # natives GUI (egui/eframe), verbindet sich auf denselben Socket
```

eBPF-Feature (Behavioral Security) aktivieren — braucht root/`CAP_BPF`+`CAP_PERFMON`
und einen Kernel mit BTF, siehe [`tarno-guard-ebpf/BPF_LINKER.md`](tarno-guard-ebpf/BPF_LINKER.md)
für den Toolchain-Aufbau:

```sh
cargo build --workspace --features tarnod/ebpf
TARNOD_DENY_LIST="cryptominer" ./target/debug/tarnod
```

Details, Architektur-Begründungen und der Status je Roadmap-Meilenstein:
siehe [`docs/architecture.md`](docs/architecture.md).
