# hal/

Hardware Abstraction Layer for the THOS target machine only (see `docs/thos/hw-target.md`).
Subtree layout: `x86_64/{apic,acpi,pci,msi}/`. ACPI is an ACPICA C port under `third_party/acpica/`.
Starts filling in Phase 1 (APIC, SMP/MADT, timers) and Phase 2 (PCIe enumeration).
