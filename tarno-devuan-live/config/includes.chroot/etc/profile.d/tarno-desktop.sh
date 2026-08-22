#!/bin/sh
# start labwc on tty1 login - same "startx from .profile" trick used for
# X for decades, just execing a wayland compositor instead.
if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ] && [ "$(tty)" = "/dev/tty1" ]; then
	exec labwc
fi
