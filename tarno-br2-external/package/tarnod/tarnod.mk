################################################################################
#
# tarnod
#
################################################################################

TARNOD_VERSION = 0.1.0
# Lokales Source aus dem selben Monorepo statt Tarball-Download (siehe
# docs/month1-foundation.md#buildroot-setup). tarnod/ ist ein Cargo-
# Workspace (Mitglieder: tarnod, tarnoctl) — SITE zeigt bewusst auf den
# Workspace-Root, damit Cargo.lock/Workspace-Resolution korrekt greifen.
TARNOD_SITE = $(BR2_EXTERNAL_TARNO_OS_PATH)/../tarnod
TARNOD_SITE_METHOD = local
TARNOD_LICENSE = AGPL-3.0-only
TARNOD_LICENSE_FILES = $(BR2_EXTERNAL_TARNO_OS_PATH)/../LICENSE

TARNOD_DEPENDENCIES = host-rustc

# Das "ebpf"-Feature (eBPF-Behavioral-Security) einzubinden braucht HIER
# keine Host-BPF-Toolchain (kein nightly Rust, kein bpf-linker): das
# eBPF-Objekt in tarno-guard-ebpf/tarnod-guard/assets/tarnod-guard.bpf.o
# ist bereits vorkompiliert und wird nur per include_bytes! eingebettet
# (eBPF-Bytecode ist architekturunabhängig). Um es einzuschalten, das
# cargo-package-Feature-Flag setzen — TODO: exakte <PKG>_CARGO_*-Variable
# gegen die tatsächlich verwendete Buildroot-Version verifizieren (siehe
# docs/month1-foundation.md, Scope-Hinweis: kein echter Buildroot-Build in
# der Entwicklungs-Sandbox möglich):
#
#   TARNOD_CARGO_ENV += CARGO_BUILD_FLAGS="--features tarnod/ebpf"
#
# (Standardmäßig AUS gelassen, bis das gegen einen echten Buildroot-
# Checkout verifiziert ist.)

$(eval $(cargo-package))
