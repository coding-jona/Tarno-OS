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

Status: builds an `.hybrid.iso` in CI, `tarnod` is packaged in, ISOLINUX
boots (see git history for the module-symlink fix). Login: `user` / `live`
- `user-setup` + `sudo` are required for live-config to actually create
the account and give it admin rights
(`/usr/lib/live/config/0030-user-setup`, `0040-sudo` - both silently
no-op if their package isn't installed, which is what happened before
this was added). root is permanently locked by live-config itself,
regardless.

Build/test locally: see the top-level README.md (`make devuan-iso`,
`make devuan-run`).
