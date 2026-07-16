# Query Ergonomics — Design (Roadmap Item 3)

*Approved 2026-07-16. Targets release 0.4.0. No index schema change (stays v5).*

## Purpose

Serve the report-workflow that msrch's primary use case validated: semantic +
temporal queries ("what did I write about X in March?") and always-fresh
results without anyone remembering to reindex. Three deliverables:

1. `--path`, `--after`, `--before` filters on `msrch query`
2. Staleness-guarded auto-reindex before query (per-project opt-in)
3. An agent-facing usage snippet (README section + copy-paste block)

## CLI surface

```
msrch query "budget concerns" --path 2026/07 --after 2026-07-01 --limit 5
msrch "what changed in the api?" --after 7d -f filename
msrch query "quarterly numbers" --before 2026-04-01 --no-auto-index
```

- `--path <s>` — substring match against the stored absolute file path
  (grep-like: `--path 2026/07` and `--path week-28` both work). SQL `LIKE`
  wildcards `%`/`_` in the input pass through as a documented bonus; single
  quotes are escaped (injection safety).
- `--after <d>` / `--before <d>` — filter on file modification time.
  Accepted forms: ISO date `YYYY-MM-DD`, or relative `Nd`/`Nw`/`Nm`
  (days/weeks/months ago; month ≈ 30 days, documented approximation).
  Bounds: `--after D` is inclusive (mtime ≥ D 00:00 local time);
  `--before D` is exclusive (mtime < D 00:00 local). Relative forms resolve
  to `now − N` (instant, not midnight-snapped). Invalid date strings are a
  CLI parse error (exit non-zero with a message showing accepted forms).
- `--no-auto-index` — skip the auto-index pass even where config enables it.
- All three filters + the escape flag are global args (work in implicit-query
  form, same pattern as `--limit`/`--format`/`--rerank`).
- Filters apply to `query` only — not `similar`, not `stats` (YAGNI).

## Core API: `SearchOptions`

`Searcher::search` currently takes `(text, limit, use_rerank)`; three more
positional args would be unwieldy and the future MCP front-end wants a
structured request. Consolidate:

```rust
/// Options for a search request. Front-ends build this; core executes it.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub limit: Option<usize>,          // None → config default_limit
    pub use_rerank: bool,              // OR'd with config reranker.enabled
    pub path_contains: Option<String>, // substring on stored file path
    pub after: Option<SystemTime>,     // inclusive lower bound on file mtime
    pub before: Option<SystemTime>,    // exclusive upper bound on file mtime
}

impl Searcher {
    pub async fn search(&self, query_text: &str, opts: &SearchOptions)
        -> Result<Vec<SearchResult>>;
}
```

Date-string parsing (`"2026-07-01"` / `"7d"` → `SystemTime`) lives in the CLI
(clap value parser), NOT in core — core takes resolved `SystemTime`s so the
MCP server can pass whatever its protocol delivers.

## Execution semantics (Approach A)

**Path filter — DB-side.** When `path_contains` is set, the LanceDB vector
search gets a predicate: `file_path LIKE '%<escaped>%'` (single quotes doubled;
`%`/`_` passed through). Pushed into the scan via the query builder's
`only_if` — no over-fetch needed for path-only filtering.

**Date filters — manifest join, post-filter, over-fetch.** When `after`/
`before` is set:

1. Load `.msrch/manifest.json` (same struct the indexer maintains) and build
   a `HashMap<PathBuf, SystemTime>` of file → mtime. Manifest read failure →
   `anyhow` error with context (a date filter without a manifest is
   unanswerable; do not silently return unfiltered results).
2. Over-fetch: `fetch_limit = max(limit × 10, 100)` — then, as today, merged
   with the reranker's `top_n` when reranking is enabled
   (`fetch_limit.max(top_n)`).
