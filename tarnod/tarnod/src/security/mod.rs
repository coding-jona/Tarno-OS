//! Behavioral-Security-Subsystem. Nur mit Cargo-Feature `ebpf` aktiv (siehe
//! Cargo.toml und docs/month3-tarno-layer.md#woche-11-behavioral-security-ohne-kernel-patch).

#[cfg(feature = "ebpf")]
pub mod ebpf_loader;
