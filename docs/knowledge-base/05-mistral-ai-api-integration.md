# Mistral AI API — Recherche für Tarno-AI-Phase-2

Wie [`04-tarno-os-debian-migration-notes.md`](04-tarno-os-debian-migration-notes.md)
für die Debian-Migration: reine Recherche, **kein Code geändert**. Diese
Datei entsteht bewusst *vor* der Phase-2-Implementierung von Tarno AI
(siehe [`../month3-tarno-layer.md`](../month3-tarno-layer.md#phase-2-nicht-umgesetzt-nur-dokumentiert-mistral-ai-api-backend)),
weil Phase 1 (heuristisches Backend) keinerlei Netzwerk-/API-Wissen
brauchte und Phase 2 das grundlegend ändert.

**Kurskorrektur gegenüber der ursprünglichen Phase-2-Planung:** die erste
Fassung dieses Dokuments (inzwischen überholt) ging von einem lokal
laufenden, quantisierten LLM aus (`candle`, GGUF, 1-3B Parameter). Das ist
verworfen. Tarno AI läuft stattdessen auf **Mistral-AI-Cloud-Modellen über
deren REST-API**, angesprochen mit einem API-Key — kein lokales Modell,
keine GPU-/RAM-Fragen auf der M6700-Zielhardware mehr relevant für die
Sprachfähigkeit selbst (wohl aber für Latenz/Netzwerkverfügbarkeit, siehe
unten).

## API-Grundlagen

- **Base-URL**: `https://api.mistral.ai/v1`
- **Endpoint**: `POST /v1/chat/completions` (OpenAI-kompatibles Schema:
  `model`, `messages: [{role, content}]`, optional `temperature`
  (empfohlen 0.0–0.7), `top_p`, `max_tokens`, `stream`)
- **Auth**: HTTP-Header `Authorization: Bearer $MISTRAL_API_KEY` — kein
  Custom-Scheme, Standard-Bearer-Token wie bei den meisten REST-APIs.
- **Response**: JSON mit `id`, `object`, `model`, `created`,
  `choices: [{message: {role, content}, finish_reason, ...}]`,
  `usage: {prompt_tokens, completion_tokens, total_tokens}`.
- **Streaming**: `stream: true` liefert Server-Sent-Events statt einer
  einzelnen JSON-Antwort — für Phase 2 zunächst nicht nötig (`tarnoctl ai
  <frage>` ist ein einzelner Request/Response-Zyklus über den bestehenden
  Unix-Socket, kein interaktiver Stream); als spätere Ausbaustufe denkbar,
  falls `tarnoctl` irgendwann ein interaktives Chat-REPL bekommt.

Quelle: [Mistral API — Chat Endpoints](https://docs.mistral.ai/api/endpoint/chat),
[Mistral API Reference](https://docs.mistral.ai/api).

## Modelle & Kosten (Stand der Recherche, Anfang 2026)

Drei kommerzielle Modellstufen: **Large 3** (komplexes Reasoning, ~$2/$6
pro 1M Input-/Output-Tokens), **Medium 3** (ausgewogen), **Small 3.1**
(hoher Durchsatz, günstig, ~$0.20/$0.60 pro 1M Tokens). Für Tarno AIs
Anwendungsfall (kurze System-Status-Fragen, keine langen Dokumente) ist
**Small 3.1 die naheliegende Standardwahl** — Kosten pro Anfrage sind
minimal, Reasoning-Tiefe von Large/Medium wird für "ist Gaming-Mode an"-
artige Fragen nicht gebraucht. Modellname sollte konfigurierbar sein
(gleiche `Vault`/Config-Mechanik wie der API-Key selbst), nicht
hartkodiert — falls sich das für komplexere Phase-3-Security-Erklärungen
später ändert.

Kostenloser **"Experiment"-Tier**: 2 Requests/Minute, 1 Milliarde
Tokens/Monat, ausdrücklich nur für Evaluierung, nicht für Produktivbetrieb
gedacht — für Entwicklung/Testen von Phase 2 ausreichend, für einen
tatsächlich im Alltag laufenden Tarno-AI-Daemon auf Dauer vermutlich nicht
(2 RPM ist niedrig, sobald mehrmals am Tag echt gefragt wird).

Quelle: [Mistral Pricing 2026 (DevTk.AI)](https://devtk.ai/en/blog/mistral-api-pricing-guide-2026/),
[Mistral AI Free Tier 2026 (AgentDeals)](https://agentdeals.dev/vendor/mistral-ai).

## Rust-Anbindung

Drei existierende Crates gefunden, keine davon bisher im Workspace
verwendet — müsste für Phase 2 evaluiert/eingebunden werden:

- **`mistralai-client`** — deckt Chat (async, mit Streaming-Variante),
  Function-Calling, Embeddings, Model-Listing ab. Breitester
  Funktionsumfang.
- **`mistral-rouille`** — aus Mistrals offizieller OpenAPI-Spec generiert,
  nutzt intern `reqwest`.
- **`mistral-api`** — schlanker, bietet einen `ChatCompletion`-Typ direkt
  auf einem `reqwest::Client`.

Alle drei brauchen `reqwest` (async, `tokio`-basiert) — passt zum
bestehenden Stack (`tokio` ist schon Workspace-Dependency, siehe
`tarnod/Cargo.toml`). Ein direkter, minimaler `reqwest`-Aufruf ohne
Zusatz-Crate ist für den schmalen Bedarf von Phase 1→2 (ein einzelner
Chat-Completion-Call pro `AiQuery`) ebenfalls realistisch und würde eine
Abhängigkeit weniger bedeuten — Entscheidung Crate vs. Handrolled-Request
ist noch offen, keine Präferenz in dieser Recherche-Phase festgelegt.

Quellen: [crates.io/mistralai-client](https://crates.io/crates/mistralai-client),
[github.com/GovCraft/mistral-rouille](https://github.com/GovCraft/mistral-rouille),
[crates.io/mistral-api](https://crates.io/crates/mistral-api).

## Anbindung an Bestehendes im Repo

- **API-Key-Speicherung**: die existierende `Vault`
  (`tarnod/tarnod/src/vault.rs`) ist bereits eine generische
  `KEY=VALUE`-Datei, root-only, einmalig beim Start gelesen, danach nur im
  Prozessspeicher (siehe Doc-Kommentar im Modul, existierender Test
  `parses_simple_keys` nutzt exemplarisch schon `MOJANG_API_KEY`). Ein
  `MISTRAL_API_KEY`-Eintrag passt ohne jede Code-Änderung an `vault.rs`
  selbst hinein — nur `ai/backend.rs`/ein künftiges LLM-Backend müsste
  `vault.get("MISTRAL_API_KEY")` abfragen.
- **`AiBackend`-Trait** (`tarnod/tarnod/src/ai/backend.rs`, Phase 1 bereits
  gebaut): ist bewusst so geschnitten (`fn answer(&self, question: &str,
  ctx: &SystemContext) -> String`), dass ein `MistralBackend`-Impl ohne
  Umbau an `AppState`/`dispatch()` eingesetzt werden kann — der Trait
  bräuchte lediglich `async fn` statt `fn` (oder einen synchronen Wrapper
  um einen async-Call), da ein Netzwerk-Request naturgemäß nicht
  synchron/blockierend im IPC-Handler laufen sollte.
- **Kein lokales Modell mehr relevant**: die in der ursprünglichen
  Phase-2-Planung diskutierte `candle`-Abhängigkeit und die
  Hardware-Grenzen-Diskussion (M6700, Q4-Quantisierung, 1-3B Parameter)
  entfallen vollständig — das Modell läuft bei Mistral, nicht auf der
  Zielhardware. Was stattdessen real relevant wird: **Netzwerkverfügbarkeit**
  (Tarno OS' M6700-Zielsystem hat nicht garantiert immer Internet;
  `MistralBackend` braucht einen sauberen Fallback auf `HeuristicBackend`,
  wenn kein Netz da ist oder die API einen Fehler/Timeout liefert — nicht
  einfach eine Fehlermeldung an den Nutzer durchreichen) und **Latenz**
  (ein Cloud-Roundtrip ist langsamer als der lokale Heuristik-Pfad;
  `tarnoctl ai <frage>` sollte das im Response klar erkennbar machen,
  z. B. welches Backend geantwortet hat).

## Offene Fragen für die eigentliche Phase-2-Umsetzung (nicht hier beantwortet)

- Crate-Wahl (`mistralai-client` vs. eigener schlanker `reqwest`-Call).
- Konkretes Fallback-/Timeout-Verhalten (`HeuristicBackend` als
  Rückfallebene, wenn `MistralBackend` fehlschlägt — Reihenfolge, nicht
  nur Ersatz).
- Wie System-Kontext (`SystemContext`, siehe Phase 1) in den
  `messages`-Payload übersetzt wird (z. B. als System-Prompt mit
  eingebettetem Live-Status), ohne bei jeder Anfrage unnötig viele Tokens
  zu verbrauchen.
- Rate-Limit-Handling gegenüber dem 2-RPM-Experiment-Tier während der
  Entwicklung.

Diese Fragen sind bewusst offengelassen — sie gehören in die
Implementierungsarbeit selbst, nicht in die Recherche-Phase.
