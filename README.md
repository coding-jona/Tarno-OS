# Tarno OS

A Devuan-based Linux distro with OpenRC instead of systemd, and `tarnod`
(this repo's Go daemon) doing gaming-mode tuning, eBPF security, and a
Mistral-backed assistant.

## Build the live ISO

On a Devuan machine:

    make devuan-iso

Wraps `scripts/build-devuan-live.sh` - installs `live-build` and runs
`lb build` against `tarno-devuan-live/`. Native debootstrap already knows
Devuan suite names, so unlike CI this needs no package swap.

## Test it

    make devuan-run

Boots the built ISO in QEMU (needs `qemu-system-x86_64`). For real
hardware, write it to a USB stick with `tarno-install`:

    go build -o tarno-install ./cmd/tarno-install
    sudo ./tarno-install tarno-devuan-live/*.iso sdX

or install it to an internal disk from a running live session with
`tarno-disk-install` (see `cmd/tarno-disk-install`).

## Updates

Tarno OS ships its own tools as a normal apt repo instead of a custom
updater - `scripts/build-deb.sh` builds `tarno-tools.deb`
(tarno-install + tarno-disk-install), `scripts/build-apt-repo.sh` turns
a folder of `.deb`s into a flat apt repo, `.github/workflows/apt-repo.yml`
publishes it to GitHub Pages on every push to `devel`. Not yet wired into
the live image's `sources.list` - the repo is unsigned until an
`APT_REPO_GPG_KEY` secret exists (`[trusted=yes]` by default otherwise,
not shipping that silently).

## Status

`tarnod` isn't packaged into the live image yet - the OpenRC service is
wired up but has nothing to run. Wifi isn't wired up either, only wired
DHCP. See `ROADMAP.md`.
