#!/bin/sh
# Builds the Devuan live ISO locally. Run this on a real Devuan machine
# (Devuan 13/excalibur) - native debootstrap already knows devuan suite
# names, so unlike the CI workflow this needs no debootstrap package swap.
set -eu

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

# built before the re-exec below, as the invoking user - so it picks up
# their normal go toolchain instead of root's (possibly missing) PATH
if ! command -v go >/dev/null 2>&1; then
	echo "no go toolchain found, install golang first" >&2
	exit 1
fi
mkdir -p "${repo_root}/tarno-devuan-live/config/includes.chroot/usr/bin"
( cd "${repo_root}" && GOOS=linux GOARCH=amd64 go build -o tarno-devuan-live/config/includes.chroot/usr/bin/tarnod . )
( cd "${repo_root}" && GOOS=linux GOARCH=amd64 go build -o tarno-devuan-live/config/includes.chroot/usr/bin/tarno-disk-install ./cmd/tarno-disk-install )
# tarnoctl was never actually built into the image before this - it
# only ever existed as source you'd have to build yourself, same class
# of gap as tarno-disk-install used to be. Useful on its own from a
# terminal (tarnoctl status/ai-status/set-api-key), and tarno-settings/
# tarno-assistant already talk to the same socket it does.
( cd "${repo_root}" && GOOS=linux GOARCH=amd64 go build -o tarno-devuan-live/config/includes.chroot/usr/bin/tarnoctl ./cmd/tarnoctl )

if [ "$(id -u)" -ne 0 ]; then
	if command -v sudo >/dev/null 2>&1; then
		exec sudo "$0" "$@"
	elif command -v doas >/dev/null 2>&1; then
		exec doas "$0" "$@"
	else
		echo "need root, and no sudo or doas found" >&2
		exit 1
	fi
fi

if ! command -v apt-get >/dev/null 2>&1; then
	echo "no apt-get found, this script is for a devuan/debian host" >&2
	exit 1
fi

apt-get update
apt-get install -y live-build squashfs-tools xorriso syslinux-utils

cd "$(dirname "$0")/../tarno-devuan-live"
lb clean
lb build

image="$(find . -maxdepth 1 -name '*.iso' | head -n1)"
if [ -z "${image}" ]; then
	echo "no .iso produced, check the build output above" >&2
	exit 1
fi
sha256sum "${image}"

echo
echo "built: tarno-devuan-live/${image#./}"
echo "boot it in a vm:      scripts/run-devuan-live.sh"
echo "write it to a stick:  go run ./cmd/tarno-install ${image#./} sdX   (as root, from repo root)"
