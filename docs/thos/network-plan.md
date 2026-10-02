# THOS – Network stack plan

*Opened 2026-10-02 (user: "thos braucht nen netzwerkstack"). Planning only; the
roadmap's Phase 5 already names a NIC driver + TCP/IP stack, this makes it concrete
and pulls the first slice forward.*

## Prerequisites found in the source review

([`source-review-2026-10.md`](source-review-2026-10.md)) — not network code, but
the network is unsafe or impossible without them: a real **CSPRNG** (B4: today
`getrandom` is a fixed-seed xorshift), a **clock/RTC** (B8), **user-pointer
validation** (B1: a socket syscall layer taking raw pointers is an RCE surface),
**signals/`select`/`poll`** semantics (B3), **IO-APIC + MSI routing** for the NIC
(B12), bus-0-only PCI scan (B13), and a kernel heap that can grow (B6: packet
buffers).

## Goals
Both personalities (POSIX sockets, Winsock) bind **the same kernel endpoint
objects** (one object, many views). Shell-first: `ping`, DHCP, DNS, `wget`/`nc`
(already in BusyBox) work before any desktop.

## Layers
1. **NIC drivers** behind one `NetDev` trait (rx/tx rings, MAC, link state, DMA via
   the existing 1 MiB-arena style allocator). Order: **virtio-net** and **e1000**
   first (QEMU, testable in CI), then the real chips.
   - **Acer 5742G** wired NIC and Wi-Fi card: *unknown until `lspci -nn` is
     captured* (Acer 5742G variants shipped Atheros/Broadcom/Realtek parts).
     Wired first; Wi-Fi is a separate, much larger project (firmware blobs,
     WPA2) — not in the first slices.
   - **ASRock**: Realtek RTL8168 (`10ec:8168`), per `hw-target.md`.
2. **Stack**: **smoltcp** (Rust, no_std, BSD-0/MIT-style licence) *vs* own — decision
   **N1**. Lean: smoltcp for Ethernet/ARP/IPv4/IPv6/ICMP/UDP/TCP + DHCPv4 + DNS
   client, wrapped in a kernel `net` service. Packet processing runs in a kernel
   thread woken by NIC IRQ + timer.
3. **Socket layer**: endpoint objects with handles; Linux syscalls
   (`socket/bind/listen/accept/connect/send*/recv*/poll/setsockopt`) and a Winsock
   mapping on the NT side. `AF_UNIX` pipes-style local sockets first (cheap, and
   what many programs need).
4. **Userland**: `ifconfig`/`ip`-style tool, resolver config (`/etc/resolv.conf`),
   DHCP client as a service, later a firewall.

## Security (this OS ships an AV — the network is part of that)
- Default-deny inbound firewall in the kernel net service; rules are data under
  `/etc/thos/`, editable by the admin only.
- Socket creation/bind gated by the capability model (listening on ports < 1024,
  raw sockets = privileged).
- Security Service gets a hook for outbound-connection verdicts (later).

## Stages
- **N0** virtio-net driver + smoltcp bring-up, static IP, `ping` the QEMU gateway.
- **N1** DHCP + DNS; BusyBox `wget`/`nc` over POSIX sockets, `qemu -nic user` test.
- **N2** e1000 + the real NIC of each target; link-state handling.
- **N3** Winsock personality on the same endpoints.
- **N4** firewall + Security Service hooks; IPv6.
- **N5** Wi-Fi (separate plan).

## Verification
`cargo xtask net-test`: boot with `-nic user`, DHCP lease, ping the gateway,
fetch a file from a host-side HTTP server started by xtask, assert contents.
