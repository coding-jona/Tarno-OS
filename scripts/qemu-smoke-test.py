#!/usr/bin/env python3
"""Boot the built Tarno OS ISO in QEMU and check it actually reaches a
working desktop - an automated stand-in for the manual real-hardware
testing this project has relied on so far.

Why this exists: several real bugs this project shipped (dhcpcd vs.
live-boot's network reset, seatd/video-group crashing labwc back to a
shell, /tmp permissions) only ever surfaced on a real laptop - "a VM
test never hit this" is a recurring line in this repo's own README.
That's not because VMs can't reproduce them; it's because nothing was
actually driving a VM boot to completion and inspecting the result.
This script does that for real:

  - A real virtual GPU (`-vga std`, not `-nographic`/`-vga none`) so
    DRM/KMS, seatd, and labwc get exercised exactly like they would on
    real hardware - `-display none` just means QEMU itself doesn't pop
    a window, the guest still sees a real graphics device it has to
    drive.
  - A *separate* serial console (ttyS0, autologin, see
    0200-agetty-console.chroot) this script drives directly - reachable
    regardless of whatever labwc/seatd are doing on the "real"
    tty1/monitor console, so a crash there is something this script can
    observe and report on instead of something that silently blanks
    the (non-existent, `-display none`) screen.
  - Real assertions against the same things this project has always
    manually grepped for by hand on real hardware: /tmp/tarno-desktop.log
    content, `rc-status`, group membership, and whether labwc/waybar
    are actually still running - not just "did QEMU exit 0".

Usage: qemu-smoke-test.py <path-to-iso>
Exit code 0 = desktop came up clean. Nonzero = a real assertion failed
(printed to stderr) or QEMU/pexpect itself errored out.
"""
import re
import shutil
import sys

try:
    import pexpect
except ImportError:
    print(
        "pexpect not installed - `pip install pexpect` first", file=sys.stderr
    )
    sys.exit(2)

BOOT_TIMEOUT = 2400  # pure TCG (no /dev/kvm, or /dev/kvm present but
# not actually usable - see has_kvm()'s own caveat) can be genuinely
# slow to get a whole live-boot + debconf + live-config + OpenRC +
# labwc chain up. A first real run against this exact ISO timed out
# completely at the previous value (900s) with zero serial output the
# entire time - not a single boot message, meaning the boot was still
# somewhere in that stack, not crashed (a crash or a real login prompt
# both produce visible output) - so this is now generous enough to
# actually distinguish "still booting under TCG" from "genuinely
# stuck", instead of just timing out on slow-but-fine hardware.
CMD_TIMEOUT = 60
SENTINEL = "TARNO_SMOKE_CMD_DONE"


def has_kvm():
    """Best-effort check - confirms /dev/kvm exists and this process can
    open it for read+write (what actually using it requires), not just
    read (opening read-only would report a node that's udev-owned
    root:root 0600 as "usable" when it isn't). Still not a guarantee:
    QEMU's own `accel=kvm:tcg` can fail to init KVM for other reasons
    (missing capability, nested-virt not actually enabled on the host)
    and silently fall back to tcg with no visible warning - this only
    rules out the one failure mode checkable from here without
    actually spawning QEMU."""
    try:
        with open("/dev/kvm", "r+b"):
            return True
    except OSError:
        return False


def qemu_cmd(iso_path):
    accel = "kvm:tcg" if has_kvm() else "tcg"
    return [
        "qemu-system-x86_64",
        "-machine",
        f"q35,accel={accel}",
        "-cpu",
        "max",
        "-m",
        "2048",
        "-smp",
        "2",
        "-vga",
        "std",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-monitor",
        "none",
        "-cdrom",
        iso_path,
        "-boot",
        "d",
        "-no-reboot",
    ]


ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]")


def run_cmd(child, cmd):
    """Run one shell command over the already-logged-in serial console,
    return its stdout. Uses a sentinel + exit code marker rather than
    trying to match a shell prompt regex - boot output is noisy enough
    that a bare `$` match would be unreliable.

    Terminal escape sequences (bash's bracketed-paste mode toggles
    showed up in real isolated testing against a real pty, not just a
    theoretical concern) get stripped, and the command's own echo is
    located by searching for its last occurrence rather than assuming
    it's exactly one fixed line - both real, observed failure modes of
    the naive "drop the first line" approach this replaced."""
    marker_cmd = f"{cmd}; echo {SENTINEL}:$?"
    child.sendline(marker_cmd)
    child.expect(rf"{SENTINEL}:(\d+)", timeout=CMD_TIMEOUT)
    exit_code = int(child.match.group(1))
    raw = ANSI_RE.sub("", child.before).replace("\r\n", "\n").replace("\r", "")
    idx = raw.rfind(marker_cmd)
    if idx != -1:
        raw = raw[idx + len(marker_cmd) :]
    return raw.strip("\n "), exit_code


