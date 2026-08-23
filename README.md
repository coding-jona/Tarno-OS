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

## Updates

Tarno OS ships its own tools as a normal apt repo instead of a custom
updater - `scripts/build-deb.sh` builds `tarno-tools.deb`
(tarno-install + tarno-disk-install), `scripts/build-apt-repo.sh` turns
a folder of `.deb`s into a flat apt repo, `.github/workflows/apt-repo.yml`
publishes it to GitHub Pages on every push to `devel`. Not yet wired into
the live image's `sources.list` - the repo is unsigned until an
`APT_REPO_GPG_KEY` secret exists (`[trusted=yes]` by default otherwise,
not shipping that silently).

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
talks to `tarnod` over its socket - a Status tab, an AI tab (Mistral API
key setup + a quick ask/answer box to sanity-check it, see "tarnod"
above), plus an Install tab wrapping
`tarno-disk-install` (also its own "Install to Disk" root-menu entry,
`tarno-install-to-disk` - an interactive terminal wrapper, since the
raw command needs a device name and rsync's progress needs a real tty,
neither of which fits a Qt widget). Starts automatically on
tty1 login (`/etc/profile.d/tarno-desktop.sh`). Theme is the old
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

## Login

There isn't one - tty1 autologs in as `user` (agetty `--autologin`, see
`tarno-devuan-live/config/hooks/0200-agetty-console.chroot`), same as
every mainstream live image. `user-setup` + `sudo` still create the
account and give it passwordless `sudo` (see
`tarno-devuan-live/README.md`); root is always locked. Log in as `user`
by hand only if you `chvt` to another console.

## Status

Wifi isn't wired up, only wired DHCP - and even that needed a fix:
Debian/Devuan's default `/etc/network/interfaces` ships its own
`iface ... inet dhcp` stanza, which made dhcpcd's own init script
refuse to start at all (avoiding a conflict with two DHCP clients on
the same interface). Overridden to loopback-only
(`config/includes.chroot/etc/network/interfaces`) since dhcpcd, not
ifupdown, manages every interface in this image. See `ROADMAP.md`.
