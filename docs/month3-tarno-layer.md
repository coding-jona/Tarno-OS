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

## Tarno AI

Tarno AI ist als Assistent direkt in `tarnod` integriert (kein separater
Prozess) — in drei Phasen geplant. Phase 1 ist in dieser Session real
gebaut und getestet; Phase 2 und 3 sind bewusst nur dokumentiert, nicht
umgesetzt.

### Phase 1 (fertig): heuristisches Backend, kein LLM

Modul `tarnod/tarnod/src/ai/` — Geschwister von `gaming.rs`, `security/`,
`vault.rs`, kein neues Crate, keine neue Cargo-Abhängigkeit:

- **`ai/backend.rs`**: `trait AiBackend { fn answer(&self, question: &str, ctx: &SystemContext) -> String; }`
  plus `SystemContext` — trägt Gaming-Mode-Status (`GamingController::current_governor`),
  isolierte CPUs (`GamingController::isolated_cpus`), das `ebpf`-Feature-
  Flag sowie `MemTotal`/`MemAvailable` aus `/proc/meminfo`. Der Trait ist
  bewusst so geschnitten, dass ein künftiges Phase-2-Backend ihn ohne
  Umbau an `AppState`/`dispatch()` implementieren kann.
- **`ai/heuristic.rs`**: `HeuristicBackend`, das Phase-1-Backend.
  Mustererkennt eine kleine Menge bekannter Fragenformen (deutsch/englisch:
  Gaming-Mode-Status, RAM-Status, Security-Status) per einfachem
  Substring-Match auf die kleingeschriebene Frage und antwortet templated
  anhand des echten `SystemContext` — kein generativer Text, daher auch
  keine Halluzinationsgefahr. Unbekannte Fragen bekommen eine ehrliche
  Fallback-Antwort statt einer erfundenen.
- **`ai/tuning.rs`**: proaktiver Tuning-Task, `tokio::spawn`'t neben dem
  IPC-Server (`main.rs`) — analog zum eBPF-RingBuf-Poller-Pattern aus
  `security/ebpf_loader.rs`. Pollt alle **30 Sekunden** (bewusst lang: ein
  Hintergrund-Task in einem Daemon, kein interaktiver Prozess) RAM- und
  Gaming-Mode-Status über denselben `SystemContext::gather` und pusht bei
  einer einfachen Regel-Verletzung (verfügbares RAM < 15 % **und**
  Gaming-Mode aus) einen Vorschlagstext in `AiState`s Suggestions-Queue
  (FIFO, auf 50 Einträge gedeckelt).
- **IPC**: drei neue `Request`-Varianten in `tarnod-protocol`
  (`AiQuery{text}`, `AiStatus`, `AiSuggestions`), mit denselben
  Serde-Attributen und demselben Response-Muster wie alle bestehenden
  Commands. `dispatch()` in `main.rs` behandelt sie wie jeden anderen
  Request.
- **CLI**: `tarnoctl ai status` (System-Kontext), `tarnoctl ai suggestions`
  (aktuelle Vorschlagsliste), `tarnoctl ai <frage...>` (Freitext-Frage).
  Der Freitext-Payload wird über `serde_json::json!`/`to_string` gebaut,
  nicht über rohe String-Interpolation wie bei den übrigen, festen
  `tarnoctl`-Commands — eine Frage kann Anführungszeichen/Sonderzeichen
  enthalten, die sonst kaputtes JSON erzeugen würden.

Testabdeckung: Unit-Tests für `HeuristicBackend::answer` (alle drei
Fragenformen + Fallback + fehlendes `/proc/meminfo`), für die
Tuning-Regel (`ai::tuning::evaluate`, isoliert von Timer/IO testbar), für
`AiState`s Queue-Deckelung, sowie Roundtrip-Serialisierungstests für die
drei neuen `Request`-Varianten in `tarnod-protocol` — im selben
`#[cfg(test)]`-Stil wie der Rest des Codebase.

### Phase 2 (nicht umgesetzt, nur dokumentiert): lokales LLM-Backend

Eine neue, optionale Crate `tarnod/tarnod-ai/` würde als
Workspace-Mitglied ergänzt (siehe reservierter Kommentar in
`tarnod/Cargo.toml`), aber **nicht** als Default-Member — genau das
Muster, mit dem früher `tarnod-ui` ausgeschlossen war, bevor es entfernt
wurde. Eingebunden in `tarnod` über ein neues optionales Cargo-Feature
`ai-llm`, das exakt so verdrahtet würde wie das bestehende `ebpf`-Feature
den `tarnod-guard`-Pfad-Dependency einbindet (`Cargo.toml`:
`tarnod-ai = { path = "../tarnod-ai", optional = true }`,
`ai-llm = ["dep:tarnod-ai"]`).

Empfehlung für die Inferenz-Bibliothek: **`candle`**
(huggingface/candle) statt `llama.cpp`-Bindings — reines Rust, kein
C++-Toolchain-Dependency im Build, aus derselben Begründung, mit der
dieses Dokument bereits `aya` statt libbpf-C für die eBPF-Anbindung
gewählt hat (siehe oben, "Sprache/Runtime: Rust").

Hardware-Realität, ehrlich benannt statt schöngeredet: die Zielhardware
(Dell Precision M6700) ist eine Laptop-CPU aus der Ivy-Bridge-Generation
ohne dedizierte KI-Beschleunigung. Realistisch ist ein kleines,
quantisiertes Modell — **1 bis 3 Milliarden Parameter, Q4-Quantisierung
(GGUF-Format-Klasse)** — mit entsprechend begrenzten Fähigkeiten
(einfache Frage-Antwort-Muster, kein komplexes Reasoning, spürbare
Latenz auf CPU-only-Inferenz). Das ist eine reale Hardware-Grenze dieses
Zielsystems, kein Implementierungsdetail, das sich wegoptimieren ließe.

### Phase 3 (nicht umgesetzt, nur dokumentiert): Security-Intelligenz

`security::ebpf_loader`s Event-Stream (`ExecEvent{pid, uid, comm,
filename}`, siehe oben) würde zusätzlich in `SystemContext`/`AiState`
eingespeist, damit `AiQuery`/`AiSuggestions` auch über jüngste
Security-Events reden können ("was wurde zuletzt geblockt", "warum wurde
Prozess X angehalten"). Das ist **additiv** zur bestehenden
Tracepoint/Policy-Engine gedacht — Tarno AI würde Events nur lesend
konsumieren und erklären, nicht die SIGSTOP-Entscheidung selbst treffen
oder ersetzen.

## Woche 12: Polish & Dokumentation

- FPS/Frametime-Overlay: siehe `scripts/benchmark.sh` in [`month2-gaming-tuning.md`](month2-gaming-tuning.md) — ein Live-Overlay (angepasstes MangoHud) ist als Stretch-Goal vorgesehen, sobald die Kernkomponenten stabil laufen; nicht Teil des initialen Full-Stack-Durchstichs in diesem Repo.
- Build-Reproduzierbarkeit: `tarno-br2-external/configs/tarno_m6700_defconfig` + dieses Doku-Set sind der Reproduzierbarkeits-Anker (ein `make BR2_EXTERNAL=... tarno_m6700_defconfig && make` auf einer sauberen Maschine soll dasselbe Image ergeben).
- Abschlussbericht (Was wurde erreicht vs. Manifest): wird nach den ersten realen Boot-/Hardware-Tests als eigenes Dokument ergänzt.
