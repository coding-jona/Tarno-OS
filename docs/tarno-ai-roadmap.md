# Tarno AI — vom Mistral-Einzeiler zum echten System-Assistenten

Tarno OS heißt Tarno OS, weil "Tarno" mehr werden soll als ein Tab in den
Settings, der eine Frage an Mistral weiterreicht. Der Namensgeber ist
[`coding-jona/tarno`](https://github.com/coding-jona/tarno) - ein
ausgereiftes, aktiv entwickeltes Windows-Projekt (Python-Backend +
WinUI-3-Frontend über gRPC), das genau das schon ist: ein
sprachfähiger, werkzeugbenutzender, langzeit-erinnernder System-Assistent.
Dieses Dokument fasst zusammen, wie `tarno` aufgebaut ist, was davon
1:1 auf Tarno OS' Go/OpenRC/Wayland-Stack übertragbar ist, und in welcher
Reihenfolge wir das tatsächlich bauen.

Kein Code hier - reine Bestandsaufnahme + Fahrplan. Stand: architektonische
Sichtung von `coding-jona/tarno` (Python-Backend, `src/tarno_backend/`,
größtenteils dokumentiert über eigene `*_README.md` pro Modul).

## Wie `tarno` aufgebaut ist

- **Kern**: `TarnoEngine` (`core/engine.py`) ist die Ausführungsschicht,
  an die alles andere andockt. Autonome/kognitive Features (proaktive
  Briefings, Aufgabenplanung, Erinnerungen) sind bewusst **separate
  Schichten obendrauf**, nicht in die Engine selbst gefaltet - eine
  explizite Architekturregel in ihrem `CLAUDE.md`.
- **Frontend-Anbindung**: gRPC, an `127.0.0.1`/`[::1]` gebunden (nicht
  Wildcard - das war mal ein Sicherheitsloch, siehe ihr TD-018), zwischen
  Python-Backend und WinUI-Frontend.
- **LLM-Provider**: `provider.py` (Interface) + `factory.py` (baut einen
  Provider oder eine Fallback-Kette aus Config + Secrets) + sieben
  konkrete Implementierungen (Mistral, Claude, Gemini, Groq, HuggingFace,
  Ollama, generisches OpenAI-kompatibel). API-Keys werden **nie** direkt
  aus `os.environ` in den Client-Klassen gelesen, sondern zentral über
  eine `SecretsVault`-Abstraktion aufgelöst (OS-Keyring primär,
  verschlüsselte Datei als Fallback-Tier).
- **Tool-Nutzung / Systemzugriff**: `command_engine.py` +
  `command_tool.py` + `executor.py` ist die eigentliche
  Befehlsausführung, **risikogestuft**, mit `permission_service.py` als
  Bestätigungsschicht für riskante Aktionen (in der GUI ein echter
  Dialog, nicht `input()` - das hing früher im Headless-Modus, TD-025).
  Dazu ein Zero-Trust-Content-Filter (`security/content_filter.py`), der
  Tool-Output/Web-Inhalte als nicht vertrauenswürdig behandelt, bevor sie
  zurück in einen Prompt dürfen.
- **Gedächtnis**: SQLite + lokale ONNX-Embeddings, komplett CPU-only,
  keine externen Dienste - Fakten, Präferenzen, semantische Suche.
- **Sicherheit**: `SecretsVault` (Keyring/verschlüsselte Datei), PII-
  Redaction in Logs, Encryption-at-rest, manipulationssicheres Audit-Log
  (Hash-Chaining), Build-Zeit-Secret-Scanner.
- **Integrationen** (`integrations/`): Discord-PTT, Git, Kalender/E-Mail,
  Smart Home, Minecraft-Voice - und als mit Abstand größte: **Tarno
  Mesh** (`integrations/mesh/`) - Multi-Geräte-Betrieb (PC, Handy,
  ESP32), eigener eingebetteter MQTT-Broker, UDP-Telemetrie,
  Presence/Heartbeat, ein 4-Szenarien-Hub-Failover, und ein
  Read-only-Client zu einem separaten `tarno-server`-FastAPI-Backend -
  **das ist das Account-System**, das später (nach VPS-Setup) hierher
  übernommen werden soll, siehe Notiz im Hauptchat.
- **Sprache**: Wakeword → STT (faster-whisper/Vosk) → LLM → TTS (Piper
  lokal, edge-tts als Netzwerk-Fallback) - **bewusst vorerst nicht**
  Teil dieses Fahrplans, siehe unten.

## Was direkt übertragbar ist (Muster, nicht Code - andere Sprache/OS)

| `tarno` (Python/Windows) | Tarno OS (`tarno/`, Go/Linux) |
|---|---|
| `TarnoEngine` | `TarnoD` (`tarno/tarno.go`) - existiert schon als der zentrale Dispatcher |
| gRPC auf `127.0.0.1` | Unix-Socket `/run/tarnod.sock`, `0666` - für ein Single-User-Image sogar einfacher/passender als loopback-TCP |
| `provider.go`-Interface (`Query(text) (string, error)`) | **existiert schon 1:1**, gleiche Idee wie ihr `provider.py` |
| `factory.py` (Provider aus Config+Vault bauen) | fehlt noch - aktuell nur `MISTRAL_API_KEY`-Env-Var, kein Vault, kein Fallback |
| `SecretsVault` (Keyring primär, verschlüsselte Datei als Fallback-Tier) | kein Desktop-Keyring auf diesem minimalen Image (kein gnome-keyring/kwallet) - wir fangen direkt bei ihrem **Fallback-Tier** an: root-only-Datei (`0600`), von `tarnod` (läuft als root) verwaltet |
| `command_engine.py` + `permission_service.py` (risikogestufte Tool-Nutzung mit Bestätigung) | **noch nicht gebaut** - das ist der Kern von "voller Systemzugriff" und darf nicht übersprungen werden, siehe Phase 3 unten |
| `memory/` (SQLite + Embeddings) | noch nicht gebaut |
| `mesh/` (Tarno Mesh, Account-System) | bewusst zurückgestellt (Nutzer-Entscheidung, wartet auf VPS-Infrastruktur) |
| Sprachpipeline (`voice/`) | **bewusst vorerst raus** (Nutzer-Entscheidung) |

## Phasenplan

**Phase 1 - Konfigurierbarer Provider (gerade in Arbeit)**
- `tarnod`: API-Key nicht mehr nur `MISTRAL_API_KEY`-Env, sondern
  persistent in einer root-only Datei (`/etc/tarnod/mistral_api_key`,
  `0600`) - dem Fallback-Tier von `SecretsVault` nachempfunden, ohne
  Desktop-Keyring-Abhängigkeit.
- Neuer socket-Befehl `set_api_key`, live-reload des Providers ohne
  Neustart (`sync.RWMutex` um den Provider, wie ihr Vault auch zur
  Laufzeit austauschbar ist).
- `tarno-settings`: API-Key-Eingabe + Status ("konfiguriert"/"nicht
  konfiguriert") - Settings ist der richtige Ort dafür, matcht ihre
  eigene Regel "API keys are never entered into the app directly [i.e.
  in den Chat]".

**Phase 2 - Eigenständige Assistent-App ("wie das ehemalige Cortana")**
- Neue App `tarno-assistant` (PySide6, gleiche Design-Sprache wie
  `tarno-settings`/`tarno-store`): Chat-Verlauf statt Einzeiler-Frage/
  Antwort wie aktuell im AI-Tab.
- Schnellzugriff: eigener labwc-Keybind (analog zu Cortana/Spotlight),
  Root-Menü-Eintrag, eigenes `.desktop` (app_id-Fix wie bei
  `tarno-settings` nötig, sonst zeigt waybar wieder "Python (vX.Y)").
- `tarnod`s `ai`-Befehl bleibt der Transportweg - keine neue Engine
  nötig, nur ein besseres Frontend dafür.

**Phase 3 - Tool-Nutzung / echter Systemzugriff (der eigentliche
"voller Systemzugriff"-Teil - NICHT ohne Schutzschicht)**
- Das ist der Teil, wo `tarno`s Vorbild am wichtigsten ist: nicht "LLM
  darf `exec()`", sondern risikogestufte Befehle
  (`command_engine.py`-Äquivalent) + explizite Bestätigung für alles
  Riskante (`permission_service.py`-Äquivalent, in `tarno-assistant`
  als echter Dialog) + Tool-Output wird nicht blind zurück in den
  Prompt gespeist (Zero-Trust-Content-Filter-Äquivalent).
- Erste, eng umrissene Tools zuerst (z.B. `tarnod status` abfragen,
  Paketliste/Systeminfo lesen - alles read-only), destruktive Aktionen
  (Paketinstallation, Dateioperationen) erst nach dem
  Bestätigungsmechanismus, nicht vorher.

**Phase 4 - Gedächtnis**
- SQLite + lokale Embeddings, wie im Vorbild - erst sinnvoll, wenn
  Phase 2/3 stehen und es tatsächlich wiederkehrende Konversationen
  gibt, die von Kontext profitieren.

**Zurückgestellt, bewusst nicht in diesem Fahrplan**
- Sprachpipeline (Wakeword/STT/TTS) - Nutzer-Entscheidung, kommt später.
- Tarno Mesh / Account-System - wartet auf VPS-Infrastruktur, siehe
  `integrations/mesh/` oben für den Umfang, der später übernommen wird.
- Die übrigen Integrationen (Discord, Minecraft, Smart Home,
  Kalender/E-Mail) - nichts davon ist für ein Betriebssystem-Kernfeature
  nötig, eher etwas für `tarno-store`/Plugins später, falls überhaupt.

## Warum nicht einfach den Code portieren

`tarno` ist Windows-spezifisch (WinUI 3, Win32-Job-Object-Sandboxing,
OS-Keyring über Windows-APIs) und riesig (voice, memory, plugins, mesh,
vision, browser-control, ~150 Python-Dateien). Eins-zu-eins-Portierung
wäre selbst ohne die Windows-Abhängigkeiten ein eigenes Monate-Projekt.
Der Wert hier liegt in den **Mustern** (Engine/Bridge-Trennung,
Provider-Factory, vor allem die risikogestufte
Tool-Ausführung+Bestätigung) - die sind es wert, in Go/PySide6
nachgebaut zu werden, nicht der Code selbst.
