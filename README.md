# Tarno OS → THOS

> **⚠️ The project has pivoted.** Tarno OS is now **THOS**, a **clean-slate hybrid
> kernel** that runs native ELF/POSIX **and** native PE/Win32 programs side by side on
> one target machine — no Linux fork, no virtualization. Motivation: end the split
> between separate Windows and Linux environments.
>
> - Architecture — [`docs/thos/architecture.md`](docs/thos/architecture.md)
> - Roadmap & milestones — [`docs/thos/roadmap.md`](docs/thos/roadmap.md)
> - Honest feasibility — [`docs/thos/feasibility.md`](docs/thos/feasibility.md)
> - Target hardware — [`docs/thos/hw-target.md`](docs/thos/hw-target.md)
> - Licensing — [`docs/thos/licensing.md`](docs/thos/licensing.md)
>
> The Devuan-distro work is frozen ([`FROZEN.md`](FROZEN.md)); its roadmap is at
> [`docs/legacy-roadmap-devuan.md`](docs/legacy-roadmap-devuan.md).

## THOS quickstart

```sh
make toolchain              # rustup target + rust-src + llvm-tools
git submodule update --init # third_party/limine (v9.x-binary, pinned in .gitmodules)
make -C third_party/limine  # build the limine host tool
make run                    # build kernel + ISO, boot in QEMU, serial on stdout
```

Milestone 0: the kernel comes up under Limine, writes to COM1 **and** paints the GOP
framebuffer, then halts. Milestone 1a: it ingests the Limine memory map, stands up the
physical frame allocator and a bootstrap heap. CI:
[`.github/workflows/qemu-boot.yml`](.github/workflows/qemu-boot.yml).

---

## Frozen: the Devuan distribution

<details><summary>Previous Tarno OS — a Devuan-based Linux distro. Frozen, kept in the repo.</summary>

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
`tarno-disk-install` - now actually shipped in the image (`sudo
tarno-disk-install` lists internal disks, `sudo tarno-disk-install sda`
installs; also reachable from the desktop, see "Desktop" below). It
wasn't before: the binary was never built into the ISO, none of its
dependencies (`parted`, `dosfstools`, `e2fsprogs`, `rsync`,
`grub-efi-amd64`, `grub-pc-bin`) were in the package list, so despite
this same README paragraph, there was no way to actually install to
disk from a live session - confirmed missing on a real boot.

