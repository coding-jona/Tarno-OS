#!/bin/sh
# Builds tarno-tools.deb (tarno-install + tarno-disk-install) from source.
# VERSION defaults to a git-sha dev version if not set.
set -eu

VERSION="${VERSION:-0.0.0+g$(git rev-parse --short HEAD)}"
ARCH="${ARCH:-amd64}"
OUT="${OUT:-.}"

mkdir -p "$OUT"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/DEBIAN" "$root/usr/bin"

cat > "$root/DEBIAN/control" <<EOF
Package: tarno-tools
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Tarno OS <noreply@tarno-os.invalid>
Description: USB stick writer and disk installer for Tarno OS
 tarno-install writes a Tarno OS image to a USB stick, tarno-disk-install
 installs a running live session onto an internal disk.
EOF

GOOS=linux GOARCH="${ARCH}" go build -o "$root/usr/bin/tarno-install" ./cmd/tarno-install
GOOS=linux GOARCH="${ARCH}" go build -o "$root/usr/bin/tarno-disk-install" ./cmd/tarno-disk-install

dpkg-deb --build --root-owner-group "$root" "${OUT}/tarno-tools_${VERSION}_${ARCH}.deb"
