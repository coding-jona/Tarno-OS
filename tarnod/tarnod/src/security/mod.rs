//! Behavioral-Security-Subsystem. Der eigentliche eBPF-Loader/Policy-Teil
//! (`ebpf_loader`) ist nur mit Cargo-Feature `ebpf` aktiv (siehe Cargo.toml
//! und docs/month3-tarno-layer.md#woche-11-behavioral-security-ohne-kernel-patch).
//! `events` (Tarno-AI-Phase-3-Baustein) ist bewusst NICHT feature-gated —
//! siehe dessen Moduldoc.

#[cfg(feature = "ebpf")]
pub mod ebpf_loader;
pub mod events;
