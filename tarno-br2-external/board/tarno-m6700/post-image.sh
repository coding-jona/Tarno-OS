#!/bin/sh
# Buildroot-Post-Image-Hook (BR2_ROOTFS_POST_IMAGE_SCRIPT, siehe
# configs/tarno_m6700_defconfig): baut aus Kernel + Rootfs das finale
# USB-Boot-Image (output/images/sdcard.img) via genimage. Standard-
# Buildroot-Boilerplate für dieses Muster (analog zu den in-tree
# board/*/post-image.sh-Referenzbeispielen von Buildroot selbst).
set -e

BOARD_DIR="$(dirname "$0")"
GENIMAGE_CFG="${BOARD_DIR}/genimage.cfg"
GENIMAGE_TMP="${BUILD_DIR}/genimage.tmp"

rm -rf "${GENIMAGE_TMP}"

genimage \
	--rootpath "${TARGET_DIR}" \
	--tmppath "${GENIMAGE_TMP}" \
	--inputpath "${BINARIES_DIR}" \
	--outputpath "${BINARIES_DIR}" \
	--config "${GENIMAGE_CFG}"

# MBR-Bootstrap-Code (erste 440 Byte) von host-syslinux in das fertige Image
# schreiben, damit das BIOS überhaupt zur SYSLINUX-Boot-Partition springt.
# conv=notrunc: nur die Boot-Code-Bytes überschreiben, Partitionstabelle
# (ab Offset 446) bleibt unangetastet. Siehe TODO in genimage.cfg — ggf.
# durch genimage-eigene MBR-Unterstützung ersetzbar, sobald verifiziert.
if [ -f "${HOST_DIR}/usr/share/syslinux/mbr.bin" ]; then
	dd if="${HOST_DIR}/usr/share/syslinux/mbr.bin" \
	   of="${BINARIES_DIR}/sdcard.img" \
	   conv=notrunc
else
	echo "warnung: ${HOST_DIR}/usr/share/syslinux/mbr.bin nicht gefunden - Image ist ohne MBR-Bootstrap-Code evtl. nicht bootfaehig" >&2
fi

exit 0
