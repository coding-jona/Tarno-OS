package main

import (
	"bufio"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
)

type disk struct {
	name string
	size uint64
}

func listDisks() ([]disk, error) {
	entries, err := os.ReadDir("/sys/block")
	if err != nil {
		return nil, err
	}

	var disks []disk
	for _, e := range entries {
		name := e.Name()
		if strings.HasPrefix(name, "loop") || strings.HasPrefix(name, "ram") || strings.HasPrefix(name, "zram") {
			continue
		}

		removable, err := os.ReadFile(filepath.Join("/sys/block", name, "removable"))
		if err != nil || strings.TrimSpace(string(removable)) == "1" {
			continue
		}

		sizeRaw, err := os.ReadFile(filepath.Join("/sys/block", name, "size"))
		if err != nil {
			continue
		}
		sectors, err := strconv.ParseUint(strings.TrimSpace(string(sizeRaw)), 10, 64)
		if err != nil {
			continue
		}

		disks = append(disks, disk{name: name, size: sectors * 512})
	}

	return disks, nil
}

func humanSize(n uint64) string {
	const unit = 1024
	if n < unit {
		return fmt.Sprintf("%d B", n)
	}
	div, exp := uint64(unit), 0
	for x := n / unit; x >= unit; x /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %ciB", float64(n)/float64(div), "KMGTPE"[exp])
}

func isMounted(name string) bool {
	mounts, err := os.ReadFile("/proc/mounts")
	if err != nil {
		return true
	}
	for _, line := range strings.Split(string(mounts), "\n") {
		fields := strings.Fields(line)
		if len(fields) > 0 && strings.HasPrefix(fields[0], "/dev/"+name) {
			return true
		}
	}
	return false
}

func isLiveSystem() bool {
	mounts, err := os.ReadFile("/proc/mounts")
	if err != nil {
		return false
	}
	return strings.Contains(string(mounts), " / overlay ") || strings.Contains(string(mounts), "/run/live")
}

// partPath returns the device node for partition n of disk (sdb -> sdb1, nvme0n1 -> nvme0n1p1).
func partPath(disk, n string) string {
	last := disk[len(disk)-1]
	if last >= '0' && last <= '9' {
		return "/dev/" + disk + "p" + n
	}
	return "/dev/" + disk + n
}

func confirm(devPath string) bool {
	fmt.Printf("type the device name (%s) to confirm: ", devPath)
	line, _ := bufio.NewReader(os.Stdin).ReadString('\n')
	return strings.TrimSpace(line) == devPath
}

func run(name string, args ...string) error {
	cmd := exec.Command(name, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s %s: %w", name, strings.Join(args, " "), err)
	}
	return nil
}

func quiet(name string, args ...string) {
	_ = run(name, args...)
}

func output(name string, args ...string) (string, error) {
	out, err := exec.Command(name, args...).Output()
	if err != nil {
		return "", fmt.Errorf("%s %s: %w", name, strings.Join(args, " "), err)
	}
	return strings.TrimSpace(string(out)), nil
}

// biosBoot reports whether this process is running under legacy
// BIOS, not UEFI - i.e. exactly the case grub-install --target=x86_64-efi
// can never make bootable, no matter how correctly it runs. Confirmed
// on a real install: BIOS-mode VM, EFI-only bootloader, "hangs at boot
// from disk" - a GPT disk with only an ESP is invisible to BIOS
// firmware, which has nothing to do with grub-install actually working.
func biosBoot() bool {
	_, err := os.Stat("/sys/firmware/efi")
	return err != nil
}

