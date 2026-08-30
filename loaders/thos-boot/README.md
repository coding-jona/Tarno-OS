<!-- SPDX-License-Identifier: GPL-2.0-or-later -->
# thos-boot — the THOS boot picker

A standalone UEFI application (`x86_64-unknown-uefi`). It runs *before* any
kernel, finds the OS loaders on every disk the firmware can see, shows a menu,
counts down to a default, and chainloads the choice with `LoadImage` +
`StartImage`. It never rewrites `BootOrder`.

## Build & test

```
cargo xtask bootpick        # build target/x86_64-unknown-uefi/release/thos-boot.efi
cargo xtask bootpick-test   # boot it under OVMF with 3 fake disks, assert it chainloads
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
