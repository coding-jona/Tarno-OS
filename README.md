# Tarno OS

Tarno OS ist heute **shell-only**: kein GUI-Layer im Repo. Der aktive Kern
ist `tarnod`, ein Root-Daemon, der drei Dinge vereint — Gaming-Tuning
(CPU-Governor/Isolation), eBPF-basierte Behavioral-Security und **Tarno AI**
(im Aufbau — ein Assistent, der über denselben Daemon läuft, siehe
[`docs/month3-tarno-layer.md`](docs/month3-tarno-layer.md)). Eine
Debian-Basis (statt des heutigen Buildroot-Cross-Compiles) ist das
langfristige, zurückgestellte Ziel — siehe `ROADMAP.md`, Abschnitt
"Zukunft — Debian-Basis".

- **Plan/Übersicht:** [`ROADMAP.md`](ROADMAP.md)
- **Technische Details je Monat + Architektur:** [`docs/`](docs/)

## Repo-Struktur

| Pfad | Zweck |
|---|---|
| [`tarnod/`](tarnod/) | Root-Daemon (`tarnod`), CLI-Client (`tarnoctl`) — Rust-Workspace |
| [`tarno-guard-ebpf/`](tarno-guard-ebpf/) | eBPF-Behavioral-Security (Tracepoint auf `execve`, RingBuf-Events, Userspace-Loader) |
| [`scripts/`](scripts/) | Gaming-Mode-Tuning (CPU-Governor, THP, JVM-Start-Wrapper, FPS-Benchmark) |
| [`tarno-br2-external/`](tarno-br2-external/) | Buildroot-`BR2_EXTERNAL`-Tree (Kernel-Config, `tarnod`-Package, Board-Support M6700, USB-Boot-Image) |
| [`docs/`](docs/) | Architektur + detaillierte Monatspläne mit Befehlen/Configs/Abnahmekriterien |

## Schnellstart (Entwicklung, ohne Buildroot)

```sh
cd tarnod
cargo build --workspace
cargo test --workspace
./target/debug/tarnod &         # TARNOD_DRY_RUN=1 für Sandboxes ohne echte cpufreq/THP-Pfade
./target/debug/tarnoctl ping
```

eBPF-Feature (Behavioral Security) aktivieren — braucht root/`CAP_BPF`+`CAP_PERFMON`
und einen Kernel mit BTF, siehe [`tarno-guard-ebpf/BPF_LINKER.md`](tarno-guard-ebpf/BPF_LINKER.md)
für den Toolchain-Aufbau:

```sh
cargo build --workspace --features tarnod/ebpf
TARNOD_DENY_LIST="cryptominer" ./target/debug/tarnod
```

**Die eigentliche `sdcard.img` bauen** (das Tarno-OS-Image selbst):
`.github/workflows/build-os-image.yml` per GitHub Actions manuell
auslösen (Actions-Tab → "Build Tarno OS image" → "Run workflow") — läuft
komplett auf GitHub-Infrastruktur, kein eigener Linux-Rechner nötig.
Dauert je nach Paketumfang etwa eine bis mehrere Stunden, Ergebnis liegt
danach als Artifact zum Download bereit. Auf einen USB-Stick schreiben
funktioniert aktuell nur per `dd`, siehe
[`docs/month1-foundation.md`](docs/month1-foundation.md).

Details, Architektur-Begründungen und der Status je Roadmap-Meilenstein:
siehe [`docs/architecture.md`](docs/architecture.md).
