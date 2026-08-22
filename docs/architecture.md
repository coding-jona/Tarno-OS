# Tarno OS – Architektur

Dieses Dokument beschreibt die technische Gesamtarchitektur der in [`ROADMAP.md`](../ROADMAP.md) skizzierten Komponenten. Details je Monat stehen in [`month1-foundation.md`](month1-foundation.md), [`month2-gaming-tuning.md`](month2-gaming-tuning.md) und [`month3-tarno-layer.md`](month3-tarno-layer.md).

## Komponentenübersicht

```
┌─────────────────────────────────────────────────────────────────┐
│ Tarno OS (Buildroot-Image, Kernel für Dell Precision M6700)      │
│                                                                   │
│  ┌───────────────┐   Unix-Socket    ┌────────────────────────┐  │
│  │   tarnoctl    │◄────/run/tarnod/────►│        tarnod        │  │
│  │  (CLI-Client) │   tarnod.sock    │   (Root-Daemon, Rust)   │  │
│  └───────────────┘   0600, tokio    │                          │  │
│                                     │  ┌──────────┐ ┌────────┐ │  │
│                                     │  │ vault.rs │ │gaming.rs│ │  │
│                                     │  │ API-Keys │ │isolcpus/│ │  │
│                                     │  │ im RAM   │ │chrt/gov │ │  │
│                                     │  └──────────┘ └────────┘ │  │
│                                     │  ┌──────────────────────┐│  │
│                                     │  │ security/ebpf_loader ││  │
│                                     │  │ (aya, RingBuf-Poll)  ││  │
│                                     │  └──────────┬───────────┘│  │
│                                     └─────────────┼────────────┘  │
│                                                    │ attach        │
│                                     ┌──────────────▼────────────┐  │
│                                     │ tarno-guard-ebpf (Kernel) │  │
│                                     │ Tracepoint sched_process_ │  │
│                                     │ exec → RingBuf-Event      │  │
│                                     └───────────────────────────┘  │
│                                                                   │
│  ┌──────────────────────┐   ┌─────────────────────────────────┐  │
│  │ scripts/gaming-mode.sh│   │ cage (Wayland-Compositor)       │  │
│  │ scripts/jvm-launch.sh │   │ → Minecraft/JVM Direct-Fullscreen│  │
│  └──────────────────────┘   └─────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Komponenten im Detail

### `tarnod` (Root-Daemon)
- Rust-Binary, läuft als OpenRC-Service (`S60tarnod`), Single-Process, `tokio`-Runtime im `current_thread`-Modus (kein Multi-Thread-Overhead, da IO-lastig statt CPU-lastig).
- Verantwortlich für: IPC-Server, API-Key-Vault, Gaming-Mode-Steuerung, eBPF-Loader/Policy-Engine.
- Warum ein Prozess statt mehrere: eBPF-RingBuf-Polling läuft ohnehin async — dieselbe Runtime bedient den IPC-Socket, statt zwei Concurrency-Modelle (Threads + async) im System zu mischen. Details/Begründung: [`month3-tarno-layer.md`](month3-tarno-layer.md#ipc-design).

### `tarnoctl` (CLI-Client)
- Dünner Client, verbindet sich auf denselben Unix-Socket, schickt JSON-Requests (`GetGamingMode`, `SetGamingMode`, `GetApiKey`, `SecurityStatus`, `ResumeProcess`), zeigt Antwort formatiert an.
- Kein eigener State, keine Privilegien nötig außer Socket-Zugriff (Gruppe `tarno` oder root).

### `tarnod-protocol` (geteiltes Typen-Crate)
- `Request`/`Response`-Enums (siehe `tarnod/src/ipc.rs` früher, jetzt hier zentral), von `tarnod` und potenziell `tarnoctl` genutzt — eine Protokolländerung muss nur an einer Stelle nachgezogen werden.

### Tarno AI (Phase 1: heuristisch, Phase 2: Mistral-API, Phase 3: Security-Event-Anbindung)
- Modul `tarnod/tarnod/src/ai/` (Geschwister von `gaming.rs`, `security/`, `vault.rs`), in `AppState` als `ai: ai::AiState` eingebunden.
- `ai/backend.rs`: `#[async_trait] trait AiBackend: Send + Sync { async fn answer(&self, question: &str, ctx: &SystemContext) -> String; }` (seit Phase 2 `async`, da ein Netzwerk-Request nicht blockierend im IPC-Handler laufen darf) + `SystemContext` (Gaming-Mode-Status/Governor, isolierte CPUs, `ebpf`-Feature-Status, RAM aus `/proc/meminfo`, seit Phase 3 zusätzlich eine Security-Event-Zusammenfassung aus `security::events::SecurityEventLog`).
- `ai/heuristic.rs`: Phase-1-Backend `HeuristicBackend` — mustererkennt bekannte Fragenformen (Gaming-Mode/RAM/Security-Status, seit Phase 3 zusätzlich "was wurde geblockt"/"warum wurde X angehalten") und antwortet templated anhand von echtem, live gelesenem `SystemContext`. Kein LLM, kein generativer Text, daher auch keine Halluzinationsgefahr.
- `ai/mistral.rs`: Phase-2-Backend `MistralBackend` — direkter `reqwest`-Aufruf (rustls, kein System-OpenSSL) gegen `https://api.mistral.ai/v1/chat/completions`, Key aus der `Vault` (`MISTRAL_API_KEY`), Retry/Backoff bei 429/5xx, fester Rate-Limiter (0.8 req/s); der System-Prompt trägt seit Phase 3 auch die Security-Event-Zusammenfassung, damit Fragen dazu geerdet statt halluziniert beantwortet werden.
- `ai/fallback.rs`: `FallbackBackend` — Mistral zuerst, bei Fehlschlag transparenter Rückfall auf `HeuristicBackend`. `AiState::from_vault` wählt beim Start automatisch: Key vorhanden → Fallback-Kette, sonst reine Heuristik (kein Absturz ohne Key).
- `ai/tuning.rs`: eigener `tokio::spawn`'ter Endlos-Task (analog zum eBPF-RingBuf-Poller), pollt alle 30s RAM- und Gaming-Mode-Status und pusht bei einer einfachen Regel-Verletzung (z. B. RAM knapp bei inaktivem Gaming-Mode) einen Vorschlag in `AiState`s Suggestions-Queue.
- `security/events.rs` (Phase 3, siehe `tarno-guard-ebpf`/Behavioral-Security unten): beschränkter FIFO-Event-Log (50 Einträge), den `security::ebpf_loader::run` befüllt und `SystemContext::gather` nur lesend konsumiert — additiv, ohne Einfluss auf die SIGSTOP-Policy-Entscheidung.
- IPC: `Request::AiQuery{text}`, `Request::AiStatus` (inkl. `mistral_configured`/`mode_note`/`recent_security_stops`/`last_stopped_comm`/`last_stopped_filename`), `Request::AiSuggestions` in `tarnod-protocol`, per `tarnoctl ai <status|suggestions|<frage...>>` erreichbar.
- Details, Testabdeckung und Setup-Anleitung: [`month3-tarno-layer.md`](month3-tarno-layer.md#tarno-ai).

### `tarno-guard-ebpf` (Behavioral Security)
- Eigenständiger 3-Crate-Workspace (Kernel-Space-Programm + Common-Types + Userspace-Loader-Lib), von `tarnod` als optionales Cargo-Feature `ebpf` eingebunden.
- Hook: Tracepoint `sched_process_exec` (kein LSM-Hook in Phase 1 — robuster, keine `CONFIG_BPF_LSM`-Abhängigkeit im Kernel).
- Datenfluss: eBPF-Programm schreibt `ExecEvent{pid, uid, comm, filename}` in eine `RingBuf`-Map → Userspace liest asynchron → Policy-Engine (Allow-/Deny-Liste aus Config) entscheidet → bei Treffer `kill(pid, SIGSTOP)` aus Userspace (**nicht** aus eBPF selbst, siehe Begründung in [`month3-tarno-layer.md`](month3-tarno-layer.md#warum-sigstop-aus-userspace)). Additiv (Tarno-AI-Phase-3): jedes ausgewertete Event wird zusätzlich, rein protokollierend, in `security::events::SecurityEventLog` gepusht (50 Einträge FIFO), damit `AiQuery`/`AiStatus` darüber reden können — ohne Einfluss auf die Policy-/SIGSTOP-Entscheidung oben.

### Gaming-Mode-Skripte
- `scripts/gaming-mode.sh`: schaltet CPU-Isolation/Governor/THP um, unabhängig von `tarnod` aufrufbar (z. B. manuell oder aus einem Login-Hook).
- `scripts/jvm-launch.sh`: Wrapper, der die JVM mit passender Core-Affinität und Priorität startet.
- Diese Skripte sind bewusst **nicht** in `tarnod` verdrahtet (kein IPC-Aufruf nötig) — einfache, auditierbare Shell-Skripte statt zusätzlicher Daemon-Logik für einen einmaligen Vorgang beim Spielstart.

### Buildroot-Integration (`tarno-br2-external/`)
- `BR2_EXTERNAL`-Tree mit eigenem Package `tarnod` (cargo-package-Infra) und Board-Definition `tarno-m6700` (Kernel-Config-Fragment, Rootfs-Overlay).
- Das eBPF-Objekt wird **vorkompiliert** eingebettet (nicht im Buildroot-Cross-Build erzeugt), um Host-BPF-Toolchain-Bootstrapping im Cross-Build zu vermeiden. Details: [`month1-foundation.md`](month1-foundation.md#woche-1-2-basis-system).

## Sicherheitsmodell (Userspace-Isolation statt Kernel-Vault)

Kein eigener Kernel-Vault nötig, weil:
1. API-Keys liegen ausschließlich im RAM des `tarnod`-Prozesses (aus einer root-only `0600`-Datei beim Start geladen, danach nicht erneut von Platte gelesen).
2. Zugriff nur über den Unix-Socket, der selbst `0600` ist und zusätzlich per `SO_PEERCRED` die UID/GID des anfragenden Prozesses verifiziert (doppelte Absicherung gegen falsch gesetzte Dateirechte).
3. Kein anderer Prozess auf dem System kann den Speicher von `tarnod` ohne `CAP_SYS_PTRACE`/root lesen — das ist auf Tarno OS ohnehin nur der Daemon selbst.
