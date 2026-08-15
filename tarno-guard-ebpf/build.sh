#!/usr/bin/env bash
# Baut tarnod-guard-ebpf (Kernel-Space-Programm) und kopiert das Ergebnis als
# vorkompiliertes Artefakt nach tarnod-guard/assets/tarnod-guard.bpf.o, wo es
# per `include_bytes_aligned!` in den Userspace-Loader eingebettet wird.
#
# Warum vorkompiliert statt zur Build-Zeit von tarnod/Buildroot neu gebaut:
# siehe docs/architecture.md#buildroot-integration-tarno-br2-external. Damit
# brauchen weder `tarnod` noch der Buildroot-Cross-Build eine Host-BPF-
# Toolchain (nightly Rust + rust-src + bpf-linker) — nur wer dieses Skript
# hier manuell ausführt, braucht sie.
#
# Voraussetzungen (siehe BPF_LINKER.md für Details/Stolpersteine):
#   rustup toolchain install nightly
#   rustup component add rust-src --toolchain nightly
#   cargo install bpf-linker --version 0.10.4 --locked   # siehe BPF_LINKER.md
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EBPF_CRATE_DIR="$SCRIPT_DIR/tarnod-guard-ebpf"
DEST="$SCRIPT_DIR/tarnod-guard/assets/tarnod-guard.bpf.o"

if ! command -v bpf-linker >/dev/null 2>&1; then
    echo "error: bpf-linker nicht im PATH gefunden. Siehe BPF_LINKER.md." >&2
    exit 1
fi

echo "==> baue tarnod-guard-ebpf (nightly, target bpfel-unknown-none) ..."
( cd "$EBPF_CRATE_DIR" && cargo +nightly build --release )

SRC="$EBPF_CRATE_DIR/../target/bpfel-unknown-none/release/tarnod-guard"
if [ ! -f "$SRC" ]; then
    echo "error: erwartetes Build-Ergebnis nicht gefunden: $SRC" >&2
    exit 1
fi

cp "$SRC" "$DEST"
echo "==> kopiert nach $DEST ($(stat -c%s "$DEST") Bytes)"
echo "==> Sanity-Check (ELF-Header):"
readelf -h "$DEST" | grep -E "Type|Machine"
