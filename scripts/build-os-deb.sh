#!/bin/sh
# Builds tarno-os.deb - the whole OS layer (tarnod, the labwc/waybar
# desktop, tarno-settings/tarno-store/tarno-assistant, all the OpenRC
# service wiring) as one installable/upgradable apt package.
#
# Until now the ONLY way to get any of this was a fresh ISO -
# tarno-tools.deb (build-deb.sh) only ever shipped the two install-
# media tools (tarno-install, tarno-disk-install), nothing of the OS
# itself. An already-installed Tarno OS system had no update path at
# all beyond "reinstall from a new ISO". This is that update path.
#
# Built from literally the same files the live image ships
# (tarno-devuan-live/config/includes.chroot/) so there's exactly one
# source of truth for what "Tarno OS" is - a change to a config file,
# a new app, a new OpenRC hook all land in the live ISO and this
# package identically, no separate list to keep in sync by hand.
#
# The hook scripts under tarno-devuan-live/config/hooks/*.chroot
# (rc-update add, ln -sf agetty, sed -i on /etc/inittab, ...) are
# already idempotent shell scripts operating on the local filesystem -
# nothing chroot-specific about any of them, so they work identically
# as this package's own postinst against a real installed system.
# Reused as-is (copied in, run from postinst) instead of re-deriving
# the same rc-update/chmod/ln logic a second time and risking drift.
# 0100-rsvg-compat.chroot is deliberately excluded - it's a live-build-
# time-only shim for ISOLINUX splash generation on the build host,
# nothing an installed system's own runtime ever needs.
set -eu

VERSION="${VERSION:-0.0.0+g$(git rev-parse --short HEAD)}"
ARCH="${ARCH:-amd64}"
OUT="${OUT:-.}"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
includes="${repo_root}/tarno-devuan-live/config/includes.chroot"
hooks="${repo_root}/tarno-devuan-live/config/hooks"

mkdir -p "$OUT"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/DEBIAN"

# Static tree first (configs, Python apps, .desktop files, init
# scripts) - the actual binaries below overwrite/add to usr/bin on top
# of this, same as tarno-devuan-live/config/hooks doesn't need to
# touch any of these files itself.
cp -a "${includes}/." "$root/"
# __pycache__ can exist in a local checkout from running the Python
# apps directly during development (confirmed: showed up in a real
# build here) - never something to actually ship, Python regenerates
# it itself on first run regardless.
find "$root" -name '__pycache__' -type d -prune -exec rm -rf {} +
mkdir -p "$root/usr/bin"
GOOS=linux GOARCH="${ARCH}" go build -o "$root/usr/bin/tarnod" "${repo_root}"
GOOS=linux GOARCH="${ARCH}" go build -o "$root/usr/bin/tarno-disk-install" "${repo_root}/cmd/tarno-disk-install"
GOOS=linux GOARCH="${ARCH}" go build -o "$root/usr/bin/tarnoctl" "${repo_root}/cmd/tarnoctl"

mkdir -p "$root/usr/share/tarno-os/postinst.d"
i=0
for hook in "${hooks}"/*.chroot; do
	name="$(basename "$hook" .chroot)"
	[ "$name" = "0100-rsvg-compat" ] && continue
	i=$((i + 1))
	cp "$hook" "$root/usr/share/tarno-os/postinst.d/$(printf '%02d' "$i")-${name}"
	chmod +x "$root/usr/share/tarno-os/postinst.d/$(printf '%02d' "$i")-${name}"
done

cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
# Runs every hook the live ISO itself runs at build time, in the same
# numeric order - see scripts/build-os-deb.sh for why these are the
# exact same files, not a re-derived copy.
set -e
if [ "$1" = "configure" ]; then
	for f in /usr/share/tarno-os/postinst.d/*; do
		"$f"
	done
fi
EOF
chmod +x "$root/DEBIAN/postinst"

# Anything under /etc this package ships is a conffile - dpkg then
# preserves local edits across an upgrade (prompts on a real conflict)
# instead of silently overwriting them, standard Debian packaging
# practice for exactly this kind of shipped-but-user-editable config.
find "$root/etc" -type f | sed "s|^${root}||" > "$root/DEBIAN/conffiles"

cat > "$root/DEBIAN/control" <<EOF
Package: tarno-os
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Tarno OS <noreply@tarno-os.invalid>
Depends: openrc, dhcpcd5, iwd, seatd, foot, swaybg, waybar, labwc,
 python3, python3-pyside6.qtcore, python3-pyside6.qtgui, python3-pyside6.qtwidgets,
 pcmanfm-qt, firefox-esr, geany, fuzzel, wl-clipboard, grim, slurp,
 xarchiver, xdg-user-dirs, papirus-icon-theme, synaptic,
 firmware-iwlwifi, firmware-realtek, firmware-atheros, firmware-brcm80211, firmware-misc-nonfree,
 parted, dosfstools, e2fsprogs, rsync, grub-efi-amd64, grub-pc-bin
Description: Tarno OS - tarnod, the desktop, and Tarno's own apps
 Everything that makes an installed Devuan/OpenRC system into Tarno OS:
 the tarnod daemon (gaming tuning, eBPF security, the Mistral-backed
 Tarno assistant), the labwc/waybar desktop, tarno-settings/
 tarno-store/tarno-assistant, and all their OpenRC service wiring.
 Lets an already-installed Tarno OS system pick up OS-level updates
 via apt instead of needing a fresh ISO for every change.
EOF

dpkg-deb --build --root-owner-group "$root" "${OUT}/tarno-os_${VERSION}_${ARCH}.deb"
