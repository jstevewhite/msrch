# Document Extraction Pipeline — Design (Roadmap Item 2)

*Approved 2026-07-15. Targets release 0.3.0.*

## Purpose

Let msrch index HTML, text-layer PDF, and .docx documents by extracting their
text before chunking. This widens the validated use case (shell-only agents and
humans searching mixed document repos) and fixes a live defect: `.html` files
are currently indexed as raw tag soup (`FileType::Unknown` → `split_default`),
polluting indexes with markup embeddings.

**Scope: exactly three formats.** HTML (+`.htm`/`.xhtml`), PDF (text layer
only), `.docx`. See the YAGNI ledger for everything deliberately excluded.

## Architecture

One new module owns all format knowledge: `crates/core/src/extract.rs`.

```
crawler → [extract] → chunker → embedder → db
              ↑
   only hook points: crawler's binary filter (whitelist .pdf/.docx),
   indexer's read site (extract() instead of read_to_string for extractables)
```

Public API:

```rust
/// True for extensions this module handles: html, htm, xhtml, pdf, docx.
pub fn is_extractable(path: &Path) -> bool;

/// Extract indexable text. Ok(Some(text)) → index it. Ok(None) → file was
/// skipped (reason already warned to stderr): no text layer, over the size
/// cap, or unparseable. Err → unexpected I/O failure (caller warns + skips,
/// consistent with existing unreadable-file handling).
pub fn extract(path: &Path, max_bytes: u64) -> Result<Option<String>>;
```

Hook changes (the only edits outside the new module):

- **`index.rs` read site (~line 260):** if `is_extractable(path)` call
  `extract(path, max_file_size_mb * 1024 * 1024)`; `None` → skip file;
  otherwise proceed with the returned text exactly as read content is used
  today. Non-extractable files keep the current `fs::read_to_string` path.
- **`crawler.rs::is_binary`:** return `false` early for `.pdf`/`.docx` so the
  null-byte check doesn't drop them. (HTML already passes as text.)
- **`chunker.rs::determine_file_type`:** map `html|htm|xhtml|docx` →
  `FileType::Markdown` and `pdf` → `FileType::Prose`. The chunker receives
  pre-extracted text; markdown-ish output from extraction gets the existing
  heading/paragraph-aware `split_markdown` for free. No other chunker changes.

The CLI is untouched. A future MCP front-end inherits extraction through core.

## Extractors

### HTML → markdown-ish text (readability + fallback)

