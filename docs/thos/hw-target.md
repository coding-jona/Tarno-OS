# THOS – Hardware target

> ## Current target: Acer Aspire 5742G (legacy BIOS, MBR) — *user, 2026-10-02*
>
> THOS now also targets an **Acer Aspire 5742G** laptop. Its firmware is
> **legacy BIOS only: no UEFI, no CSM switch, MBR / MS-DOS partition table.**
> Targets are ordinary **SATA SSDs, no NVMe**. Figures below come from a
> comparison sheet (user screenshot); re-capture with the `lspci`/`lscpu`
> commands in *Raw inventory* before writing drivers against exact IDs.
>
> | Part | Acer Aspire 5742G | (HP EliteBook 840 G1, for comparison) |
> |---|---|---|
> | CPU | Core **i3-370M**, Westmere 2010, 3.5 GHz as listed | i5-4300U Haswell 2013 |
> | GPU | **Radeon HD 5470M**, 512 MB GDDR3, 40 shader units (Evergreen / TeraScale 2) | Intel HD 4000 |
> | Chipset | Intel **HM55** (Ibex Peak-M) — expected, to verify | — |
> | Firmware | BIOS + MBR | — |
>
> What this changes versus the ASRock/Raptor Lake target below:
>
> | Area | Consequence |
> |---|---|
> | Boot | **Limine BIOS** (stage 1 in the MBR, stage 2 in the post-MBR gap) instead of UEFI. `cargo xtask bios-image` / `bios-test`. The UEFI boot picker (`loaders/thos-boot`) does **not** apply here. |
> | Disk | **MBR partition table** — `kernel/src/mbr.rs`; the root ext2 lives in a type-`0x83` partition (`ext2::open` probes it). No GPT/ESP on this machine. |
> | Keyboard | Internal keyboard is **PS/2 (i8042)**; HM55 has **no xHCI** (USB 2 EHCI only) — `kernel/src/ps2.rs`. External USB keyboards need an EHCI driver (not written). |
> | Storage | HM55 SATA is **3 Gb/s (SATA II)**: a SATA III 6 Gb/s SSD works but is link-limited. The BIOS must have SATA mode = **AHCI** (the driver matches class `0106`; IDE/compat mode shows `0101` and is **not** supported). |
> | CPU | Westmere: xAPIC only (no x2APIC), **no TSC-deadline**, no AVX/XSAVE, no RDRAND/SMEP — the kernel already uses xAPIC + PIT-calibrated periodic timer and an RDRAND-or-fallback salt, so nothing blocks. 2 cores / 4 threads: SMP code is generic. |
> | GPU | Boot framebuffer comes from **VBE via Limine**; a native Evergreen KMS driver is a far-future item, replacing the RDNA2 plan. |
> | Safety | Fixed-LBA scratch/stress tests are skipped when the root FS is found in a partition (`ext2::on_partition`). |
>
> The ASRock section below stays as the **secondary/original** target.

THOS targets **exactly one machine**. Every driver is written for the chips below and
nothing else. This is what keeps the project finite. Inventory captured 2026-08-29 from
the running Devuan install on that machine.

## System

| Part | Value |
|---|---|
| Motherboard | ASRock **B760M-HDV/M.2 D4** |
| Firmware | AMI UEFI, BIOS **11.01** (2025-09-25). Boots UEFI (EFI vars present). |
| Chipset | Intel **B760** PCH — ISA bridge `8086:7a06`, SMBus `8086:7a23` |

## CPU

| | |
|---|---|
| Model | Intel Core **i7-13700KF** (Raptor Lake, `GenuineIntel`) |
| Topology | **24 threads** = 8 P-cores (Raptor Cove) ×2 HT + 8 E-cores (Gracemont) ×1 |
| Sockets / cores-per-socket | 1 / 16 |
| Notes | No integrated GPU ("KF") ⇒ no fallback display path. Hybrid topology from `CPUID.1AH`. Thread Director / HFI (`IA32_HW_FEEDBACK_*`) — Phase 5. |

## GPU

| | |
|---|---|
| Model | AMD **Radeon RX 6600** — Navi 23, RDNA2, `GFX10.3` |
| PCI ID | `1002:73ff` (rev c7) at `03:00.0` (VGA) |
| HDMI/DP audio | `1002:ab28` at `03:00.1` |
| Topology | Behind an on-card AMD PCIe switch: upstream `1002:1478` (`01:00.0`), downstream `1002:1479` (`02:00.0`), GPU at bus `03`. The KMS driver's PCIe probe must walk this bridge chain. |
| Plan | Port Mesa RADV for userspace. KMS via Path A (port `amdgpu`) or Path B (thin RDNA2 KMS + DRM compat). Decide after the Phase 4 spike. DCN 3.x display, PM4/SDMA rings, SMU power. |

## Storage — THOS boots from SATA, not NVMe

Disk map on the machine (triple-boot target):

| Linux dev | Drive | Use | THOS |
|---|---|---|---|
| `nvme0n1` | INTEL SSDPEKKF512G8L 512 GB | **Windows 11** | not touched — boot-menu entry only |
| `sdb` | Samsung 850 EVO 250 GB | **Devuan** | boot-menu entry |
| `sdc` | **KINGSTON SA400S37240G 240 GB** | **THOS install target** | root FS lives here |
| `sda` | Seagate ST1000LM024 1 TB HDD | user data | not touched |
| `sde` | Toshiba TransMemory 14.5 GB USB | intended THOS boot stick (later) | — |

⇒ **Phase 2 storage driver = AHCI/SATA**, not NVMe.

| | |
|---|---|
| Controller | Intel **Raptor Lake SATA AHCI Controller** `8086:7a62` (rev 11) at `00:17.0`, class `0106` |
| Boot disk | `sdc` = Kingston A400 240 GB (SATA, `KINGSTON SA400S37240G`) |
| Notes | Standard AHCI 1.3.1 register interface — no NVMe queue machinery needed. THOS never reads the Windows NVMe. |

## Network

| | |
|---|---|
| NIC | **Realtek RTL8111/8168/8211/8411** PCIe Gigabit Ethernet `10ec:8168` (rev 15) at `05:00.0` |
| Driver class | `r8169`-family (RTL8168h/RTL8111h-era). Phase 5. |

## USB

| | |
|---|---|
| Controller | Intel **Raptor Lake USB 3.2 Gen 2x2 xHCI Host Controller** `8086:7a60` (rev 11) at `00:14.0` |
| Plan | xHCI 1.x standard interface + USB-HID for keyboard/mouse. Phase 2. |

## Audio (Phase 5)

| | |
|---|---|
| PCH codec | Intel **Raptor Lake HD Audio Controller** `8086:7a50` at `00:1f.3` |
| GPU HDMI audio | AMD `1002:ab28` at `03:00.1` |

## Raw inventory

Full `lspci -nn` / `lsblk` / `lscpu` output archived at `~/hw-inventory.txt` on the
machine. Re-capture with:

```bash
lspci -nnvvv ; lsblk -d -o NAME,MODEL,SIZE,TRAN,ROTA ; lscpu
lsusb -t ; for f in board_name bios_version; do cat /sys/class/dmi/id/$f; done
```
