package main

import (
	"bufio"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

type device struct {
	name      string
	size      uint64
	removable bool
}

func listDevices() ([]device, error) {
	entries, err := os.ReadDir("/sys/block")
	if err != nil {
		return nil, err
	}

	var devices []device
	for _, e := range entries {
		name := e.Name()
		if strings.HasPrefix(name, "loop") || strings.HasPrefix(name, "ram") || strings.HasPrefix(name, "zram") {
			continue
		}

		removable, err := os.ReadFile(filepath.Join("/sys/block", name, "removable"))
		if err != nil || strings.TrimSpace(string(removable)) != "1" {
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

		devices = append(devices, device{name: name, size: sectors * 512, removable: true})
	}

	return devices, nil
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
		return false
	}
	return strings.Contains(string(mounts), "/dev/"+name)
}

func confirm(devPath string) bool {
	fmt.Printf("type the device name (%s) to confirm: ", devPath)
	line, _ := bufio.NewReader(os.Stdin).ReadString('\n')
	return strings.TrimSpace(line) == devPath
}

func writeImage(imgPath, devPath string) error {
	src, err := os.Open(imgPath)
	if err != nil {
		return err
	}
	defer func() { _ = src.Close() }()

	dst, err := os.OpenFile(devPath, os.O_WRONLY, 0)
	if err != nil {
		return err
	}
	defer func() { _ = dst.Close() }()

	buf := make([]byte, 4*1024*1024)
	written, err := io.CopyBuffer(dst, src, buf)
	if err != nil {
		return err
	}
	fmt.Printf("%s written\n", humanSize(uint64(written)))

	return dst.Sync()
}

func usage() {
	fmt.Fprintln(os.Stderr, "usage: tarno-install [-force] <image> <device>")
	fmt.Fprintln(os.Stderr, "       tarno-install            (list removable devices)")
}

func main() {
	force := flag.Bool("force", false, "write to a non-removable or mounted device")
	flag.Usage = usage
	flag.Parse()
	args := flag.Args()

	devices, err := listDevices()
	if err != nil {
		fmt.Fprintln(os.Stderr, "listing devices:", err)
		os.Exit(1)
	}

	if len(args) == 0 {
		if len(devices) == 0 {
			fmt.Println("no removable devices found")
			return
		}
		fmt.Println("removable devices:")
		for _, d := range devices {
			fmt.Printf("  /dev/%s  %s\n", d.name, humanSize(d.size))
		}
		return
	}

	if len(args) != 2 {
		usage()
		os.Exit(1)
	}

	imgPath := args[0]
	devName := strings.TrimPrefix(args[1], "/dev/")
	devPath := "/dev/" + devName

	if _, err := os.Stat(imgPath); errors.Is(err, os.ErrNotExist) {
		fmt.Fprintf(os.Stderr, "%s not found\n", imgPath)
		os.Exit(1)
	}

	if !*force {
		found := false
		for _, d := range devices {
			if d.name == devName {
				found = true
				break
			}
		}
		if !found {
			fmt.Fprintf(os.Stderr, "%s is not a removable device, use -force to override\n", devPath)
			os.Exit(1)
		}
		if isMounted(devName) {
			fmt.Fprintf(os.Stderr, "%s has mounted partitions, unmount first or use -force\n", devPath)
			os.Exit(1)
		}
	}

	fmt.Printf("this will erase all data on %s\n", devPath)
	if !confirm(devPath) {
		fmt.Println("aborted")
		os.Exit(1)
	}

	if err := writeImage(imgPath, devPath); err != nil {
		fmt.Fprintln(os.Stderr, "write failed:", err)
		os.Exit(1)
	}

	fmt.Println("done")
}