First real install hung at "boot from disk" - the installer only ever
put down an EFI bootloader, invisible to BIOS/legacy firmware (which is
what every VM/machine in this whole session has actually been using -
same reason the live ISO needs ISOLINUX at all). Fixed: the partition
table now always includes a `bios_grub` partition alongside the ESP
(GPT+BIOS booting needs one to embed into, there's no MBR-style gap to
reuse), and the installer detects the current firmware
(`/sys/firmware/efi` present or not) and runs the matching
`grub-install --target=i386-pc` or `--target=x86_64-efi` accordingly -
so the same install works on whichever firmware is actually booting it.
Verified locally: the full partition table (including the `bios_grub`
flag) and a real `grub-install --target=i386-pc` against it both
succeed against a loopback device. The rsync + chroot + full boot cycle
still needs a real machine to fully confirm end to end (needs the live
system's own root, not reproducible in a dev sandbox).

## Automated boot test

`.github/workflows/build-devuan-image.yml` has a second job,
`qemu-smoke-test`, that runs after every build: boots the produced ISO
in QEMU and fails the workflow if it doesn't reach a real working
desktop. This exists because a recurring line in this same README used
to be "a VM test never hit this" - several real bugs (dhcpcd vs.
live-boot's network reset, seatd/video-group crashing labwc back to a
shell) only ever surfaced on someone's actual laptop, not because VMs
can't reproduce them, but because nothing was actually driving a VM
boot to completion and inspecting the result.

`scripts/qemu-smoke-test.py` is what does that: a real virtual GPU
(`-vga std`, `-display none` just means QEMU doesn't pop a window on
the CI runner - the guest still gets a real graphics device to drive
via DRM/KMS, same as real hardware) plus a second, independent serial
console (`ttyS0`, autologin, see `0200-agetty-console.chroot`) the
script drives directly - reachable regardless of whatever labwc/seatd
are doing on the "real" tty1/monitor console, so a crash there is
something this script can observe and report instead of a silent
black `-display none` screen. It asserts the same things this project
has always manually checked by hand on real hardware: `/tmp/
tarno-desktop.log` has no `Permission denied` and no `labwc exited`,
`rc-status default` shows `seatd`/`dhcpcd`/`tarno-earlysetup`/`tarnod`
all `started`, `user` is actually in the `video` group,
`/etc/network/interfaces` is still loopback-only, and `labwc`/`waybar`
are both still running processes at the end of boot - not just "did
QEMU exit 0".

Best-effort KVM acceleration (`-accel kvm:tcg`, GitHub-hosted runners
have had `/dev/kvm` since ~2023, just not group-writable by default -
the workflow fixes that the same way the Android-emulator CI action
ecosystem does), falling back to plain software emulation (slower,
still functionally correct - these are boot/config bugs, not
performance-dependent ones) if that doesn't pan out.

Verified locally: the actual command-parsing mechanics
(`run_cmd`/`wait_for_shell` in `qemu-smoke-test.py`) run against a
real pty (`pexpect.spawn("/bin/bash", ...)`, not a mock) - caught and
fixed two real bugs this way before they could produce a silently
wrong CI result: bash's bracketed-paste-mode escape codes breaking the
naive first-line-strip output parsing, and `rc-status default`'s
actual output format (a leading `*` bullet before each service name)
not matching the original regex. The `rc-status`/`tarno-desktop.log`/
`pgrep` assertions themselves checked against realistic sample text
built from this project's own prior real-hardware findings. **Not**
run end-to-end against the real ISO - this sandbox has no `/dev/kvm`,
no `qemu-system-x86_64` installed, and its network policy blocks
`deb.devuan.org` (same class of restriction as `flathub.org`/
`codeberg.org` elsewhere in this README), so an actual `lb build` +
QEMU boot isn't possible here. The next CI run of this workflow is the
first real end-to-end test.

## Updates

Tarno OS ships its own tools as a normal apt repo instead of a custom
updater - `scripts/build-deb.sh` builds `tarno-tools.deb` (tarno-install
+ tarno-disk-install, the install-media tools), `scripts/build-apt-repo.sh`
turns a folder of `.deb`s into a flat apt repo, `.github/workflows/apt-repo.yml`
publishes it to GitHub Pages on every push to `devel`.

Until now that repo only ever carried those two install-media tools -
nothing of the OS itself, so an already-installed Tarno OS system had
no update path at all beyond "reinstall from a new ISO". Fixed:
`scripts/build-os-deb.sh` builds `tarno-os.deb` - literally the whole
OS layer (tarnod, the labwc/waybar desktop, tarno-settings/
tarno-store/tarno-assistant, and all their OpenRC service wiring),
built from the exact same `tarno-devuan-live/config/includes.chroot/`
tree the live ISO ships, so there's one source of truth instead of two
things to keep in sync by hand. Its `postinst` re-runs the same hook
scripts (`tarno-devuan-live/config/hooks/*.chroot`) the ISO build
itself runs - they're already idempotent shell (`rc-update`, `chmod`,
`ln -sf`, `sed -i`), nothing chroot-specific, so they work identically
against a real installed system. Everything under `/etc` it ships is a
`conffile`, so a local edit survives an upgrade instead of getting
silently clobbered.

Deliberately *not* wired into the live/USB image's own `sources.list` -
a live session never persists across a reboot, so it has no use for an
update channel. `tarno-disk-install` writes
`/etc/apt/sources.list.d/tarno-os.list` into the disk-installed system
it produces instead, since that's the system that actually benefits.
The repo is still unsigned - no `APT_REPO_GPG_KEY` secret exists yet -
so this ships as `deb [trusted=yes] ...` for now (a commented-out
signed line sits right above it in the same file, ready to swap in the
moment the secret exists) - a deliberate, user-confirmed interim
tradeoff to make the channel actually work today (transport is already
TLS via GitHub Pages either way) rather than leaving it blocked
indefinitely on a manual step, not something done silently.

