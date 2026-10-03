# Schichtbericht — Nacht auf 2026-10-03 (bis ca. 9:30 Uhr)

*Für Jona. Alles lokal committet auf `thos/acer-bios-fbcon`; **nichts gepusht** (das machst du, siehe unten).*

## Kurzfassung
THOS kann jetzt **echte, unveränderte Linux-Programme** laufen lassen, nicht nur statisch gelinkte Testprogramme:
ein dynamisch gelinktes glibc-PIE-Programm mit `ld.so`, `libc.so.6`, `malloc`, `qsort`, **pthreads** (Mutex, Condvar, Join,
TLS), `fork`/`exit`/`atexit`, dazu **Signale, Ctrl+C, Netzwerk (TCP/UDP, DHCP, `wget`)**, `/dev`, `/proc` (`free`, `ps`),
PS/2-Maus und echte Uhr/Zufall. Alle 21 Tests der Suite melden „PASS“ — mit einer Einschränkung beim Netztest (siehe „Ehrlich“).

## Gebaut (Commits, neueste zuerst)
| Commit | Was |
|---|---|
| `93f2af0` | `select`/`pselect6` auf Basis von `poll_mask` |
| `d4dc05b` | `alarm`, `setitimer`/`getitimer` (SIGALRM) über einen Timer-Thread, der bei Leerlauf schläft |
| `e8047c7` | `fork-test` (glibc: fork + exit + atexit) |
| `d1d53c7` | virtuelles `/proc`: meminfo, uptime, version, cpuinfo, loadavg, stat, mounts, `self`, `<pid>/{stat,status,cmdline,exe}`; BusyBox `free` und `ps` laufen |
| `4db9f83` | `smp-test`: Frame-Basislinie erst nach Beruhigung messen (war ein Timing-Fehler im Test) |
| `40e7f17` | **Threads**: `clone(CLONE_THREAD)`, `futex` (WAIT/WAKE/BITSET), tids + `clear_child_tid`, `exit` vs. `exit_group`, tkill/tgkill, `mprotect` committet PROT_NONE-Reservierungen, echtes `prlimit64`/`getrlimit` |
| `b5abf6a` | Loader-Regressionen behoben: Adresslimit für ET_EXEC, `munmap`/MAP_FIXED geben Frames zurück (Leck vom `proc teardown`-Meilenstein gefangen) |
| `43bf8cb` | **Dynamischer Loader**: ET_DYN/PIE + `PT_INTERP`, Aux-Vektor (AT_BASE/ENTRY/SECURE/CLKTCK, zufälliges AT_RANDOM), POSIX-`mmap` (Datei, FIXED, PROT_NONE), `munmap`, `mprotect`, `pread64`, `access`, `st_dev`/`st_ino` |
| `c43ee74` | **PS/2-Maus/Touchpad** (`/dev/input/mice`, Zeigerposition), Tastatur-Thread schläft statt zu spinnen |
| `7a954a5` | `net-test` schreibt ein pcap und erkennt eine ausgeschaltete Host-Loopback |
| `796666e` | **DHCP** mit statischem Fallback, `/etc/resolv.conf` virtuell aus dem Lease |
| `1810d09` | **Sockets** (AF_INET TCP/UDP) als Dateiobjekte, Socket-Syscalls, echtes `poll`/`ppoll`, `O_NONBLOCK` |
| `83fad8a` | **Netzwerk N0**: virtio-net-Treiber + smoltcp, Ping des Gateways |
| `8ce7f49` | `/dev/null`, `zero`, `urandom`, `random`, `tty`, `console` |
| `a368d98` | **POSIX-Signale**: Handler auf Linux-`rt_sigframe`, Masken, SA_RESTART, EINTR, SIGKILL über Timer-IRQ, Prozessgruppen, Ctrl+C → SIGINT |

Insgesamt (seit `f7749aa`): 33 Dateien, rund +4700 Zeilen.

## Getestet (Abschluss-Suite, 09:23 Uhr)
**PASS (20):** kbd (inkl. fputest, ptrtest, clocktest, sigtest mit alarm, devtest mit select, Ctrl+C-Ende-zu-Ende), dyn, thr, fork,
proc, busybox, pipe, pe, bios, bios-kbd, shortcuts, longcmd, bios-power, random, mouse, ahci, ext2, fat, smp, integrity.

**Ehrlich — nicht wirklich bestanden:**
- **`net-test` hat sich selbst übersprungen** („SKIPPED“, Exit-Code 0, deshalb „PASS“ in der Liste). Auf deinem Rechner ist die
  Loopback-Schnittstelle **`lo` ausgeschaltet** (`ip -br addr` zeigt `lo DOWN`); QEMU leitet Gast-Verbindungen an 10.0.2.2 über
  den Host-Loopback, daher laufen TCP/UDP-Verbindungen zum Host in Timeouts. Beweis: pcap zeigt SYNs raus, keine Antwort; selbst
  Python kann `127.0.0.1` nicht erreichen. Vorher (als `lo` noch oben war) bestanden Ping, TCP, UDP, `poll`, BusyBox-`wget`,
  DHCP und resolv.conf. **Abhilfe (braucht root, das darf ich nicht):** `sudo ip link set lo up`, dann `cargo xtask net-test`.
