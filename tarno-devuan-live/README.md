# tarno-devuan-live

live-build config for a Devuan (excalibur) live image with OpenRC instead
of systemd. Built via `.github/workflows/build-devuan-image.yml`
(`workflow_dispatch`, not on every push — a full `lb build` pulls a base
system + packages from Devuan's mirrors and takes a while).

Devuan ships sysvinit by default. OpenRC is switched on with the
`init=/sbin/openrc-init` kernel param in `auto/config`, the same mechanism
GRUB/syslinux normally use to override init.

Console login (agetty) and wired DHCP (dhcpcd) are enabled via chroot
hooks - neither comes for free under openrc-init. Wifi isn't wired up.

Status: builds an `.hybrid.iso` in CI, `tarnod` is packaged in. First
real boot test (QEMU/KVM) failed on `ISOLINUX: Failed to load ldlinux.c32`
- our `config/bootloaders/isolinux/` override only had symlinks for
`isolinux.bin`/`vesamenu.c32`, missing the other required syslinux 6
modules (`ldlinux.c32`, and `vesamenu.c32`'s own runtime deps
`libcom32.c32`/`libutil.c32`). Fixed, not yet re-tested.

Build/test locally: see the top-level README.md (`make devuan-iso`,
`make devuan-run`).
