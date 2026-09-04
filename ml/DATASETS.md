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

## Planned additions (P1+, not yet wired)

| Source | Licence | Notes / obligations |
|---|---|---|
| Simple English Wikipedia (`simplewiki` dump) | CC BY-SA 4.0 | Attribution + **share-alike**: any distributed *text derived from it* must stay CC BY-SA. Weights are a separate legal question, tracked when we get there. Needs a markup stripper (`wikiextractor`-class). |
| Project Gutenberg — full mirror subset | Public domain | Larger book set; same header-stripping rule. |
| FineWeb / FineWeb-Edu (HF) | ODC-BY | Documented-filtered Common Crawl. Attribution to the dataset; respect Common Crawl's terms. This is the kind of open web corpus the big labs also use. |
| The Stack v2 (HF, `bigcode`) | per-file OSS licences + opt-out list | Code. Must honour the maintainer **opt-out** list and keep per-file licence metadata. |
| arXiv bulk (S3 requester-pays) / PubMed Central OA | mixed CC / arXiv licence | Per-paper licence varies; filter to CC-BY / CC0 / arXiv-perpetual before use. |
| StackExchange data dump | CC BY-SA 4.0 | Same share-alike as Wikipedia; attribution to contributors + SE. |

## When weights are distributed

Open question, parked until a model is actually worth sharing: whether CC BY-SA
source text imposes share-alike on the *weights* is unsettled. Until decided,
treat any release as if it does — publish the training recipe + this file
alongside, and prefer public-domain / permissive sources for anything meant to be
redistributed. See `docs/thos/ai.md` → Open decisions.
