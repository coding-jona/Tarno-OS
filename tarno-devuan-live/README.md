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
boots (see git history for the module-symlink fix). tty1 autologs in as
`user` (`0200-agetty-console.chroot`, agetty `--autologin`) - no
username/password to type at all. The account is created at build time
by `0175-user-account.chroot` (plain `adduser`, fixed group list,
locked password) rather than left to live-config's own runtime
`0030-user-setup` component - that component happily no-ops once the
account it wants already exists, which used to leave `user` in zero
supplementary groups (see the top-level README's "Tenth real boot
test" for the actual bug this caused). Admin rights come from
`etc/sudoers.d/tarno-user` (`NOPASSWD`, by username, not by group).
root is permanently locked by live-config itself, regardless.

Build/test locally: see the top-level README.md (`make devuan-iso`,
`make devuan-run`).