Verified locally: `build-deb.sh` + `build-os-deb.sh` +
`build-apt-repo.sh` run together, producing a real two-package repo
(`dpkg-scanpackages` correctly indexes both). `tarno-os.deb`'s contents
inspected with `dpkg-deb -c`/`-e` - correct file tree, ownership,
permissions, `conffiles`, and a working `postinst` (all 6 hook copies
present in the right order, valid shell syntax). The
`tarno-disk-install` apt-source write tested in isolation - correct
path, content, and `0644` permissions, and the resulting one-line
`sources.list` entry parses cleanly with a real `apt-get`. Not run
through an actual `dpkg -i`/`apt install` - no OpenRC/Devuan host in
this sandbox to exercise `rc-update` and friends for real, so the
postinst hasn't been confirmed end to end yet.

## tarnod

The root daemon (this package, `main.go`) listens on a Unix socket
(`/run/tarnod.sock`, JSON in/out) and starts on boot via the OpenRC
service in `tarno-devuan-live/config/includes.chroot/etc/init.d/tarnod`,
enabled by `0450-tarnod-enable.chroot` (the init script alone was never
enough - same gap as agetty/dhcpcd/seatd, confirmed missing on a real
boot). Both the CI workflow and `scripts/build-devuan-live.sh` build it
(along with `tarnoctl` - previously only ever existed as source you'd
have to build yourself, same class of gap `tarno-disk-install` used to
have) and drop them into `config/includes.chroot/usr/bin/` before
`lb build` runs.

    go run . &
    go run ./cmd/tarnoctl status
    MISTRAL_API_KEY=... go run . &          # env var still works...
    go run ./cmd/tarnoctl set-api-key sk-...  # ...or set it live, no restart
    go run ./cmd/tarnoctl ai-status
    go run ./cmd/tarnoctl ai "some question"