1. Parse and run readability-style main-content extraction
   (crate: `dom_smoothie`; if it fails health checks at plan time, implement
   the same walk over `scraper`'s DOM).
2. Walk the cleaned DOM emitting markdown-ish text: `h1`–`h6` → lines prefixed
   with 1–6 `#` characters; block elements (`p`, `li`, `blockquote`, `pre`,
   table rows) → text blocks separated by blank lines; scripts/styles dropped.
3. **Degenerate-extraction fallback:** compute whole-page tag-stripped text.
   If main-content text is `< 200` chars OR `< 5%` of whole-page text length,
   use the whole-page text instead (same markdown-ish emission, headings
   preserved). Covers dashboards, index pages, framesets.

### .docx → markdown-ish text (zip + XML)

- `zip` crate to open the archive, `quick-xml` to stream `word/document.xml`.
- `w:p` = paragraph; a `w:pStyle` of `Heading1`–`Heading6` (also `Heading 1`
  spelling variants and `Title` → `#`) prefixes the paragraph with matching
  `#` count. `w:t` runs concatenate; `w:tab` → single space.
- Tables: one line per row, cells joined with ` | `.
- Anything unrecognized is ignored, not an error. Empty result → skip with
  warning ("no extractable text").
- Deliberately NOT using `docx-rs` (writer-oriented, heavy); `zip` +
  `quick-xml` are small, mature, and we need read-only access to one XML file.

### PDF → plain prose (text layer only)

- `pdf-extract` crate, whole-document text extraction, chunked as `Prose`
  (PDFs carry no reliable heading structure — no markdown mapping).
- **Graphics-only heuristic:** trimmed extracted text `< 200` chars for the
  whole document → `Ok(None)` with warning `"skipping <path>: no text layer"`.
  No OCR, no vision models, ever (out of scope by design).

### Size gate (all three formats)

Before reading/parsing: if file metadata size exceeds `max_file_size_mb`
(existing `ChunkingConfig` field, default 10 MB, currently unwired), skip with
a warning. This wires the config field for extractable types only; regular
text files keep current behavior (v1 scope decision).

## Failure semantics

Per-file, never per-run. Every failure mode (parse error, no text layer,
oversize, I/O error) warns to stderr and skips the file; indexing continues.
A corrupt PDF must not abort an index run. Warning style matches the indexer's
existing unreadable-file messages.

## Versioning & migration

- **`SCHEMA_VERSION` 4 → 5.** Existing `.html` chunks contain raw markup with
  correspondingly wrong embeddings; mtime-based incremental reindexing would
  never refresh them (files unchanged). The bump forces wipe-and-rebuild on the
  next index/reindex. Changelog comment: "v5: HTML/PDF/docx extraction — .html
  content semantics changed from raw markup to extracted text."
- **Release 0.2.0 → 0.3.0** per the versioning policy (feature + index-compat
  change): bump `workspace.package.version`, CHANGELOG entry (including "run
  `msrch reindex` after upgrading"), `git tag v0.3.0`.

## Dependencies (new, core only)

`dom_smoothie` (or `scraper` fallback), `pdf-extract`, `zip`, `quick-xml`.
All compile-time-verified at plan time (`cargo info`, then `cargo build`);
exact versions pinned in the plan, added to `[workspace.dependencies]`.

## Testing

Fixtures in `crates/core/tests/fixtures/`:

| Fixture | Kind | Asserts |
| --- | --- | --- |
| `saved-page.html` | nav + sidebar + `<article>` with headings | readability keeps article, drops nav; headings become `#` lines |
| `nav-only.html` | degenerate page (links/nav, no main content) | fallback to whole-page text triggers |
| `text-layer.pdf` | tiny committed binary (~1–2 KB), real text layer | text extracted, chunked as prose |
| `graphics-only.pdf` | tiny committed binary, no text layer | `Ok(None)` + "no text layer" warning |
| (docx built in-test) | test helper zips a minimal `word/document.xml` | heading styles → `#`, runs concatenated, table rows flattened |

Test layers:

1. **Unit** (per extractor): heading mapping, degenerate fallback trigger,
   graphics-only skip, size gate, empty-docx skip.
2. **Integration:** `chunk_file` on extracted output produces heading-aligned
   chunk boundaries (`FileType::Markdown` routing verified end to end).
3. **End-to-end (no network):** crawl + extract + chunk a tempdir containing
   all fixtures — the pipeline up to but excluding embedding — asserting
   per-file chunk presence/absence and that no produced chunk contains `<div`.
   The full index-with-embeddings smoke test happens manually against a real
   repo after implementation.

## YAGNI ledger (explicitly not in v1)

- OCR / vision models for graphics-only PDFs
- `.pptx`, `.xlsx`, legacy `.doc`, RTF
- Extracted-text disk cache (`.msrch/extracted/` — would enable grep-over-PDF;
  revisit as its own roadmap note; purely additive later)
- Heading context-paths in the `context` column for documents (stays empty,
  as markdown chunks are today)
- New config keys (readability thresholds are hardcoded constants)
- Size gate for non-extractable file types

## Success criteria

- `msrch index` on a repo containing the fixture formats indexes HTML/PDF/docx
  content searchably, skips graphics-only PDFs with a clear warning, and never
  aborts on a corrupt document.
- Querying an indexed saved-HTML page returns article text, not markup.
- All existing tests keep passing; suite grows with the extractor tests.
- `msrch --version` reports 0.3.0 / schema v5; reindex-after-upgrade documented.