3. Post-filter hits: keep a result iff its file's manifest mtime satisfies
   `after ≤ mtime` (when set) and `mtime < before` (when set). A hit whose
   path is missing from the manifest is EXCLUDED when a date filter is active
   (debug-logged; shouldn't happen — every indexed chunk has a manifest entry).
4. Order of operations: vector search (with path predicate) → date post-filter
   → rerank (on the filtered survivors) → truncate to `limit`. Reranking
   after filtering means the cross-encoder never wastes budget on hits the
   filter would discard.

**Why not a schema column (Approach B):** exact DB-side date filtering needs
an mtime column → `SCHEMA_VERSION` 6 → full rebuild on every machine one
release after v5 forced one. Parked: fold an mtime column in whenever the
next schema bump happens for other reasons.

## Auto-reindex

- New config key: `query.auto_index` (bool, default `false`). Set it in a
  report repo's `.msrch/config.toml`; code repos never pay the crawl.
- When effective config has `auto_index = true` and `--no-auto-index` is not
  passed, the CLI Query arm runs the incremental index pass (the existing
  `Indexer::index` machinery) BEFORE constructing the search, against the
  discovered index root.
- **Quiet mode:** the indexer gains a quiet flag (plumbed from the auto-index
  call only; `msrch index`/`reindex` keep current output). Quiet mode draws
  no progress bars and prints exactly one line, only when work happened:
  `auto-index: refreshed N file(s)` — silent when the index was already fresh.
- **Non-fatal:** any auto-index failure (embedding endpoint down, I/O error)
  prints `warning: auto-index failed (<err>); searching the existing index`
  to stderr and the query proceeds against the stale index. A query must
  never fail because freshness maintenance failed.

## Error handling

- Unparseable `--after`/`--before` → clap-level error, non-zero exit, message
  lists accepted forms (`YYYY-MM-DD`, `Nd`, `Nw`, `Nm`).
- `--path` with embedded single quotes works (escaped); empty string → treated
  as absent (no predicate).
- Date filter + missing/corrupt manifest → hard error with `.context`
  ("date filters need the index manifest").
- Over-fetch starving: if fewer than `limit` results survive the date filter,
  return what survived (no second fetch round in v1; the 10× over-fetch makes
  this rare — noted as a known limitation).

## Agent-facing snippet

README gains a short "Using msrch from coding agents" section: when to use
msrch vs grep (semantic hop vs identifier hop), the `-f filename` pipe
pattern, filter examples, JSON mode, and a note that `query.auto_index = true`
keeps report repos fresh. Same block saved as `docs/AGENTS-SNIPPET.md` for
copy-pasting into consuming repos' AGENTS.md files.

## Versioning

0.3.0 → 0.4.0 (feature, no index-compat change — pre-0.4 indexes work as-is).
CHANGELOG entry + `git tag v0.4.0` on main after merge, per policy.

## Testing

- **Date parser unit tests** (CLI): ISO happy path, each relative unit,
  rejection of garbage (`"tomorrow"`, `"2026-13-40"`, `""`), inclusive/
  exclusive boundary math against a fixed `now`.
- **Filter unit tests** (core): a pure `passes_date_filter(mtime, after,
  before)` helper tested at the boundaries (exactly-at-after → kept;
  exactly-at-before → dropped); path-escape helper (`'` → `''`, `%` passes).
- **Manifest-join test** (core): tempdir manifest with three files at known
  mtimes → filter keeps/drops correctly; missing-entry exclusion.
- **Auto-index tests** (core/CLI): quiet flag suppresses output when fresh;
  config key default false; `--no-auto-index` wins over config. (Endpoint-
  failure fallback is verified manually — needs a live-but-wrong endpoint.)
- **CLI parse tests**: global-flag placement after implicit query text (same
  pattern as the existing `--format` tests).
- Manual end-to-end on the real repo: `--path fixtures --after 1d` etc.

## YAGNI ledger (explicitly not in v1)

- mtime column in the vector schema (parked for next schema bump)
- Filters on `similar`/`stats`
- Natural-language dates; timezone flags (local time only)
- Multiple `--path` values or negation (`--not-path`)
- Second-round fetch when the date filter starves results
- Auto-index for `similar` (query only)

## Success criteria

- `msrch query "x" --path 2026/07 --after 2026-07-01` returns only hits from
  matching, recent-enough files; boundary semantics as specified.
- In a repo with `query.auto_index = true`, editing a file and immediately
  querying reflects the edit — with one status line, no progress bars; killing
  the embedding endpoint degrades to a warning + stale results, never a
  failed query.
- Existing behavior unchanged when no filters/config are used; suite grows;
  `msrch --version` reports 0.4.0 / schema v5.