The API key can be set two ways now: the `MISTRAL_API_KEY` env var (as
before), or persisted at `/etc/tarnod/mistral_api_key` (`0600`,
root-owned) via the new `set_api_key` socket command - which is what
`tarno-settings`' AI tab and `tarnoctl set-api-key` actually use. tarnod
checks the env var first, falls back to that file on startup, and
`set_api_key` swaps the live provider in immediately (a `sync.RWMutex`
around it) - no restart needed after saving a key from the GUI. Modeled
on (not copied from - it's Python/Windows, this is Go/Linux) how
[`coding-jona/tarno`](https://github.com/coding-jona/tarno) resolves
API keys through a `SecretsVault` instead of reading them raw in
provider code; see `docs/tarno-ai-roadmap.md` for the full comparison
and what's still ahead. Verified locally: a real `tarnod` + `tarnoctl`
round trip (`ai-status` → `not configured`, `set-api-key`, `ai-status`
→ `configured`, and again after a full process restart with no env var
set, confirming the file actually persists it).

The socket is `chmod 0666` right after creation - tarnod runs as root,
so without this it inherits the process umask (typically `0755`), and
Unix-socket `connect()` needs *write* permission, meaning any non-root
client (`tarnoctl`, `tarno-settings`, both run as the live user) got
`EACCES`. Confirmed on a real boot ("tarnod unreachable: [Errno 13]
Permission denied") and fixed locally (verified: a non-root user can
now connect and get a real response). Fine for a single-user image with
root permanently locked - world-writable is the point, not a leftover
mistake.

## Desktop

Minimal Wayland desktop: [labwc](https://labwc.github.io/) (an
Openbox-alike wlroots compositor with a real right-click root menu,
config in `tarno-devuan-live/config/includes.chroot/etc/xdg/labwc/`) +
[waybar](https://github.com/Alexays/Waybar) as a taskbar (window list via
its `wlr/taskbar` module - needs nothing labwc-specific, just the
`wlr-foreign-toplevel-management` protocol every wlroots compositor
implements - plus a clock and a launcher button for `tarno-settings`;
config in `tarno-devuan-live/config/includes.chroot/etc/xdg/waybar/`) +
`tarno-settings`, a small PySide6 panel
(`tarno-devuan-live/config/includes.chroot/usr/bin/tarno-settings`) that
talks to `tarnod` over its socket - a Status tab, a Network tab (wired
status + WiFi scan/connect/known-networks via `iwctl`, see "WiFi"
below), a System tab (hostname/kernel/uptime/memory/disk, read-only),
an AI tab (Mistral API key setup + a quick ask/answer box to
sanity-check it, see "tarnod" above), an Install tab wrapping
`tarno-disk-install` (also its own "Install to Disk" root-menu entry,
`tarno-install-to-disk` - an interactive terminal wrapper, since the
raw command needs a device name and rsync's progress needs a real tty,
neither of which fits a Qt widget), and a Power tab (reboot/shut
down). Starts automatically on tty1 login
(`/etc/profile.d/tarno-desktop.sh`). Theme is the old
`tarno-ui-theme` palette (cyan `#0BC7FF` on Fluent-style dark gray),
ported to an Openbox `themerc` and a Qt stylesheet - completely different
layout from the old deleted `tarno-desktop`/`tarnod-ui`, same colors.

First real boot test found labwc crashing the login session instantly
(no systemd/elogind means nothing set `XDG_RUNTIME_DIR`, and that's
labwc's very first startup check - see git history for the fix). Login
now drops into labwc without exec'ing it, so any *other* crash (e.g. no
usable DRM/KMS device on a given machine/VM) lands you back at a shell
instead of bouncing to login. `/etc/profile.d/tarno-desktop.sh` always
logs what happened to `/tmp/tarno-desktop.log` (whether it even ran,
what tty/WAYLAND_DISPLAY/DISPLAY it saw) plus `$XDG_RUNTIME_DIR/labwc.log`
for labwc's own output - no more guessing blind from a fast-flashing
console.

Second real boot test (with that fix in) found `/tmp` itself not
actually writable - this image's `init=/sbin/openrc-init` setup skips
the usual sysvinit/OpenRC boot chain that normally fixes `/tmp`
permissions (openrc's own `bootmisc`, not wired into this image's
runlevels), so it kept bare `mkdir` permissions. Fixed by a small
dedicated service, `etc/init.d/tarno-earlysetup` (enabled by
`0150-tarno-earlysetup-enable.chroot`), that runs before agetty and
just fixes `/tmp`/`/var/tmp` permissions. `XDG_RUNTIME_DIR` also moved
from `/run/user/<uid>` (root:root 0755 - a plain user process can never
mkdir there, confirmed) to `/tmp/runtime-<uid>`, the XDG basedir spec's
own documented fallback for when nothing sets it up properly.

Third real boot test (past both of the above): labwc actually started -
a plain black screen, which turned out to be success, not failure.
labwc, like Openbox on X11, paints no background of its own; without a
wallpaper client the whole output is just black and indistinguishable
from a hung/crashed session. Added `swaybg` (filling with the theme's
base `#202020`) to `etc/xdg/labwc/autostart`, so a working session
actually looks like one.

Fourth real boot test: waybar's taskbar showed every `tarno-settings`
window as "Python (v3.13)". `tarno-settings` runs as `python3
/usr/bin/tarno-settings` (the shebang invokes the interpreter, not a
compiled binary of that name), so without an explicit app_id, Qt's
wayland platform plugin fell back to identifying the window as the
Python interpreter itself. Fixed with `app.setDesktopFileName
("tarno-settings")` plus a matching
`/usr/share/applications/tarno-settings.desktop` (`Name=Tarno
Settings`) so waybar's `wlr/taskbar` resolves it to a real name instead
of the raw app_id.

Core apps: a file manager (`pcmanfm-qt`), a web browser
(`firefox-esr`), a text editor (`geany`), and an app launcher
(`fuzzel`, bound to `Super+Space`, reads `/usr/share/applications/*.desktop`
automatically - no separate config needed to list apps in it) - up to
this point there was nothing to browse files, get online, or edit a
document with at all, just labwc's menu, `tarno-settings`, and a bare
terminal. Also added `wl-clipboard`/`grim`/`slurp` (clipboard and
screenshot CLI tools) - baseline utilities every other wlroots desktop
ships that were simply missing here. Themed `fuzzel.ini` matches the
same palette as everything else.

The waybar "Tarno" button now opens `fuzzel` (a real start menu -
search-as-you-type over every installed app) instead of launching
`tarno-settings` directly; `tarno-settings` is still one search away,
or from the root menu.

`pcmanfm-qt` hit real bugs: "Operation not supported" / "No such file
or directory" doing basic file operations. Root-caused against its own
stock config and dependency list
(`/usr/share/pcmanfm-qt/lxqt/settings.conf`,
`apt-cache show pcmanfm-qt`): it defaults to a terminal and archiver
(`qterminal`, `lxqt-archiver`) that aren't in its own Depends/
Recommends and were never installed here, and defaults to
`UseTrash=true`, which is a known failure class on a live system's
overlay/union root (this image boots via live-boot's overlay over a
read-only squashfs) - GIO's trash implementation can throw exactly
"Operation not supported" there. Fixed via
`etc/skel/.config/pcmanfm-qt/lxqt/settings.conf` (copied into the live
user's home from `/etc/skel` at account creation, not a change to the
package's own file): `Terminal=foot`, `Archiver=xarchiver` (added),
`UseTrash=false` (permanent delete instead, same as most live distros'
file managers default to for this exact reason). Also added
`xdg-user-dirs` + a call to `xdg-user-dirs-update` in
`tarno-desktop.sh` - `~/Desktop`, `~/Downloads` etc. never existed
(nothing in this image was creating them), and pcmanfm-qt's sidebar
bookmarks point at them regardless.

`tarno-store` (`config/includes.chroot/usr/bin/tarno-store`, root menu
+ `/usr/share/applications/tarno-store.desktop`) - a small curated
catalog of ~20 real, well-known apps across categories a usable desktop
needs (Internet, Office, Graphics, Media, Development, System,
Utilities), install/remove with one click via plain `apt-get`
underneath (`QProcess`, streamed output, `DEBIAN_FRONTEND=noninteractive`,
`sudo -n` so it fails fast instead of hanging if that were ever not
passwordless). Not a package-repository browser - `synaptic` (also on
the root menu, `sudo -E synaptic` to keep the Wayland session env
across the privilege jump) is the real thing for that, searching all of
`sources.list`. Same design language as `tarno-settings` - card-style
rows, `Papirus` icons (added explicitly, not left to chance among
pcmanfm-qt's own OR'd icon-theme Recommends), a proper header, refined
color tokens shared between both apps. Verified locally: headless run
(`QT_QPA_PLATFORM=offscreen`) renders all 20 rows, `is_installed()`
correctly distinguishes installed vs. not (checked against real
`dpkg-query` state), search filtering works.

Second tab, "Flathub" - the curated list is deliberately small (~20
apps), so this is the actual "browse everything" complement: a search
box wired to the real `flatpak search --user
--columns=name,description,application <query>` (tab-separated fields,
confirmed against `flatpak`'s own C source - `flatpak-table-printer.c`
- since the exact format isn't documented anywhere and depends on
whether stdout is a TTY), triggered on Enter/button click rather than
live-filtered as you type since each search is a real subprocess call,
not a filter over data already in memory. Results render as the same
card-row look as the curated tab, Install/Remove driving `flatpak
install/uninstall --user -y --noninteractive` instead of `apt-get`.
`--user` (not `--system`) deliberately avoids needing polkit/root -
this image has no desktop polkit agent, same reasoning as `tarnod`'s
world-writable socket and the NOPASSWD sudoers drop-in. The `flathub`
remote itself is added with `--if-not-exists` from
`tarno-desktop.sh` on every login (per-account, so it can't be done
once at build time the way the apt sources list is) - and since
`flatpak`'s own `/etc/profile.d/flatpak.sh` (shipped by the `flatpak`
package itself) adds `--user`-installed apps' `.desktop` files to
`XDG_DATA_DIRS` automatically, `fuzzel` picks them up with zero extra
plumbing, same as every other app on this image.

Verified locally: the tab-separated parsing logic against sample
`flatpak search`/`flatpak list` output built from the real column
formats in `flatpak`'s own source, headless run instantiates both
tabs without crashing, `flatpak`'s own `/etc/profile.d/flatpak.sh`
inspected directly (installed `flatpak` locally to check) to confirm
the `XDG_DATA_DIRS` wiring. **Not verified**: an actual `flatpak
search`/`install` against the real Flathub remote - this sandbox's
network egress proxy blocks `flathub.org` outright (`403` on
`CONNECT`), so nothing here has run against Flathub's real catalog.
Real on-device confirmation is still needed.

`tarno-assistant` (`config/includes.chroot/usr/bin/tarno-assistant`,
root menu entry "Tarno", `Super+T`) - a real chat window for Tarno OS'
namesake feature instead of the single-line ask/answer box buried in a
settings tab, closer to what Cortana used to be for Windows (minus
voice, deliberately not part of this yet). Talks to the same `tarnod`
socket `tarno-settings`' AI tab does (`ai`/`ai_status`) - API keys are
still only ever configured in Settings, never typed into the chat
itself, matching the same rule the project this is modeled on
([`coding-jona/tarno`](https://github.com/coding-jona/tarno), see
`docs/tarno-ai-roadmap.md`) follows for its own chat surface. Shows a
clear "not configured, go set a key" message and disables the input
instead of silently failing when no key is set yet. Verified locally:
headless run (`QT_QPA_PLATFORM=offscreen`) against a real `tarnod`
instance, both states (no key set -> disabled input + guidance message;
key set via a real `set_api_key` call -> "ready", input enabled).

No animations anywhere on purpose - they cost real frame time for zero
functional benefit, especially on the kind of aging hardware this has
actually been tested on. `labwc` has no animation system to begin with
(deliberately effect-free, unlike KWin/Mutter) and neither does
`fuzzel`, so there was nothing to turn off there. `gtk-enable-animations=false`
(`/etc/skel/.config/gtk-3.0/settings.ini` and the `gtk-4.0` twin, same
`/etc/skel` override mechanism as pcmanfm-qt's `settings.conf`) covers
every GTK3/4 app's menu/tooltip/tab fades - Geany, GTK dialogs, and
`waybar` itself (it's a GTK app under the hood, same setting reaches
its own tooltip fade-in). Firefox's chrome/content animations
(`toolkit.cosmeticAnimations.enabled`, `general.smoothScroll`,
`browser.tabs.animate`, `browser.fullscreen.animate`) are set via a
distribution policy file (`/usr/lib/firefox-esr/distribution/policies.json`,
Mozilla's own binary-relative lookup path, not distro-specific) as
non-locked defaults - the user can still flip them back in
`about:config` if they want, but a fresh install starts snappy. Qt apps
(`tarno-settings`, `tarno-store`) never used any animation API to begin
with, nothing to disable.

`docs/tarno-ai-roadmap.md` lays out where "Tarno" (the AI, not just the
`tarnod` binary) is actually headed - a real system assistant, not just
a Q&A tab - based on a from-scratch architectural survey of
[`coding-jona/tarno`](https://github.com/coding-jona/tarno) (the
namesake project this whole OS is meant to eventually host a version
of): what pattern maps directly onto `tarnod`'s Go/Unix-socket setup,
what's a genuinely separate future phase (tool-use with a real
permission layer, long-term memory), and what's explicitly deferred
(voice, the "Tarno Mesh" multi-device/account system - both by the
user's own choice, not forgotten).

Sixth real boot test (real hardware this time, not a VM - a QEMU boot
had never hit this): black screen, then dropped into a shell in a
rapid respawn loop, `tarno-desktop.sh`'s own log showing
`tty=/dev/console` on every line instead of `/dev/tty1` - so its
`tty[1-6]` check never matched and labwc never even got a chance to
start.
Root-caused via `cat /etc/inittab` and `cat /proc/1/cmdline` on the
actual machine: Devuan's stock `/etc/inittab` still ships its own
`1:2345:respawn:/sbin/getty --noclear 38400 tty1` entry, and
`openrc-init` (confirmed as PID 1) is inittab-compatible and honors
it - completely independent of, and fighting over the same VT with,
the `agetty.tty1` OpenRC service this image adds for autologin. A VM
test apparently never surfaced the race, presumably just different
console/tty wiring under QEMU. Fixed by commenting out just that one
`tty1` inittab line in the same hook that adds `agetty.tty1`
(`0200-agetty-console.chroot`) - `tty2`-`tty6` are left alone, they're
the documented way to get a manual login on another console.

Seventh real boot test: the whole desktop rendered squashed into a
square aspect ratio, on both the live session and a disk-installed
system - not something any config in this image was ever setting
(confirmed: nothing here specifies a resolution/mode anywhere), and
labwc itself has no way to force a specific output mode on its own -
its own manual explicitly says so, pointing at `wlr-randr`/`kanshi`
instead. Whatever labwc's own auto-enable picked on this hardware
wasn't the panel's actual preferred mode. Fixed by reading the mode
`wlr-randr` itself reports as `(preferred)` straight from the
connector and forcing exactly that at the top of
`etc/xdg/labwc/autostart`, before anything else starts - fixes this on
whatever hardware it runs on without hardcoding a resolution number
that would be wrong on different hardware, and is a no-op if the
preferred mode was already active. New `wlr-randr` package dependency.
Verified locally: the parsing logic against realistic sample
`wlr-randr` output (including a case where the active mode differs
from the preferred one - the actual bug class - and a multi-output
case) - correctly extracts and would force the right mode in both.
Not run against real hardware yet.

Eighth real boot test: `dhcpcd` refused to start again ("... defines
some interfaces that will use a DHCP client"), the exact failure class
already fixed once by overriding `etc/network/interfaces` to loopback-
only. `cat /etc/network/interfaces` on the actual booted system showed
the stock `allow-hotplug eth0` / `iface eth0 inet dhcp` content -
confirmed the file baked into the image is still correct (`git show`
against the actual built commit matches the loopback-only source
exactly), so something at *boot time* was overwriting it before
`dhcpcd` ever got to check. live-config's own runtime network
component does exactly this - regenerates a stock DHCP-ready
`interfaces` file on every boot as a "just works" default - and runs
before `openrc-init`/any of this image's own services even start, so
no ordering trick within this image's own runlevels can beat it; it
has to be actively reverted instead. Fixed by having
`tarno-earlysetup` (already the earliest thing this image runs, see
above) force-write it back to loopback-only every boot, right after
fixing `/tmp`'s permissions - same "don't fight the timing, just win
the last word" approach. Verified locally: the exact heredoc logic
in isolation, and that its output does *not* match `dhcpcd`'s own
documented sanity-check pattern (confirmed against the literal grep
expression from `/etc/init.d/dhcpcd`, so `dhcpcd` would actually start
against this content).

Ninth: real hardware repeatedly landed at a plain shell prompt instead
of the desktop, no crash message visible - the actual symptom being
"the OS just drops me into a shell". Root-caused by reading the real
`live-config`, `user-setup`, and `seatd` packages rather than
guessing: `live-config`'s own `0030-user-setup` component explicitly
overrides `user-setup`'s default group list with an *empty* string
(`debconf-set-selections passwd/user-default-groups`) unless a
`live-config.user-default-groups=` kernel cmdline parameter says
otherwise, which nothing in this image sets - so the live `user`
account ends up in zero supplementary groups, not even
`user-setup`'s own (already `video`-less) built-in default. Devuan's
packaged `/etc/init.d/seatd` runs `seatd -g video` (`/etc/default/
seatd`), which makes the seatd socket owned by the `video` group -
without membership, `labwc`'s `libseat` backend can't connect to it
at all, `labwc` exits immediately, and since `tarno-desktop.sh`
deliberately doesn't `exec labwc` (so a crash is visible instead of
silently bouncing back to login), that exit drops straight back into
the interactive login shell - exactly the reported symptom. A
kernel-cmdline fix wouldn't reach a disk-installed system either
(`tarno-disk-install`'s `GRUB_CMDLINE_LINUX_DEFAULT` has no
`boot=live`, so `live-config` - and any group list it'd assign -
never runs again there); fixed instead with `usermod -aG video user`
in `tarno-earlysetup`, every boot, same "don't fight the timing, just
win the last word" approach as the two fixups above - this way both
the live USB and anything already installed to disk self-heal on
their next boot. Verified locally: the `usermod -aG video` logic
tested in isolation against a real throwaway user account (starts in
no supplementary groups, ends in exactly `video` after one run,
idempotent on a second run). Not run against a real `seatd`/`labwc`
pair - no such stack in this sandbox to exercise the actual socket
connection end to end.

Tenth: the Eighth and Ninth fixes above both worked by winning a race
every boot - actively reverting whatever a third-party live-boot/
live-config component had just done. Asked to stop patching around
that and remove what's actually getting in the way instead, so both
got fixed at the root:

- The `/etc/network/interfaces` reset isn't `live-config` at all - it's
  `live-boot`'s own `9990-netbase.sh`, which runs from the initramfs
  (before `openrc-init`/anything in this image ever starts) and
  *unconditionally* overwrites the file, confirmed against its actual
  source. It has a documented escape hatch for exactly this: `if
  [ "${STATICIP}" = "frommedia" ] && [ -e "${IFFILE}" ]; then ...
  return; fi` - leave an already-existing file alone. Now set via
  `STATICIP=frommedia` in `auto/config`'s `--bootappend-live`, so
  live-boot never touches the file in the first place.
- The empty-groups bug came from `live-config`'s `0030-user-setup`
  always starting `user` from zero supplementary groups. Its own
  `Config()` already skips itself entirely if the account it wants to
  create already exists (`grep -q "^${LIVE_USERNAME}:" /etc/passwd`) -
  so new hook `0175-user-account.chroot` creates `user` at *build*
  time instead, with the right groups (`video`, `audio`, `cdrom`,
  `dialout`, `plugdev`) from the start, no cmdline flag or
  component-disabling needed, just winning the race by already being
  done before live-config ever checks.

`tarno-earlysetup`'s two reverts from the Eighth/Ninth fixes stay in
place regardless, as zero-cost safety nets in case either escape hatch
ever behaves differently on some Devuan version - but neither should
have anything left to do. Verified locally: `live-boot`'s and
`live-config`'s actual packaged source (`9990-netbase.sh`,
`0030-user-setup`) read directly to confirm both guards exist and
behave as described; the `0175-user-account.chroot` logic (`adduser`
+ `usermod -aG` + `passwd -l`) run in isolation against a real
throwaway account - correct groups, correct locked-password state
(`!` in `/etc/shadow`), idempotent on a second run. Not run through an
actual `lb build` + boot - no OpenRC/live-boot host in this sandbox to
confirm `STATICIP=frommedia` and the build-time account both behave
identically on Devuan's exact packaged versions.

## WiFi

[iwd](https://iwd.wiki.kernel.org/) instead of wpa_supplicant + a
separate tool - one self-contained daemon with its own `iwctl` client,
no `wpa_supplicant.conf` to hand-edit, and it only does L2 association
(dhcpcd still handles DHCP for every interface it finds, wireless
included - same "probes everything" behavior that already covered
wired). Enabled via `0500-iwd-enable.chroot`. Actual chip firmware
(iwlwifi/realtek/atheros/brcm80211, plus a `firmware-misc-nonfree`
catch-all) comes from the `non-free-firmware` archive area, newly
enabled in `auto/config` (Devuan mirrors Debian's post-bookworm
component split - it wasn't enabled before, so none of this firmware
was ever reachable regardless of the package list).

`tarno-settings`' Network tab drives `iwctl` directly: scan, a
double-click-to-connect list (prompts for a password on secured
networks), known-networks with a forget button, and the live wired/
WiFi status. Parses `iwctl`'s human-readable table output the same way
`tarno-store` shells out to `apt-get` - not a stable API, but the only
one that exists without pulling in a D-Bus binding dependency for one
feature. Verified locally as far as the sandbox allows: the parsing
helpers against real sample `iwctl` output, `system_info()`/
`wired_status()` against this sandbox's own `/proc`, `/sys`, `ip` -
**not** run against a real iwd instance or real WiFi hardware (none
available here), so real on-device confirmation is still needed.

## Login

There isn't one - tty1 autologs in as `user` (agetty `--autologin`, see
`tarno-devuan-live/config/hooks/0200-agetty-console.chroot`), same as
every mainstream live image. The account itself is created at *build*
time by `0175-user-account.chroot` (`adduser` + a fixed group list,
locked password - see the Tenth real boot test above for why this
moved off `user-setup`'s own runtime account creation); root is always
locked. Log in as `user` by hand only if you `chvt` to another console.

Passwordless sudo specifically comes from `etc/sudoers.d/tarno-user`
(`user ALL=(ALL) NOPASSWD: ALL`, chmod'd to the `0440` sudo insists on
by `0250-sudoers-perms.chroot` - git can't represent that exact mode on
its own). This README claimed passwordless sudo existed since early in
this project, but nothing ever actually configured it - `sudo` group
membership alone still requires typing your own password. Every place
in this image that shells out with `sudo -n` (non-interactive, so it
fails outright instead of hanging on a password prompt nobody can
answer) was silently failing as a result: `tarno-store`'s Install/
Remove buttons, `tarno-settings`' Power tab (reboot/shut down) and its
Install tab. Confirmed on a real boot (reported as "the Install button
doesn't work" and "can't shut down or restart").

## Status

Wired DHCP needed a fix before it worked at all: Debian/Devuan's
default `/etc/network/interfaces` ships its own `iface ... inet dhcp`
stanza, which made dhcpcd's own init script refuse to start at all
(avoiding a conflict with two DHCP clients on the same interface).
Overridden to loopback-only (`config/includes.chroot/etc/network/interfaces`)
since dhcpcd, not ifupdown, manages every interface in this image.
WiFi is wired up now too, see "WiFi" above. See `ROADMAP.md`.

</details>
