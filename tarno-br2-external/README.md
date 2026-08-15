# tarno-br2-external

`BR2_EXTERNAL`-Tree für Tarno OS: Buildroot-Package für `tarnod` und
Board-Support für den Dell Precision M6700.

**Scope-Hinweis:** Diese Dateien wurden nach aktueller Buildroot-Manual-
Syntax geschrieben, aber in der Entwicklungs-Sandbox **nicht gegen einen
echten Buildroot-Checkout gebaut** (mehrere GB Downloads, Stunden Bauzeit,
reale Zielhardware zum Booten — siehe
[`../docs/month1-foundation.md`](../docs/month1-foundation.md#scope-hinweis-für-diese-sandbox)).
Was hier real getestet ist: die `tarnod`- und `tarno-guard-ebpf`-Crates
selbst (siehe [`../tarnod/`](../tarnod/) und
[`../tarno-guard-ebpf/`](../tarno-guard-ebpf/)).

## Verwendung

```sh
git clone https://github.com/buildroot/buildroot.git
cd buildroot
make BR2_EXTERNAL=/pfad/zu/Tarno-OS/tarno-br2-external tarno_m6700_defconfig
make menuconfig   # zur Kontrolle/Anpassung an die tatsächliche Hardware
make              # Ergebnis: output/images/sdcard.img
```

**Auf einen USB-Stick schreiben und booten** (Anforderung: Installation
ohne separaten Installer, siehe
[`../docs/month1-foundation.md`](../docs/month1-foundation.md#usb-boot-image-anforderung-installationauslieferung-per-usb-stick)):

```sh
sudo dd if=output/images/sdcard.img of=/dev/sdX bs=4M status=progress conv=fsync
```

(`/dev/sdX` durch den tatsächlichen USB-Stick-Gerätepfad ersetzen — nicht
eine bestehende Festplatte, `dd` überschreibt ohne Rückfrage.) Stick in den
M6700, im BIOS-Bootmenü auswählen — bootet direkt über SYSLINUX.

## Struktur

| Pfad | Zweck |
|---|---|
| `external.desc`, `external.mk`, `Config.in` | Buildroot-`BR2_EXTERNAL`-Boilerplate |
| `package/tarnod/` | Buildroot-Package (cargo-package-Infra) für `tarnod`/`tarnoctl` |
| `board/tarno-m6700/linux.config.fragment` | Kernel-Config-Fragment (nur M6700-Hardware) |
| `board/tarno-m6700/rootfs-overlay/` | OpenRC-Service für `tarnod` |
| `board/tarno-m6700/genimage.cfg`, `syslinux.cfg`, `post-image.sh` | USB-Boot-Image (SYSLINUX/BIOS, per `dd` auf Stick schreibbar) |
| `configs/tarno_m6700_defconfig` | Buildroot-defconfig |

Details/Begründungen: [`../docs/architecture.md`](../docs/architecture.md),
[`../docs/month1-foundation.md`](../docs/month1-foundation.md).