def wait_for_shell(child):
    """The ttyS0 login (agetty --autologin user -> login -f user) can
    take a while to even start accepting input during early boot -
    retry sending a probe command instead of guessing a fixed delay."""
    deadline_attempts = BOOT_TIMEOUT // 10
    for attempt in range(deadline_attempts):
        if attempt % 6 == 0:  # once a minute - so a slow-but-fine boot
            # is visibly still-alive in the CI log instead of looking
            # identical to a stuck one until the final timeout fires
            print(f"... still waiting for a shell on ttyS0 ({attempt * 10}s elapsed)")
        child.sendline(f"echo {SENTINEL}:$?")
        try:
            child.expect(rf"{SENTINEL}:(\d+)", timeout=10)
            return
        except pexpect.TIMEOUT:
            continue
    raise RuntimeError(
        f"no shell prompt on ttyS0 after {BOOT_TIMEOUT}s - boot likely hung "
        "or crashed before login"
    )


def main():
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-iso>", file=sys.stderr)
        return 2
    iso_path = sys.argv[1]

    if shutil.which("qemu-system-x86_64") is None:
        print("qemu-system-x86_64 not found on PATH", file=sys.stderr)
        return 2

    failures = []

    kvm_usable = has_kvm()
    print(f"/dev/kvm usable (read+write): {kvm_usable}")
    if not kvm_usable:
        print(
            "no KVM acceleration - this boot runs under plain software "
            "emulation (TCG), which is slow but still functionally "
            "correct for the config/boot bugs this test checks for"
        )

    cmd = qemu_cmd(iso_path)
    print("+ " + " ".join(cmd))
    child = pexpect.spawn(cmd[0], cmd[1:], timeout=BOOT_TIMEOUT, encoding="utf-8")
    child.logfile = sys.stdout  # full serial transcript on stdout for CI logs

    try:
        wait_for_shell(child)
        print("\n=== ttyS0 shell reached ===\n")

        out, code = run_cmd(child, "cat /tmp/tarno-desktop.log")
        print(f"--- /tmp/tarno-desktop.log ---\n{out}\n")
        if "Permission denied" in out:
            failures.append("tarno-desktop.log contains 'Permission denied'")
        if "labwc exited" in out:
            failures.append(
                "tarno-desktop.log shows labwc already exited "
                "(this is the exact 'lands in shell' symptom)"
            )
        if "starting labwc" not in out:
            failures.append(
                "tarno-desktop.log never shows labwc being started on tty1 - "
                "tarno-desktop.sh's console check may not be matching"
            )

        out, code = run_cmd(child, "id -nG user")
        print(f"--- id -nG user ---\n{out}\n")
        if "video" not in out.split():
            failures.append(
                f"user is not in the video group (groups: {out!r}) - "
                "seatd's socket is unreachable, labwc can't start"
            )

        out, code = run_cmd(child, "cat /etc/network/interfaces")
        print(f"--- /etc/network/interfaces ---\n{out}\n")
        if "dhcp" in out.lower():
            failures.append(
                "/etc/network/interfaces has a dhcp stanza - live-boot's "
                "9990-netbase.sh overwrote it, STATICIP=frommedia didn't "
                "take effect, dhcpcd will refuse to start"
            )

        out, code = run_cmd(child, "rc-status default")
        print(f"--- rc-status default ---\n{out}\n")
        for svc in ("seatd", "dhcpcd", "tarno-earlysetup", "tarnod"):
            m = re.search(rf"^\s*\*?\s*{svc}\s+\[\s*(\w+)\s*\]", out, re.MULTILINE)
            if not m:
                failures.append(f"rc-status default doesn't list {svc} at all")
            elif m.group(1) != "started":
                failures.append(f"{svc} is not started (rc-status: {m.group(1)})")

        out, code = run_cmd(child, "pgrep -x labwc")
        print(f"--- pgrep -x labwc ---\n{out!r} (exit {code})\n")
        if code != 0 or not out.strip():
            failures.append(
                "no running labwc process - it crashed or never started "
                "(the actual 'lands in shell' bug)"
            )

        out, code = run_cmd(child, "pgrep -x waybar")
        print(f"--- pgrep -x waybar ---\n{out!r} (exit {code})\n")
        if code != 0 or not out.strip():
            failures.append(
                "no running waybar process - labwc's autostart didn't "
                "complete, desktop isn't actually usable even if labwc "
                "itself is technically alive"
            )

    except (pexpect.TIMEOUT, pexpect.EOF, RuntimeError) as exc:
        failures.append(f"boot/interaction failed: {exc}")
    finally:
        child.sendline("poweroff -f")
        try:
            child.expect(pexpect.EOF, timeout=30)
        except pexpect.TIMEOUT:
            pass
        child.close(force=True)

    print("\n=== result ===")
    if failures:
        print(f"FAIL - {len(failures)} problem(s):", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1

    print("PASS - desktop reached a working state, no known regressions found")
    return 0


if __name__ == "__main__":
    sys.exit(main())
