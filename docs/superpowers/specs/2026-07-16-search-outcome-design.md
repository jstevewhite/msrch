# SearchOutcome: score_kind, tri-state rerank, in-band warnings — Design

*Approved 2026-07-16, from MCP field feedback. Targets release 0.7.0. No index
schema change (stays v5).*

## Purpose

Field testing the MCP server surfaced three observability/control gaps:
results don't say whether their scores are cosine similarities or
cross-encoder relevance (two humans and one agent have now misread this);
`rerank: false` cannot switch OFF a config-enabled reranker (force-on-only
semantics); and degradation notices (rerank fallback, auto-index failures/
skips) go to server stderr, which MCP clients cannot see.

## 1. Core: `SearchOutcome`

`Searcher::search` changes its return type — the truth about what happened
during a search originates in core:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKind {
    /// Cosine similarity from the vector search (1.0 − distance).
    Vector,
    /// Cross-encoder relevance from the reranker (its own scale).
    Reranker,
}

#[derive(Debug)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    /// Reranker ONLY when reranking ran and succeeded; a rerank attempt that
    /// fell back to vector scores reports Vector plus a warning.
    pub score_kind: ScoreKind,
    /// Human-readable degradation notices. Text of the rerank-fallback entry
    /// is byte-identical to the pre-0.7 stderr line
    /// ("Reranking failed, using vector scores: <err>").
    pub warnings: Vec<String>,
}

pub async fn search(&self, query_text: &str, opts: &SearchOptions)
    -> Result<SearchOutcome>;
```

- Core stops printing entirely on this path: the rerank-fallback `eprintln!`
  inside `search()` becomes a `warnings` entry. (Better core/front-end
  separation than today; the CLI re-prints it, below.)
- `SearchOptions.use_rerank: bool` → `pub rerank: Option<bool>`:
  `Some(true)` force-on, `Some(false)` force-off (overrides config),
  `None` → config default. Resolution:
  `let rerank_enabled = opts.rerank.unwrap_or(config.reranker.enabled);`
  (replaces the current force-on OR).

## 2. CLI

- `--rerank` keeps meaning force-on (`Some(true)`); new `--no-rerank` means
  force-off (`Some(false)`); passing both is a clap conflict error; neither →
  `None`. Both flags exist in the global (implicit-query) and `query`
  subcommand forms, mirroring existing dual definitions.
- The Query arm prints each `outcome.warnings` entry to stderr prefixed
  nothing (the text already reads as a warning) — preserving today's visible
  behavior for the fallback notice, relocated from core.
- JSON output (additive; existing fields untouched):
  `"score_kind": "vector" | "reranker"` and `"warnings": [...]` (omitted when
  empty) at the top level next to `query`/`index_path`.
- Context format header (declared behavior change, one line): reranked
  result sets print `Found N results (reranked):`; vector sets keep the
  existing `Found N results:` exactly. No per-result changes. Plain and
  filename formats unchanged.
- `similar` untouched (no rerank path there).

## 3. MCP

- `SearchArgs.rerank: Option<bool>` becomes a true pass-through to
  `SearchOptions.rerank` (drops the `unwrap_or(false)`).
- Search response gains:
  - `"score_kind": "vector" | "reranker"` (always present), and
  - `"warnings": [...]` (omitted when empty), containing in order:
    core's `outcome.warnings`, plus MCP-layer degradations —
    auto-index failure (`"auto-index failed for '<name>' (<err>); searched the existing index"`,
    text mirroring the current stderr line) and, new visibility, the try-lock
    skip (`"auto-index skipped: a refresh is already in flight for '<name>'"`).
  - The existing stderr prints in `search_impl` for auto-index failure remain
    (server operator visibility) — the warnings array is additive.
- `stats` / `list_indexes` untouched.

## Compatibility & scope guards

- No change to how scores are computed, filtered, capped, or truncated.
- Warning strings for pre-existing notices stay byte-identical to their
  current stderr text (only their delivery channel gains a lane).
- JSON/MCP additions are additive; `similarity` field name and all existing
  fields unchanged.
- Release 0.6.0 → 0.7.0 (response-contract addition = minor per policy);
  CHANGELOG + `git tag v0.7.0` on main after merge. Docs: README query
  options (`--no-rerank`, score_kind note in the JSON example area),
  AGENTS-SNIPPET one line ("results carry score_kind — reranker scores use
  their own scale"), CLAUDE.md search.rs bullet.

## Error handling

- Both-flags (`--rerank --no-rerank`) → clap conflict error, non-zero exit.
- A rerank attempt that fails still degrades exactly as today (vector scores,
  truncate to limit) — now with `score_kind: Vector` + the warning in-band.
- Auto-index failures remain non-fatal everywhere.

## Testing

- Core tri-state resolution table: config enabled × flag {None, Some(true),
  Some(false)} → 6 cases asserting the resolved `rerank_enabled`.
- Rerank fallback populates warnings + reports Vector: hermetic fixture with
  `reranker.enabled = true` pointing at `127.0.0.1:1` (dead endpoint), vector
  path served by... (note: full search needs an embedding endpoint — this
  test lives at the MCP/handler level only if an embedding endpoint stub is
  available; otherwise the fallback branch is unit-tested by extracting the
  rerank step into a testable helper. Plan decides the cheaper mechanism —
  requirement: the fallback→warning+Vector path has an automated test that
  does not need live endpoints.)
- CLI: `--no-rerank` parse (both forms), `--rerank --no-rerank` conflict,
  JSON contains score_kind and omits empty warnings.
- MCP: response-shape tests — score_kind present; warnings populated for the
  auto-index-skip case (lock held, hermetic, extending the existing
  `auto_index_lock_skips_when_held` test to assert the in-band warning).
- Full workspace suite green; clippy no new warnings; context-header change
  covered by asserting the render output for both ScoreKind values.

## YAGNI ledger (explicitly not in v1)

- Per-result score_kind (uniform per query by construction)
- Warning severity levels / structured warning objects (strings suffice)
- Rerank score normalization (raw cross-encoder scale stays)
- `similar` rerank support
- MCP notifications/logging channel for warnings (in-band array only)

## Success criteria

- An MCP agent can tell from the response alone whether scores are reranker
  or vector, and see every degradation that affected its query.
- `rerank: false` / `--no-rerank` actually disables a config-enabled
  reranker (verifiable live: scores flip from cross-encoder to cosine scale).
- Existing pipelines (`-f filename`, JSON consumers reading existing fields)
  see zero breakage; suite grows; `msrch --version` reports 0.7.0 / schema v5.
