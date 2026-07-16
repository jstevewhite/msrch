# Changelog

Notable changes to msrch. Versions follow SemVer 0.x — **minor** for features
or index-compatibility changes, **patch** for fixes. Every release is a git tag
(`vX.Y.Z`); `msrch --version` prints the semver, index schema version, and the
commit the binary was built from.

## [0.4.0] - 2026-07-16

### Added
- **Query filters**: `--path <substring>` (matches anywhere in the file path;
  SQL LIKE wildcards pass through), `--after` / `--before` (file modification
  time; `YYYY-MM-DD` or relative `7d`/`2w`/`3m`; after-inclusive,
  before-exclusive). Filters compose with `--rerank` — reranking now runs on
  the filtered candidates.
- **Auto-index**: set `query.auto_index = true` in a repo's
  `.msrch/config.toml` and every query refreshes the index first — quietly
  (one status line only when files changed) and non-fatally (endpoint down →
  warning + stale results, never a failed query). `--no-auto-index` skips it.
- `docs/AGENTS-SNIPPET.md`: copy-paste msrch usage block for agent-driven repos.

No index schema change — existing indexes work as-is.

## [0.3.0] - 2026-07-15

### Added
- **Document extraction**: HTML, text-layer PDF, and .docx files are now
  indexed. HTML gets readability-style main-content extraction (with automatic
  whole-page fallback for non-article pages); docx headings map to markdown
  headings for structure-aware chunking; PDFs index their text layer.
  Graphics-only PDFs are skipped with a warning — no OCR.
- `max_file_size_mb` (config, default 10) is now enforced for extractable
  document types.

### Changed
- Index schema bumped to v5: `.html` files were previously indexed as raw
  markup; **run `msrch reindex` after upgrading** to replace tag-soup chunks
  with extracted text.

## [0.2.0] - 2026-07-15

### Changed
- **Workspace split**: the single crate is now `msrch-core` (library: index,
  search, config, db, embedding, reranker, chunker, crawler) plus `msrch`
  (CLI). All logic lives in core; the CLI is a thin front-end. This is the
  groundwork for the planned MCP server front-end.
- **lancedb 0.23 → 0.31** (lance storage engine 1.x → 8.x), forced by
  rustc 1.95 compatibility. Index schema bumped to v4:
  **run `msrch reindex` after upgrading** — pre-0.2.0 indexes are wiped and
  rebuilt on the next index/reindex; a bare `query` against an old index
  errors until then.
- `msrch stats` reports the effective (project-overlaid) config instead of the
  global config.
- `msrch similar`: embedding failures now exit non-zero (previously printed to
  stderr and exited 0); the "Finding files similar to" header prints after
  index discovery, so a missing index shows only the error.
- `msrch --version` now prints the index schema version and build commit.

### Added
- **Project-level config**: `.msrch/config.toml` in an index root overlays the
  global config field-by-field. Precedence: CLI flags > project > global >
  defaults.
- **Reranker API key support** (bearer auth): `reranker.api_key` in config.
- `msrch reindex` preserves a project `config.toml` (previously the whole
  `.msrch/` directory was deleted).

### Fixed
- UTF-8 panic in `msrch similar` on multibyte files larger than 8000 bytes.

## [0.1.0]

Initial: per-directory semantic code/document search — tree-sitter chunking
with semantic context paths, OpenAI-compatible embeddings, LanceDB storage,
optional cross-encoder reranking, incremental mtime-based reindexing, git-like
index discovery.
