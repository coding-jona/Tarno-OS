#!/bin/sh
# start labwc on tty1 login - same "startx from .profile" trick used for
# X for decades, just launching a wayland compositor instead.
#
# NOT execed: this image has no systemd/elogind, so nothing creates
# XDG_RUNTIME_DIR - and labwc's very first startup check is for that
# var, printing "XDG_RUNTIME_DIR is unset" and exit(1)-ing before doing
# anything else (confirmed locally: happens in under a millisecond, no
# delay at all). A bare `exec labwc` here replaced the login shell, so
# that instant crash just ended the whole session and getty respawned
# the login prompt - on screen that's indistinguishable from a ~0.1s
# flash, way too fast to read. Set the var up by hand (the same thing
# pam_systemd would normally do), force libseat straight at seatd
# instead of letting it try a (non-existent) logind bus first, and run
# labwc as a plain child with its output logged - so any *other* crash
# (e.g. no usable DRM/KMS device) drops back to a shell with a log
# pointer instead of silently bouncing back to login again.
if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ] && [ "$(tty)" = "/dev/tty1" ]; then
	: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
	mkdir -p "$XDG_RUNTIME_DIR"
	chmod 700 "$XDG_RUNTIME_DIR"
	export XDG_RUNTIME_DIR
	export LIBSEAT_BACKEND=seatd

	labwc >"$XDG_RUNTIME_DIR/labwc.log" 2>&1
	echo "labwc exited (status $?) - see \$XDG_RUNTIME_DIR/labwc.log"
fi