- **Flaky** (in Einzelläufen schon rot gewesen): `smp-test` (Scheduler-Heisenbug mit 24 CPUs, nach der Test-Härtung 3/3 + 1x grün),
  `shortcuts-test` (einmal rot, zweimal grün; Tipp-Timing).
- Bekannt flaky und diese Nacht nicht gelaufen: `ncq-error-test` (B15).

## Gefundene und behobene Fehler (Auswahl)
- SIGPIPE im Pipe-Code tötete beim Exec-Check den startenden Prozess → jetzt im Syscall-Layer.
- `nanosleep` schlief bis zu einen Tick zu kurz.
- `init_mouse` las das ACK der Tastatur als Controller-Konfig und schaltete damit die Tastatur ab.
- `ld.so` hielt `libc.so.6` für „schon geladen“, weil `st_ino` immer 0 war.
- `munmap` gab Frames nicht frei (der Teardown-Meilenstein hat 2 Frames/Prozess Leck gemeldet).
- `prlimit64` schrieb nichts → glibc errechnete absurde Thread-Stack-Größen → Kernel-Panic im `mprotect` (jetzt echte Werte, ENOMEM statt Panic).
- AT_RANDOM war eine Konstante (0x5A…) → jetzt aus dem CSPRNG (Stack-Canary/malloc).

## Offen / Risiken
- **B17 — BusyBox `ps` als geforkter Shell-Kindprozess stürzt beim Beenden ab** (`call *%rax` mit rax=1 in `__run_exit_handlers`).
  Direkt per exec (`/busybox ps`) und mit glibc-`fork`+`atexit` (fork-test, dynamisch **und** statisch gelinkt) tritt es nicht auf — es ist also BusyBox-spezifisch, kein allgemeines Fork-Problem. Ausgabe/Shell nicht betroffen. Nicht gelöst.
- **B16 — Scheduler-Heisenbug im `smp-test`** (24 CPUs, TCG): nicht ursächlich geklärt, nur der zeitabhängige Frame-Zähler-Test gehärtet.
- `munmap` gibt Frames frei, aber `MAP_SHARED` schreibt nicht zurück; Signalzustand ist pro Prozess (kein thread-gerichtetes Signal); keine robust-/PI-Futexe; `clone3` fehlt (glibc fällt auf `clone` zurück).
- Netzwerk pollt noch (10 ms bzw. 2 ms in blockierenden Aufrufen) statt IRQ-getrieben; keine echten NIC-Treiber (Acer: `lspci -nn` fehlt); DNS Ende-zu-Ende ungetestet; `sendmsg/recvmsg/sendmmsg`, AF_UNIX fehlen.
- PS/2-Eingabe: Tastatur/Maus pollen alle 8 ms (IRQ-getrieben über den IO-APIC steht aus, B12).
- Echte Hardware (Acer 5742G) wurde nicht getestet — nur QEMU (`-cpu Westmere -m 512M` für die neuen Tests).

## Limit-Verbrauch
Wochenlimit: 94 % → (Reset 0:00) → ca. 10 % bis jetzt; 5-Stunden-Fenster ca. 51 %.

## Was du tun musst
1. **Pushen** (ich darf nicht): die neuen Commits liegen nur lokal. Dein früherer Push-Befehl für `condingmaaster/Tarno-OS` gilt weiter.
2. **`sudo ip link set lo up`**, dann `cargo xtask net-test` — ich sehe dann, ob das Netzwerk weiterhin komplett grün ist.
3. Bei Gelegenheit auf dem Acer: `lspci -nn` und `lsusb` ausgeben (für die NIC-Treiber und EHCI).

## Vorschläge für als Nächstes
1. IRQ-getriebene Eingabe + Netzwerk (IO-APIC/MSI) → Akku/Last auf dem Acer.
2. Echte NIC (Realtek RTL8168/Atheros — erst `lspci` abwarten), danach e1000.
3. B17 und B16 einkreisen (B17 mit einem reproduzierenden C-Test, B16 mit Scheduler-Tracing).
4. Desktop-Plan beginnen: Maus ist da, `window`/`gdi` existieren; ein Compositor ist der nächste echte Schritt.
5. Software installieren: ein kleines `apt`/`dpkg`-ähnliches Format oder direkt Alpine-`apk` (musl-dynamisch, braucht `ld-musl`) prüfen.
