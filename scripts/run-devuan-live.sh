#!/bin/sh
# Boots the built live ISO in QEMU, for testing without a spare machine
# or USB stick. Needs qemu-system-x86_64.
set -eu

image="$(find "$(dirname "$0")/../tarno-devuan-live" -maxdepth 1 -name '*.iso' | head -n1)"
if [ -z "${image}" ]; then
	echo "no .iso in tarno-devuan-live/, build one first: scripts/build-devuan-live.sh" >&2
	exit 1
fi

exec qemu-system-x86_64 \
	-machine accel=kvm:tcg \
	-m 2048 \
	-net nic -net user \
	-cdrom "${image}" \
	-boot d
