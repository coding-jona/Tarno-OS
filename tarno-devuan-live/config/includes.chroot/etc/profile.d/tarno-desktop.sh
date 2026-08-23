#!/bin/sh
# start labwc on login - same "startx from .profile" trick used for X
# for decades, just launching a wayland compositor instead.
#
# Logs to /tmp/tarno-desktop.log UNCONDITIONALLY, before anything else
# runs - a fixed path that never depends on XDG_RUNTIME_DIR existing or
# labwc getting anywhere at all. Whatever happens here, `cat
# /tmp/tarno-desktop.log` always has the answer - no more guessing
# whether this block even ran.
{
	printf '%s tty=%s WAYLAND_DISPLAY=[%s] DISPLAY=[%s]\n' \
		"$(date '+%F %T')" "$(tty 2>&1)" "$WAYLAND_DISPLAY" "$DISPLAY"
} >>/tmp/tarno-desktop.log 2>&1

# ~/Desktop, ~/Downloads etc. don't exist on a fresh live account -
# nothing ever created them (no systemd/GTK/KDE session doing the usual
# XDG-autostart dance, and labwc's own autostart is a plain script we
# write ourselves, not an XDG-autostart-.desktop processor). pcmanfm-qt's
# sidebar bookmarks point at them anyway, so browsing there hit "No such
# file or directory". Idempotent, cheap, runs on every login regardless
# of tty/graphical session state - same reasoning as tarno-earlysetup:
# don't gate a basic fixup on anything that could itself fail first.
xdg-user-dirs-update 2>/dev/null || true

# Only tty1-6 (any local virtual console) get a getty in this image
# (see 0200-agetty-console.chroot) - matching that instead of the exact
# string "/dev/tty1" so a boot-order/console-naming quirk can't just
# silently skip this whole block with zero trace.
case "$(tty 2>/dev/null)" in
	/dev/tty[1-6]) on_console=1 ;;
	*) on_console=0 ;;
esac

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ] && [ "$on_console" = 1 ]; then
	# NOT execed: this image has no systemd/elogind, so nothing creates
	# XDG_RUNTIME_DIR - and labwc's very first startup check is for that
	# var, printing "XDG_RUNTIME_DIR is unset" and exit(1)-ing before
	# doing anything else (confirmed locally: happens in under a
	# millisecond, no delay at all). A bare `exec labwc` here replaced
	# the login shell, so that instant crash just ended the whole
	# session and getty respawned the login prompt - on screen that's
	# indistinguishable from a ~0.1s flash, way too fast to read. Set
	# the var up by hand (the same thing pam_systemd would normally
	# do), force libseat straight at seatd instead of letting it try a
	# (non-existent) logind bus first, and run labwc as a plain child
	# with its output logged - so any *other* crash (e.g. no usable
	# DRM/KMS device) drops back to a shell with a log pointer instead
	# of silently bouncing back to login again.
	# /run/user/<uid> is the "proper" XDG location, but /run itself is
	# (correctly) root:root 0755 - a non-root process can never mkdir
	# under it without a privileged helper (that's what pam_systemd
	# does on a systemd box). Confirmed on a real boot: "Permission
	# denied" trying exactly that. This image has no such helper, so
	# fall back to a per-user directory under /tmp instead - the XDG
	# basedir spec's own documented fallback when no real session
	# manager sets XDG_RUNTIME_DIR up. Requires
	# tarno-earlysetup (etc/init.d/tarno-earlysetup) to have already
	# fixed /tmp's permissions, which it does before this ever runs.
	: "${XDG_RUNTIME_DIR:=/tmp/runtime-$(id -u)}"
	mkdir -p "$XDG_RUNTIME_DIR"
	chmod 700 "$XDG_RUNTIME_DIR"
	export XDG_RUNTIME_DIR
	export LIBSEAT_BACKEND=seatd

	echo "$(date '+%F %T') starting labwc, XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" >>/tmp/tarno-desktop.log
	labwc >"$XDG_RUNTIME_DIR/labwc.log" 2>&1
	status=$?
	echo "$(date '+%F %T') labwc exited (status $status)" >>/tmp/tarno-desktop.log
	echo "labwc exited (status $status) - see \$XDG_RUNTIME_DIR/labwc.log and /tmp/tarno-desktop.log"
else
	echo "$(date '+%F %T') skipped (on_console=$on_console, WAYLAND_DISPLAY=[$WAYLAND_DISPLAY], DISPLAY=[$DISPLAY])" >>/tmp/tarno-desktop.log
fi