func install(devName string) error {
	devPath := "/dev/" + devName
	espPath := partPath(devName, "2")
	rootPath := partPath(devName, "3")

	fmt.Println("partitioning", devPath)
	// GPT + BIOS booting needs a dedicated ~1MiB BIOS boot partition
	// (the `bios_grub` flag) for grub-install --target=i386-pc to
	// embed its core image into - there's no traditional MBR gap to
	// (ab)use like there is with an msdos partition table. Always
	// creating both this and the ESP means the same install works
	// whichever firmware the machine actually has, rather than baking
	// in an assumption.
	if err := run("parted", "--script", devPath,
		"mklabel", "gpt",
		"mkpart", "bios_grub", "1MiB", "2MiB",
		"set", "1", "bios_grub", "on",
		"mkpart", "ESP", "fat32", "2MiB", "514MiB",
		"set", "2", "esp", "on",
		"mkpart", "root", "ext4", "514MiB", "100%",
	); err != nil {
		return err
	}
	if err := run("mkfs.vfat", "-F32", espPath); err != nil {
		return err
	}
	if err := run("mkfs.ext4", "-F", rootPath); err != nil {
		return err
	}

	target, err := os.MkdirTemp("", "tarno-disk-install")
	if err != nil {
		return err
	}
	defer func() { _ = os.RemoveAll(target) }()

	if err := run("mount", rootPath, target); err != nil {
		return err
	}
	defer quiet("umount", "-R", target)

	efiDir := filepath.Join(target, "boot", "efi")
	if err := os.MkdirAll(efiDir, 0o755); err != nil {
		return err
	}
	if err := run("mount", espPath, efiDir); err != nil {
		return err
	}

	fmt.Println("copying live system to", target, "(this takes a while)")
	if err := run("rsync", "-aHAX", "--info=progress2",
		"--exclude=/proc/*", "--exclude=/sys/*", "--exclude=/dev/*",
		"--exclude=/run/*", "--exclude=/tmp/*", "--exclude=/mnt/*",
		"--exclude=/media/*", "--exclude=/lost+found",
		"--exclude="+target,
		"/", target+"/",
	); err != nil {
		return err
	}

	for _, dir := range []string{"proc", "sys", "dev"} {
		if err := os.MkdirAll(filepath.Join(target, dir), 0o755); err != nil {
			return err
		}
	}
	if err := run("mount", "--bind", "/dev", filepath.Join(target, "dev")); err != nil {
		return err
	}
	defer quiet("umount", filepath.Join(target, "dev"))
	if err := run("mount", "-t", "proc", "proc", filepath.Join(target, "proc")); err != nil {
		return err
	}
	defer quiet("umount", filepath.Join(target, "proc"))
	if err := run("mount", "-t", "sysfs", "sys", filepath.Join(target, "sys")); err != nil {
		return err
	}
	defer quiet("umount", filepath.Join(target, "sys"))

	espUUID, err := output("blkid", "-s", "UUID", "-o", "value", espPath)
	if err != nil {
		return err
	}
	rootUUID, err := output("blkid", "-s", "UUID", "-o", "value", rootPath)
	if err != nil {
		return err
	}

	fstab := fmt.Sprintf(
		"UUID=%s / ext4 defaults 0 1\nUUID=%s /boot/efi vfat umask=0077 0 1\n",
		rootUUID, espUUID,
	)
	if err := os.WriteFile(filepath.Join(target, "etc", "fstab"), []byte(fstab), 0o644); err != nil {
		return err
	}

	// Wires the disk-installed system up to Tarno OS' own apt repo
	// (scripts/build-os-deb.sh -> tarno-os.deb, published by
	// .github/workflows/apt-repo.yml) so it can actually receive OS
	// updates via `apt upgrade` - the live/USB session this installer
	// runs from never needed this (ephemeral, nothing persists across
	// a reboot), but the disk-installed system it produces does.
	//
	// [trusted=yes]: the repo has no signing key yet (APT_REPO_GPG_KEY
	// doesn't exist as a secret) - deliberate, user-confirmed interim
	// tradeoff so the update channel actually works today rather than
	// staying blocked indefinitely on a manual step. Transport is
	// already TLS via GitHub Pages; trusted=yes only waives apt's
	// additional package-signature check on top of that. Swap for the
	// commented-out signed line below the moment the secret exists.
	aptSourcesDir := filepath.Join(target, "etc", "apt", "sources.list.d")
	if err := os.MkdirAll(aptSourcesDir, 0o755); err != nil {
		return err
	}
	aptSource := "# deb https://coding-jona.github.io/Tarno-OS/ tarno main\n" +
		"deb [trusted=yes] https://coding-jona.github.io/Tarno-OS/ tarno main\n"
	if err := os.WriteFile(
		filepath.Join(aptSourcesDir, "tarno-os.list"), []byte(aptSource), 0o644,
	); err != nil {
		return err
	}

	if biosBoot() {
		fmt.Println("installing bootloader (BIOS/legacy)")
		if err := run("chroot", target, "grub-install",
			"--target=i386-pc", "--recheck", devPath,
		); err != nil {
			return err
		}
	} else {
		fmt.Println("installing bootloader (UEFI)")
		if err := run("chroot", target, "grub-install",
			"--target=x86_64-efi", "--efi-directory=/boot/efi",
			"--bootloader-id=Tarno", "--recheck", devPath,
		); err != nil {
			return err
		}
	}

	f, err := os.OpenFile(filepath.Join(target, "etc", "default", "grub"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	// GRUB_GFXPAYLOAD_LINUX=keep is the actual fix for "whole system
	// stuck at the wrong resolution after install" - without it, GRUB
	// negotiates its own graphics mode with the firmware for its menu,
	// then resets to a plain VESA/VGA text mode before handing off to
	// the kernel, which on BIOS/legacy firmware (this repo's own
	// biosBoot(), same as the ESP+bios_grub partitioning above) usually
	// means a low default like 800x600 - and that low mode is what the
	// console AND everything drawn on top of it afterwards (including
	// labwc/Wayland, which inherits whatever mode is already active)
	// gets stuck with. `keep` hands off GRUB's own negotiated mode
	// unchanged instead of resetting it; GRUB_GFXMODE=auto is what lets
	// GRUB negotiate its best (i.e. actual native) mode in the first
	// place rather than defaulting conservatively itself. Appended
	// (shell-sourced last-assignment-wins, same reasoning as
	// GRUB_CMDLINE_LINUX_DEFAULT below) rather than trying to edit
	// Devuan's own stock line in place.
	_, werr := f.WriteString(
		"GRUB_GFXMODE=auto\n" +
			"GRUB_GFXPAYLOAD_LINUX=keep\n" +
			"GRUB_CMDLINE_LINUX_DEFAULT=\"init=/sbin/openrc-init\"\n",
	)
	cerr := f.Close()
	if werr != nil {
		return werr
	}
	if cerr != nil {
		return cerr
	}

	return run("chroot", target, "update-grub")
}

func usage() {
	fmt.Fprintln(os.Stderr, "usage: tarno-disk-install [-force] <device>")
	fmt.Fprintln(os.Stderr, "       tarno-disk-install                  (list internal disks)")
}

func main() {
	force := flag.Bool("force", false, "skip the live-system, internal-disk and mounted checks")
	flag.Usage = usage
	flag.Parse()
	args := flag.Args()

	if os.Geteuid() != 0 {
		fmt.Fprintln(os.Stderr, "must run as root")
		os.Exit(1)
	}

	disks, err := listDisks()
	if err != nil {
		fmt.Fprintln(os.Stderr, "listing disks:", err)
		os.Exit(1)
	}

	if len(args) == 0 {
		if len(disks) == 0 {
			fmt.Println("no internal disks found")
			return
		}
		fmt.Println("internal disks:")
		for _, d := range disks {
			fmt.Printf("  /dev/%s  %s\n", d.name, humanSize(d.size))
		}
		return
	}

	if len(args) != 1 {
		usage()
		os.Exit(1)
	}

	devName := strings.TrimPrefix(args[0], "/dev/")
	devPath := "/dev/" + devName

	if !*force {
		if !isLiveSystem() {
			fmt.Fprintln(os.Stderr, "this doesn't look like a live system, refusing (use -force to override)")
			os.Exit(1)
		}

		found := false
		for _, d := range disks {
			if d.name == devName {
				found = true
				break
			}
		}
		if !found {
			fmt.Fprintf(os.Stderr, "%s is not an internal disk, use -force to override\n", devPath)
			os.Exit(1)
		}

		if isMounted(devName) {
			fmt.Fprintf(os.Stderr, "%s has mounted partitions, unmount first or use -force\n", devPath)
			os.Exit(1)
		}
	}

	fmt.Printf("this will erase all data on %s and install the running system there\n", devPath)
	if !confirm(devPath) {
		fmt.Println("aborted")
		os.Exit(1)
	}

	if err := install(devName); err != nil {
		fmt.Fprintln(os.Stderr, "install failed:", err)
		os.Exit(1)
	}

	fmt.Println("done, reboot into", devPath)
}
