<!-- SPDX-License-Identifier: GPL-2.0-or-later -->
# thos-boot — the THOS boot picker

A standalone UEFI application (`x86_64-unknown-uefi`). It runs *before* any
kernel, finds the OS loaders on every disk the firmware can see, shows a menu,
counts down to a default, and chainloads the choice with `LoadImage` +
`StartImage`. It never rewrites `BootOrder`.

## Measured boot

Right before `StartImage` on the chosen loader, the picker measures that
loader's already-`LoadImage`d bytes into the TPM (`EFI_TCG2_PROTOCOL`, PCR 4 —
"Boot Manager Code and Boot Attempts" per the TCG PC Client spec) and logs the
event. This only happens if a TPM 2.0 device and the TCG2 protocol are present
— most dev/test machines (and plenty of real ones) have neither, and the
picker boots exactly the same either way; measuring is additive, never a boot
requirement.

## Build & test

```
cargo xtask bootpick             # build target/x86_64-unknown-uefi/release/thos-boot.efi
cargo xtask bootpick-test        # boot it under OVMF with 3 fake disks, assert it chainloads
cargo xtask bootpick-tpm-test    # same, with a real swtpm attached — asserts the TCG2
                                  # measurement actually reaches the TPM. Needs `swtpm` on
                                  # PATH; skips (not fails) if it isn't.
```

## How it finds OSes

1. **NVRAM** — `BootOrder` + each `Boot####` load option, filtered to entries
   that reach a hard-drive partition and end in `*.efi` (firmware apps — setup
   UI, UEFI shell, generic "UEFI …" fallbacks — are dropped).
2. **Filesystem probe** — every `SimpleFileSystem` volume is checked for
   well-known loader paths: `\EFI\Microsoft\Boot\bootmgfw.efi` (Windows),
   `\EFI\<distro>\{shim,grub}x64.efi`, `\EFI\systemd\systemd-bootx64.efi`, and
   `\EFI\limine\BOOTX64.EFI` / `\EFI\thos\BOOTX64.EFI` → **THOS**.

Entries are de-duplicated by label.

## Config — `\EFI\thos\boot.conf`

Read from the ESP the picker itself was launched from. Optional.

```
timeout=5          # seconds before the default boots; 0 = wait forever
default=THOS       # entry index (0,1,2,…) or a substring of the label
```

## Installing onto a disk

The picker is the firmware's removable-media fallback loader:

```
<ESP>/EFI/BOOT/BOOTX64.EFI      <- thos-boot.efi
<ESP>/EFI/thos/boot.conf        <- optional config
<ESP>/EFI/limine/BOOTX64.EFI    <- the THOS kernel loader (Limine), so "THOS" appears
```

Dropping these on a disk's ESP is additive — it does not touch partitioning or
any other OS. Point the mainboard's boot menu at that disk (or make it first in
the boot order) and every boot lands in the picker.
