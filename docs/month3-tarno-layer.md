# Monat 3 — Tarno-Layer: Daemon, Security, Polish (Detailplan)

Übersicht siehe [`ROADMAP.md`](../ROADMAP.md#monat-3--tarno-layer-daemon-security-ai). Umgesetzt in `tarnod/` (Rust-Workspace) und `tarno-guard-ebpf/` (eBPF-Subsystem).

## Woche 9-10: `tarnod` als Userspace-Service

### Sprache/Runtime: Rust
Begründung (statt C++ oder .NET Native AOT, beide in der ROADMAP als Optionen genannt): Rust bietet mit `aya` eine ausgereifte, reine Rust-eBPF-Toolchain (kein libbpf-C-Binding nötig), Memory-Safety ohne GC-Pausen (relevant für einen Prozess, der neben einem Spiel mitläuft), und dieselbe Sprache für Daemon + eBPF-Programm reduziert die Toolchain-Komplexität.

### IPC-Design
- Unix-Domain-Socket unter `/run/tarnod/tarnod.sock`, Verzeichnis `0700`, Socket `0600` (Race zwischen `bind()` und `chmod()` wird durch `umask(0o177)` **vor** `bind()` vermieden, nicht durch nachträgliches `chmod`).
- Zusätzlich `SO_PEERCRED`-Check (`UnixStream::peer_cred()`, seit Rust 1.65 in `std` stabil) — verifiziert UID/GID des verbindenden Prozesses als zweite Absicherungsebene neben den Dateirechten.
- Framing: Newline-delimited JSON (`serde_json`) — einfachstes Protokoll ohne Extra-Crate, ausreichend für das niedrige Anfragevolumen eines lokalen Kontroll-Sockets.
- **Warum `tokio` statt synchronem `std::os::unix::net` + Threads:** Da der eBPF-RingBuf-Reader (Woche 11) ohnehin einen async Event-Loop braucht (`tokio::io::unix::AsyncFd`), würde ein synchroner IPC-Server ein zweites, paralleles Concurrency-Modell im selben Prozess bedeuten. Einheitlich `tokio` mit `current_thread`-Flavor hält RAM-Verbrauch niedrig (kein Multi-Thread-Pool) und vermeidet die Komplexität von zwei Nebenläufigkeitsmodellen.

### API-Key-Vault
- Root-only Datei (`/etc/tarnod/secrets.toml`, `0600`) wird **einmal beim Start** gelesen und in einer `HashMap<String, String>` im Prozessspeicher gehalten — kein erneuter Festplattenzugriff danach.
- Zugriff ausschließlich über den IPC-Socket (`GetApiKey{name}`-Request), der denselben Dateirechte- und `peer_cred`-Schutz wie alle anderen Requests durchläuft.
- Kein Kernel-Vault/keine Verschlüsselung im RAM nötig (wie im Manifest ursprünglich angedacht) — die Prämisse ist: korrekte Unix-Dateirechte + Prozessisolation reichen, da auf Tarno OS ohnehin kein Multi-User-Betrieb stattfindet.

## Woche 11: Behavioral Security ohne Kernel-Patch (eBPF)

### Hook-Wahl: Tracepoint statt LSM (Phase 1)
- `sched_process_exec`-Tracepoint statt `bprm_check_security`-LSM-Hook: keine `CONFIG_BPF_LSM=y`-Kernel-Abhängigkeit, robuster über verschiedene Kernel-Configs hinweg. Nachteil: reaktiv (Prozess läuft bereits kurz, bevor SIGSTOP greift), nicht präventiv wie ein LSM-Deny. Für "80% des Behavioral Kernel Shields" (Roadmap-Ziel) ausreichend; LSM-Hook als möglicher Phase-2-Ausbau dokumentiert, falls Pre-Exec-Blocking später gewünscht ist.

### Warum SIGSTOP aus Userspace
eBPF-Programme können nur `bpf_send_signal()`/`bpf_send_signal_thread()` an den **aktuell laufenden Task** senden, nicht an beliebige andere PIDs. Ein Tracepoint-Programm, das einen fremden verdächtigen Prozess stoppen soll, kann das also nicht direkt aus dem Kernel-Space tun. Deshalb: eBPF sendet nur das `ExecEvent` (PID, UID, `comm`, Binärpfad) über eine `RingBuf`-Map an Userspace; `tarnod` wertet es gegen die Policy (Allow-/Deny-Liste, ggf. Heuristiken) aus und ruft bei Treffer selbst `kill(pid, SIGSTOP)` (via `libc`) auf. Das hält die Policy-Logik zudem in Rust statt in eBPF-Bytecode — leichter wartbar und erweiterbar.

### Ablauf fortsetzen
- `tarnoctl resume <pid>` sendet `SIGCONT` an einen gestoppten Prozess (nach manueller Prüfung durch den Nutzer) — verhindert, dass ein False Positive das System dauerhaft blockiert.

### Systemvoraussetzungen (siehe auch [`architecture.md`](architecture.md))
- `CONFIG_BPF=y`, `CONFIG_BPF_SYSCALL=y`, `CONFIG_DEBUG_INFO_BTF=y` im Kernel (bereits in Woche 1-2 als Kernel-Fragment vorgesehen).
- Capabilities für `tarnod`: `cap_bpf`, `cap_perfmon` (für Tracing-Programme seit Kernel 5.8 zusätzlich zu `cap_bpf` nötig).

## Woche 12: Polish & Dokumentation

- FPS/Frametime-Overlay: siehe `scripts/benchmark.sh` in [`month2-gaming-tuning.md`](month2-gaming-tuning.md) — ein Live-Overlay (angepasstes MangoHud) ist als Stretch-Goal vorgesehen, sobald die Kernkomponenten stabil laufen; nicht Teil des initialen Full-Stack-Durchstichs in diesem Repo.
- Build-Reproduzierbarkeit: `tarno-br2-external/configs/tarno_m6700_defconfig` + dieses Doku-Set sind der Reproduzierbarkeits-Anker (ein `make BR2_EXTERNAL=... tarno_m6700_defconfig && make` auf einer sauberen Maschine soll dasselbe Image ergeben).
- Abschlussbericht (Was wurde erreicht vs. Manifest): wird nach den ersten realen Boot-/Hardware-Tests als eigenes Dokument ergänzt.
