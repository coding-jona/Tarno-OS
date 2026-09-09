# THOS `ml/` — datasets & provenance

**Rule:** training data is **only** open / permissively-licensed / public-domain,
and every source is recorded here with its licence and obligations before it is
used — the same discipline `docs/thos/licensing.md` applies to `third_party/`.

**Not allowed, ever:**
- Proprietary training mixes from other AI providers (not public — nothing to use).
- Known-pirated corpora: Books3, the-eye / shadow-library dumps, LibGen /
  Z-Library / Anna's Archive scrapes, or anything derived from them.
- Scraped commercial AV verdicts / labels as ground truth (for the later
  exec-gate application).

Downloaded corpora live in `train/data/` and are **git-ignored**. `train/fetch.py`
keeps a SHA-256 manifest (`train/data/manifest.json`) so a re-run reproduces the
exact bytes.

## v0 corpus (P0 spike)

| Source | What | Licence | Obligations |
|---|---|---|---|
| Project Gutenberg | ~12 English public-domain novels (`train/fetch.py` `SOURCES`) | Public domain in the US (works pre-1929). The Gutenberg **trademark licence** covers only the added header/footer. | `train/prepare.py` strips the `*** START/END OF THE PROJECT GUTENBERG ***` boilerplate so nothing but the public-domain text remains. Do not redistribute with the Gutenberg header or the "Project Gutenberg" name attached. |

Total ≈ 5–10 MB — fits one nightly internet window.

## P1b/P1c corpus (active — the `staged.sh` run)

| Source | What | Licence | Obligations |
|---|---|---|---|
| Project Gutenberg (English) | ~80 English public-domain novels/plays/philosophy (`train/fetch.py` `SOURCES`, P1/P1b blocks) | Public domain in the US. Trademark licence covers only header/footer. | Same header-stripping as above. |
| Project Gutenberg (German) | 10 German-language public-domain classics — Goethe (*Faust*, *Werther*), Grimm (*Kinder- und Hausmärchen*, original German), Nietzsche (*Also sprach Zarathustra*), Kafka (*Die Verwandlung*, *Der Prozess*), Kant (*Kritik der reinen Vernunft*), Schiller (*Wilhelm Tell*), Heine (*Buch der Lieder*), Fontane (*Effi Briest*) — IDs prefixed `de_` in `SOURCES` | Public domain in the US (pre-1929 / author's death 70+ years ago). Same Gutenberg trademark carve-out. | Same header-stripping. Ebook IDs were looked up and confirmed against gutenberg.org before adding — see commit history for the search trail. |

Total ≈ 70–100 MB (English) + a few MB (German) — the German slice is intentionally
small relative to English for now (~10 titles vs. ~90); it gives the tokenizer and
model *some* real German exposure (umlauts, grammar, vocabulary) rather than none,
without claiming this makes the model fluent in German. A byte-level BPE
tokenizer needs no special handling for German — the merge learner just sees
UTF-8 bytes and will pick up German-specific merges (e.g. `ü`, `sch`, `-ung`) on
its own from whatever fraction of the corpus is German. Growing that fraction is
future work if German output quality matters more than a first proof of it.

## Stage-2 corpus (active — `fetch_web.py` + `prepare_web.py`, `data_stage2/`)

The real P1 pretraining mix for the ~34M model: ~1.2B tokens, streamed from
Hugging Face (`streaming=True`, capped per source, never fully downloaded).
All open-licensed. `data_stage2/` is git-ignored. Token counts are the
`fetch_web.py` budgets (bytes/4 estimate).

| Source | HF path | Budget | Licence | Obligations |
|---|---|---|---|---|
| TinyStories | `roneneldan/TinyStories` | 350M | CDLA-Sharing-1.0 (synthetic) | none beyond attribution; front-loaded to teach fluency |
| FineWeb-Edu (sample-10BT) | `HuggingFaceFW/fineweb-edu` | 600M | **ODC-By-1.0** | attribute the dataset; respect Common Crawl terms |
| Wikipedia (en, 2023-11-01) | `wikimedia/wikipedia` | 150M | **CC-BY-SA-4.0** | attribution + **share-alike** on distributed derived text (weights TBD — see below) |
| Cosmopedia v2 | `HuggingFaceTB/smollm-corpus` | 120M | Apache-2.0 (synthetic) | attribution |

`prepare_web.py` re-learns a byte-BPE (vocab 16384) on a sample across all four,
so the Stage-2 model has its **own** `data_stage2/tokenizer.json` and is not
`--resume`-able from the P0 `small-30m` checkpoint. Config:
`config/small-30m-stage2.toml` (ctx 512, 120k steps).

## Planned additions (later, not yet wired)

| Source | Licence | Notes / obligations |
|---|---|---|
| Project Gutenberg — full mirror subset | Public domain | Larger book set; same header-stripping rule. |
| The Stack v2 (HF, `bigcode`) — permissive-licensed Python only | per-file OSS licences + opt-out list | Code. Must honour the maintainer **opt-out** list and keep per-file licence metadata. Was in the Stage-2 plan but deferred to keep the first real run simple. |
| arXiv bulk (S3 requester-pays) / PubMed Central OA | mixed CC / arXiv licence | Per-paper licence varies; filter to CC-BY / CC0 / arXiv-perpetual before use. |
| StackExchange data dump | CC BY-SA 4.0 | Same share-alike as Wikipedia; attribution to contributors + SE. |

## When weights are distributed

Open question, parked until a model is actually worth sharing: whether CC BY-SA
source text imposes share-alike on the *weights* is unsettled. Until decided,
treat any release as if it does — publish the training recipe + this file
alongside, and prefer public-domain / permissive sources for anything meant to be
redistributed. See `docs/thos/ai.md` → Open decisions.
