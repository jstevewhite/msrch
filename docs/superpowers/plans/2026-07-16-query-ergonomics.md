# Query Ergonomics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `--path`/`--after`/`--before` filters on query, quiet non-fatal auto-reindex behind `query.auto_index`, and agent-facing docs (spec: `docs/superpowers/specs/2026-07-16-query-ergonomics-design.md`).

**Architecture:** A `SearchOptions` struct consolidates core's search request (built for the future MCP front-end). Path filtering compiles to a LanceDB SQL `LIKE` predicate pushed into the vector scan; date filtering joins search hits against the manifest's per-file mtimes with 10× over-fetch, and reranking moves after filtering. Auto-index reuses the existing incremental `Indexer` in a new quiet mode, invoked from the CLI Query arm, non-fatally.

**Tech Stack:** Rust 2024 workspace. No new dependencies (chrono is already a cli dep). No index schema change — SCHEMA_VERSION stays 5.

## Global Constraints

- `cargo test --workspace` green at every commit (baseline 70 tests); `cargo clippy` no new warnings (baseline ~24-26 pre-existing).
- Spec semantics, verbatim: `--path` = substring on stored absolute path (`file_path LIKE '%…%'`, single quotes doubled, `%`/`_` pass through); `--after D` inclusive (mtime ≥ D at 00:00 local for ISO; ≥ now−N for relative); `--before D` exclusive (mtime < bound); relative forms `Nd`/`Nw`/`Nm` with month = 30 days; date over-fetch `max(limit × 10, 100)` merged with reranker `top_n`; rerank runs on filter survivors.
- Date-string parsing lives in the CLI; core takes resolved `SystemTime`s.
- Date filter with missing/corrupt manifest → hard error with context "date filters need the index manifest". A hit missing from the manifest is excluded when a date filter is active (debug-logged).
- Auto-index: config `query.auto_index` default `false`; `--no-auto-index` wins; quiet mode prints exactly one line only when work happened (`auto-index: refreshed N file(s)`); ANY auto-index failure → `warning: auto-index failed (<err>); searching the existing index` to stderr, query proceeds.
- Filters and auto-index apply to `query` only (not `similar`/`stats`).
- No production `unwrap()`; `anyhow` + `.context()`; existing behavior byte-identical when no new flags/config are used.
- Version 0.3.0 → 0.4.0 in Task 6; `git tag v0.4.0` happens on main after merge (controller/human, not a task step).
- Adaptation clause: lancedb 0.31 query-builder filter method is expected to be `only_if(...)` (from the `QueryBase` trait). If the name differs, adapt mechanically and note it; if no SQL-predicate filter exists on vector queries at all, STOP and report BLOCKED.

## File Structure (end state)

```
crates/cli/src/
├── main.rs        # + --path/--after/--before/--no-auto-index args; Query arm builds SearchOptions; auto-index call
└── dates.rs       # NEW — parse_date_arg (ISO + Nd/Nw/Nm), injectable-now core, unit tests
crates/core/src/
├── search.rs      # SearchOptions struct; search(query, &SearchOptions); path predicate; date post-filter; rerank-after-filter
├── db.rs          # search() gains filter: Option<&str> → only_if
├── index.rs       # + load_file_mtimes(); Indexer::index_quiet() -> Result<usize> via run_index(quiet)
└── config.rs      # QueryConfig.auto_index: bool (default false)
docs/AGENTS-SNIPPET.md   # NEW — copy-paste block for consuming repos
README.md          # + filter docs + "Using msrch from coding agents" section
CHANGELOG.md       # 0.4.0 entry
```

---

### Task 1: CLI date parsing module

**Files:**
- Create: `crates/cli/src/dates.rs`
- Modify: `crates/cli/src/main.rs` (add `mod dates;` next to `mod output;`)

**Interfaces:**
- Produces: `dates::parse_date_arg(s: &str) -> Result<std::time::SystemTime, String>` — clap-compatible value parser. ISO `YYYY-MM-DD` → that day's 00:00 local time; `Nd`/`Nw`/`Nm` → `now − N×unit` (m = 30 days). Error string lists accepted forms.

- [ ] **Step 1: Write the failing tests**

Create `crates/cli/src/dates.rs` with only the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn relative_forms_subtract_from_now() {
        let now = t(100 * 86_400); // fixed "now": 100 days after epoch
        assert_eq!(resolve_with_now("7d", now).unwrap(), t(93 * 86_400));
        assert_eq!(resolve_with_now("2w", now).unwrap(), t(86 * 86_400));
        assert_eq!(resolve_with_now("3m", now).unwrap(), t(10 * 86_400)); // 3 × 30d
        assert_eq!(resolve_with_now("1d", now).unwrap(), t(99 * 86_400));
    }

    #[test]
    fn iso_dates_resolve_to_local_midnight() {
        use chrono::{Local, TimeZone};
        let got = resolve_with_now("2026-07-01", t(0)).unwrap();
        let expected: SystemTime = Local
            .with_ymd_and_hms(2026, 7, 1, 0, 0, 0)
            .single()
            .expect("unambiguous local midnight")
            .into();
        assert_eq!(got, expected);
    }

    #[test]
    fn garbage_is_rejected_with_helpful_message() {
        for bad in ["tomorrow", "2026-13-40", "", "7", "d7", "7y", "07/01/2026"] {
            let err = parse_date_arg(bad).unwrap_err();
            assert!(
                err.contains("YYYY-MM-DD") && err.contains("7d"),
                "error must list accepted forms, got: {err}"
            );
        }
    }

    #[test]
    fn relative_larger_than_now_is_rejected_not_panicking() {
        // now − N would underflow SystemTime; must error, not panic.
        let err = resolve_with_now("999999d", t(86_400)).unwrap_err();
        assert!(err.contains("too far in the past"), "{err}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch dates:: 2>&1 | tail -5`
Expected: compile error — `resolve_with_now`, `parse_date_arg` not defined. (Add `mod dates;` to main.rs first or the module won't compile at all.)

- [ ] **Step 3: Implement**

Above the tests module in `crates/cli/src/dates.rs`:

```rust
//! Date-argument parsing for `--after`/`--before`.
//!
//! Accepted forms: ISO `YYYY-MM-DD` (resolves to that day's 00:00 local time)
//! and relative `Nd`/`Nw`/`Nm` (days/weeks/months ago; month ≈ 30 days).
//! Inclusive/exclusive semantics are applied by core at comparison time —
//! both flags parse identically here.

use std::time::{Duration, SystemTime};

const FORMS: &str = "accepted forms: YYYY-MM-DD, or relative 7d / 2w / 3m (days/weeks/months ago)";

/// clap value parser for `--after` / `--before`.
pub fn parse_date_arg(s: &str) -> Result<SystemTime, String> {
    resolve_with_now(s, SystemTime::now())
}

fn resolve_with_now(s: &str, now: SystemTime) -> Result<SystemTime, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(format!("empty date; {FORMS}"));
    }

    // Relative: digits followed by a single unit char.
    if let Some(unit) = s.chars().last().filter(|c| matches!(c, 'd' | 'w' | 'm')) {
        let digits = &s[..s.len() - 1];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let n: u64 = digits
                .parse()
                .map_err(|_| format!("'{s}' is out of range; {FORMS}"))?;
            let days = match unit {
                'd' => n,
                'w' => n * 7,
                'm' => n * 30, // documented approximation
                _ => unreachable!(),
            };
            let delta = Duration::from_secs(days.saturating_mul(86_400));
            return now
                .checked_sub(delta)
                .ok_or_else(|| format!("'{s}' is too far in the past"));
        }
    }

    // ISO date → local midnight.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        use chrono::TimeZone;
        let midnight = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| format!("'{s}' has no midnight (?); {FORMS}"))?;
        return chrono::Local
            .from_local_datetime(&midnight)
            .single()
            .map(SystemTime::from)
            .ok_or_else(|| format!("'{s}' is ambiguous in local time (DST edge); {FORMS}"));
    }

    Err(format!("could not parse date '{s}'; {FORMS}"))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p msrch dates::`
Expected: 4 passed.

- [ ] **Step 5: Full suite + clippy + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` (74 total) and `cargo clippy 2>&1 | tail -3`.

```bash
git add -A
git commit -m "feat: date-argument parser for query filters (ISO + relative forms)"
```

---

### Task 2: `SearchOptions` + DB-side path predicate

**Files:**
- Modify: `crates/core/src/db.rs` (`search` gains `filter: Option<&str>`)
- Modify: `crates/core/src/search.rs` (`SearchOptions`, signature swap, predicate build, escape helper)
- Modify: `crates/cli/src/main.rs` (Query arm builds a `SearchOptions`)

**Interfaces:**
- Produces: `search::SearchOptions { limit: Option<usize>, use_rerank: bool, path_contains: Option<String>, after: Option<SystemTime>, before: Option<SystemTime> }` (all `pub`, derives `Debug, Clone, Default`)
- Produces: `Searcher::search(&self, query_text: &str, opts: &SearchOptions) -> Result<Vec<SearchResult>>`
- Produces: `VectorDB::search(&self, vector: Vec<f32>, limit: u64, min_score: f32, filter: Option<&str>) -> Result<Vec<ScoredPoint>>`
- Produces (crate-internal): `search::sql_like_escape(s: &str) -> String`
- Note: `after`/`before` fields exist but are wired to filtering logic in Task 3 (dead-but-declared here is fine; add `// filtering wired in the date-filter change` comment).

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/search.rs` tests module:

```rust
#[test]
fn sql_like_escape_doubles_single_quotes_only() {
    assert_eq!(sql_like_escape("it's a 'test'"), "it''s a ''test''");
    assert_eq!(sql_like_escape("plain/path"), "plain/path");
    // % and _ pass through — documented LIKE-wildcard bonus.
    assert_eq!(sql_like_escape("week-%_x"), "week-%_x");
}

#[test]
fn search_options_default_is_all_off() {
    let opts = SearchOptions::default();
    assert!(opts.limit.is_none());
    assert!(!opts.use_rerank);
    assert!(opts.path_contains.is_none());
    assert!(opts.after.is_none() && opts.before.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core sql_like 2>&1 | tail -3`
Expected: compile error — `sql_like_escape`, `SearchOptions` not defined.

- [ ] **Step 3: Implement**

`crates/core/src/db.rs` — change the `search` signature and add the predicate. Current code:

```rust
    pub async fn search(
        &self,
        vector: Vec<f32>,
        limit: u64,
        min_score: f32,
    ) -> Result<Vec<ScoredPoint>> {
        ...
        let results = table
            .vector_search(vector)?
            .distance_type(DistanceType::Cosine)
            .limit(limit as usize)
            .execute()
            .await?;
```

becomes:

```rust
    pub async fn search(
        &self,
        vector: Vec<f32>,
        limit: u64,
        min_score: f32,
        filter: Option<&str>,
    ) -> Result<Vec<ScoredPoint>> {
        ...
        let mut query = table
            .vector_search(vector)?
            .distance_type(DistanceType::Cosine)
            .limit(limit as usize);
        if let Some(predicate) = filter {
            query = query.only_if(predicate);
        }
        let results = query.execute().await?;
```

(`only_if` comes from lancedb's `QueryBase` trait — add the `use` if the compiler asks; adaptation clause applies to the method name.)

`crates/core/src/search.rs` — add above `pub struct Searcher`:

```rust
use std::time::SystemTime;

/// Options for a search request. Front-ends build this; core executes it.
/// Designed as a struct so the future MCP front-end can map protocol
/// requests onto it without signature churn.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Max results; `None` uses the config's `default_limit`.
    pub limit: Option<usize>,
    /// Force reranking on (OR'd with config `reranker.enabled`).
    pub use_rerank: bool,
    /// Substring match against the stored absolute file path.
    pub path_contains: Option<String>,
    /// Inclusive lower bound on file modification time.
    pub after: Option<SystemTime>,
    /// Exclusive upper bound on file modification time.
    pub before: Option<SystemTime>,
}

/// Escape a string for embedding in a SQL LIKE '...' literal: single quotes
/// double; `%`/`_` deliberately pass through (documented wildcard bonus).
fn sql_like_escape(s: &str) -> String {
    s.replace('\'', "''")
}
```

Rework `Searcher::search` — signature and the two changed regions (rest of the body unchanged in this task):

```rust
    pub async fn search(
        &self,
        query_text: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;

        // Create reranker config, overriding enabled flag if requested
        let mut reranker_config = self.config.reranker.clone();
        if opts.use_rerank {
            reranker_config.enabled = true;
        }
        let reranker = RerankerClient::new(reranker_config)?;

        let db = VectorDB::new(self.msrch_dir().join("index.db")).await?;

        let embedding = embedder.embed(vec![query_text.to_string()]).await?;
        let query_vector = embedding.first().context("No embedding generated")?.clone();

        let limit = opts.limit.unwrap_or(self.config.query.default_limit);
        let min_score = self.config.query.min_similarity;

        // Filtering wired in the date-filter change; path predicate is live now.
        let predicate = opts
            .path_contains
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| format!("file_path LIKE '%{}%'", sql_like_escape(p)));

        // If reranker enabled, fetch more candidates for reranking
        let fetch_limit = if reranker.is_enabled() {
            reranker.top_n().max(limit)
        } else {
            limit
        };

        let mut results = db
            .search(query_vector, fetch_limit as u64, min_score, predicate.as_deref())
            .await?;
```

(the reranking block below this is untouched in this task).

Update the OTHER `db.search` call site — `find_similar` passes no filter:

```rust
        let results = db.search(query_vector, 20, 0.0, None).await?;
```

`crates/cli/src/main.rs` — Query arm builds the struct (no new flags yet; that's Task 4):

```rust
        Commands::Query {
            text,
            limit,
            format,
            rerank,
        } => {
            let searcher = search::Searcher::new(None)
                .await
                .context("Initialization failed")?;
            let opts = search::SearchOptions {
                limit: *limit,
                use_rerank: *rerank,
                ..Default::default()
            };
            let results = searcher
                .search(text, &opts)
                .await
                .context("Search failed")?;
            output::render(*format, text, &searcher.msrch_dir(), &results);
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 76 total, all green (74 + 2 new).

- [ ] **Step 5: Clippy + commit**

```bash
git add -A
git commit -m "feat: SearchOptions request struct; --path predicate pushed into LanceDB"
```

---

### Task 3: Date filtering — manifest join, over-fetch, rerank-after-filter

**Files:**
- Modify: `crates/core/src/index.rs` (add `load_file_mtimes`)
- Modify: `crates/core/src/search.rs` (date post-filter, over-fetch, restructured rerank/truncate)

**Interfaces:**
- Consumes: `SearchOptions.after/.before` (Task 2).
- Produces: `index::load_file_mtimes(index_root: &Path) -> Result<HashMap<PathBuf, SystemTime>>`
- Produces (crate-internal): `search::passes_date_filter(mtime: SystemTime, after: Option<SystemTime>, before: Option<SystemTime>) -> bool`

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/search.rs` tests:

```rust
#[test]
fn passes_date_filter_boundaries() {
    use std::time::{Duration, UNIX_EPOCH};
    let t = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
    // after is inclusive:
    assert!(passes_date_filter(t(100), Some(t(100)), None));
    assert!(!passes_date_filter(t(99), Some(t(100)), None));
    // before is exclusive:
    assert!(!passes_date_filter(t(100), None, Some(t(100))));
    assert!(passes_date_filter(t(99), None, Some(t(100))));
    // both bounds; and unbounded passes everything:
    assert!(passes_date_filter(t(150), Some(t(100)), Some(t(200))));
    assert!(passes_date_filter(t(0), None, None));
}
```

Add to `crates/core/src/index.rs` tests:

```rust
#[test]
fn load_file_mtimes_reads_manifest_and_errors_when_missing() {
    use std::time::{Duration, UNIX_EPOCH};
    let dir = tempfile::tempdir().unwrap();
    let msrch_dir = dir.path().join(".msrch");
    std::fs::create_dir_all(&msrch_dir).unwrap();
    std::fs::write(
        msrch_dir.join("manifest.json"),
        r#"{"version":5,"files":{
            "/repo/a.md":{"modified_at":{"secs_since_epoch":100,"nanos_since_epoch":0},"chunk_ids":[]},
            "/repo/b.md":{"modified_at":{"secs_since_epoch":200,"nanos_since_epoch":0},"chunk_ids":[]}
        }}"#,
    )
    .unwrap();

    let mtimes = load_file_mtimes(dir.path()).unwrap();
    assert_eq!(mtimes.len(), 2);
    assert_eq!(
        mtimes[&PathBuf::from("/repo/a.md")],
        UNIX_EPOCH + Duration::from_secs(100)
    );

    // Missing manifest → hard error mentioning the manifest.
    let empty = tempfile::tempdir().unwrap();
    let err = load_file_mtimes(empty.path()).unwrap_err();
    assert!(format!("{err:#}").contains("manifest"), "{err:#}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core 'passes_date|load_file_mtimes' 2>&1 | tail -3`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement**

`crates/core/src/index.rs` — add near `get_stats` (public API region):

```rust
/// Per-file modification times from the index manifest, for date-filtered
/// queries. Errors when the manifest is missing or unreadable — a date filter
/// without a manifest is unanswerable, so callers must not silently degrade.
pub fn load_file_mtimes(index_root: &Path) -> Result<HashMap<PathBuf, SystemTime>> {
    let manifest_path = index_root.join(".msrch").join("manifest.json");
    let file = std::fs::File::open(&manifest_path)
        .with_context(|| format!("date filters need the index manifest at {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_reader(file)
        .with_context(|| format!("date filters need a readable index manifest at {}", manifest_path.display()))?;
    Ok(manifest
        .files
        .into_iter()
        .map(|(path, meta)| (path, meta.modified_at))
        .collect())
}
```

(`HashMap` is already imported in index.rs; add if not.)

`crates/core/src/search.rs` — add the pure helper near `sql_like_escape`:

```rust
/// True when `mtime` satisfies the (optional) bounds: `after` is inclusive,
/// `before` is exclusive — matching `--after`/`--before` CLI semantics.
fn passes_date_filter(
    mtime: SystemTime,
    after: Option<SystemTime>,
    before: Option<SystemTime>,
) -> bool {
    if let Some(a) = after {
        if mtime < a {
            return false;
        }
    }
    if let Some(b) = before {
        if mtime >= b {
            return false;
        }
    }
    true
}
```

Rework the middle of `Searcher::search` (between the predicate build and the final `Ok(...)`) to this exact structure:

```rust
        let date_filtering = opts.after.is_some() || opts.before.is_some();

        // Date filters post-filter the hit list, so over-fetch to avoid
        // starving `limit`. Path-only filtering is exact (DB-side predicate).
        let base_fetch = if date_filtering {
            (limit * 10).max(100)
        } else {
            limit
        };
        let fetch_limit = if reranker.is_enabled() {
            base_fetch.max(reranker.top_n())
        } else {
            base_fetch
        };

        let mut results = db
            .search(query_vector, fetch_limit as u64, min_score, predicate.as_deref())
            .await?;

        // Manifest join: drop hits whose file mtime misses the date bounds.
        if date_filtering {
            let mtimes = crate::index::load_file_mtimes(&self.index_root)?;
            results.retain(|r| {
                let path = r
                    .payload
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
                match path.and_then(|p| mtimes.get(&p).copied()) {
                    Some(mtime) => passes_date_filter(mtime, opts.after, opts.before),
                    None => {
                        debug!("date filter: no manifest mtime for a hit; excluding it");
                        false
                    }
                }
            });
        }

        // Rerank the filter survivors (never wastes cross-encoder budget on
        // hits the date filter would discard), then truncate to limit.
        if reranker.is_enabled() && !results.is_empty() {
            debug!("Reranking {} candidates", results.len());

            let documents: Vec<String> = results
                .iter()
                .map(|r| {
                    r.payload
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect();

            match reranker.rerank(query_text, documents).await {
                Ok(reranked) => {
                    debug!("Reranking complete, got {} results", reranked.len());
                    let mut reranked_results: Vec<ScoredPoint> = reranked
                        .into_iter()
                        .filter_map(|(idx, score)| {
                            results.get(idx).map(|r| ScoredPoint {
                                id: r.id.clone(),
                                score, // Use reranker score
                                payload: r.payload.clone(),
                            })
                        })
                        .collect();
                    reranked_results.truncate(limit);
                    results = reranked_results;
                }
                Err(e) => {
                    // Stderr on purpose: user-visible degradation notice even
                    // without a logger initialized (query never inits env_logger).
                    eprintln!("Reranking failed, using vector scores: {}", e);
                    results.truncate(limit);
                }
            }
        } else {
            results.truncate(limit);
        }

        Ok(results.iter().map(SearchResult::from_point).collect())
```

Behavior notes baked into this structure (verify while implementing):
- No filters + no rerank: `base_fetch = limit`, `retain` skipped, final `truncate(limit)` is a no-op → byte-identical to today.
- Rerank without date filter: `fetch = top_n.max(limit)` → identical to today.
- The new `else { results.truncate(limit); }` matters only when over-fetching without rerank.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 78 total green.

- [ ] **Step 5: Clippy + commit**

```bash
git add -A
git commit -m "feat: date filters — manifest mtime join with over-fetch, rerank after filtering"
```

---

### Task 4: CLI flags — --path / --after / --before

**Files:**
- Modify: `crates/cli/src/main.rs` (Cli globals, Query variant, implicit-query copy-over, Query arm, parse tests)

**Interfaces:**
- Consumes: `dates::parse_date_arg` (Task 1), `SearchOptions` (Task 2).

- [ ] **Step 1: Write the failing tests**

Add to main.rs tests (same style as the existing implicit-query tests):

```rust
#[test]
fn implicit_query_honors_filter_flags() {
    let cli = Cli::try_parse_from([
        "msrch", "budget concerns", "--path", "2026/07", "--after", "2026-07-01",
    ])
    .expect("should parse");
    assert_eq!(cli.path, Some("2026/07".to_string()));
    assert!(cli.after.is_some());
    assert!(cli.before.is_none());
    assert_eq!(cli.query_args, vec!["budget concerns".to_string()]);
}

#[test]
fn bad_date_is_a_parse_error_listing_forms() {
    let err = Cli::try_parse_from(["msrch", "q", "--after", "tomorrow"]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("YYYY-MM-DD"), "clap error lists accepted forms: {msg}");
}

#[test]
fn no_auto_index_flag_parses() {
    let cli = Cli::try_parse_from(["msrch", "q", "--no-auto-index"]).expect("should parse");
    assert!(cli.no_auto_index);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch implicit_query_honors_filter 2>&1 | tail -3`
Expected: compile error — fields don't exist.

- [ ] **Step 3: Implement**

Add to the `Cli` struct (after the `rerank` field, same global pattern):

```rust
    /// Only match files whose path contains this substring
    #[arg(long, global = true)]
    path: Option<String>,

    /// Only match files modified on/after this date (YYYY-MM-DD, or 7d/2w/3m ago)
    #[arg(long, global = true, value_parser = dates::parse_date_arg)]
    after: Option<std::time::SystemTime>,

    /// Only match files modified before this date (YYYY-MM-DD, or 7d/2w/3m ago)
    #[arg(long, global = true, value_parser = dates::parse_date_arg)]
    before: Option<std::time::SystemTime>,

    /// Skip the automatic index refresh even if query.auto_index is enabled
    #[arg(long, global = true)]
    no_auto_index: bool,
```

Add the same four fields to the `Commands::Query` variant (mirroring how `limit`/`rerank` are duplicated for the subcommand form):

```rust
    /// Search (implicit query if not a subcommand)
    Query {
        text: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, short, value_enum, default_value_t = OutputFormat::Context)]
        format: OutputFormat,
        /// Use reranker for more precise results (slower)
        #[arg(long)]
        rerank: bool,
        /// Only match files whose path contains this substring
        #[arg(long)]
        path: Option<String>,
        /// Only match files modified on/after this date (YYYY-MM-DD, or 7d/2w/3m ago)
        #[arg(long, value_parser = dates::parse_date_arg)]
        after: Option<std::time::SystemTime>,
        /// Only match files modified before this date (YYYY-MM-DD, or 7d/2w/3m ago)
        #[arg(long, value_parser = dates::parse_date_arg)]
        before: Option<std::time::SystemTime>,
    },
```

Implicit-query construction copies them over:

```rust
            Commands::Query {
                text: cli.query_args.join(" "),
                limit: cli.limit,
                format: cli.format.unwrap_or_default(),
                rerank: cli.rerank,
                path: cli.path.clone(),
                after: cli.after,
                before: cli.before,
            }
```

(`no_auto_index` stays a Cli-level global only — read it via a `let no_auto_index = cli.no_auto_index;` binding taken BEFORE `cli.command` is moved; it's consumed by Task 5's wiring but bind it now so this task compiles the flag end to end.)

Query arm:

```rust
        Commands::Query {
            text,
            limit,
            format,
            rerank,
            path,
            after,
            before,
        } => {
            let searcher = search::Searcher::new(None)
                .await
                .context("Initialization failed")?;
            let opts = search::SearchOptions {
                limit: *limit,
                use_rerank: *rerank,
                path_contains: path.clone(),
                after: *after,
                before: *before,
            };
            let results = searcher
                .search(text, &opts)
                .await
                .context("Search failed")?;
            output::render(*format, text, &searcher.msrch_dir(), &results);
        }
```

If clippy flags the unused `no_auto_index` binding, mark it `let _no_auto_index = ...;` with a `// consumed by auto-index wiring` comment — Task 5 renames it back.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 81 total green.

- [ ] **Step 5: Manual smoke + clippy + commit**

Run: `cargo run -q -- "extraction" --path fixtures --limit 3 -f filename` (in the repo root — should list only fixture paths)
Run: `cargo run -q -- "extraction" --after 1d --limit 3 -f filename 2>&1 | head -3` (recently-modified files only)
Include actual outputs in the commit-message body? No — in your report.

```bash
git add -A
git commit -m "feat: --path/--after/--before query filters on the CLI"
```

---

### Task 5: Auto-index — config key, quiet indexer, non-fatal wiring

**Files:**
- Modify: `crates/core/src/config.rs` (`QueryConfig.auto_index`)
- Modify: `crates/core/src/index.rs` (`index_quiet`, `run_index(quiet)` refactor)
- Modify: `crates/cli/src/main.rs` (Query arm wiring)

**Interfaces:**
- Consumes: `no_auto_index` binding (Task 4).
- Produces: `QueryConfig.auto_index: bool` (serde default false)
- Produces: `Indexer::index_quiet(&self) -> Result<usize>` (count of files whose chunks were re-embedded; no progress bars; no stdout)
- `Indexer::index(&self) -> Result<()>` keeps its exact current output (delegates to the shared internal).

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/config.rs` tests:

```rust
#[test]
fn auto_index_defaults_false_and_loads_from_toml() {
    assert!(!QueryConfig::default().auto_index);
    let config: Config = toml::from_str("[query]\nauto_index = true\n").unwrap();
    assert!(config.query.auto_index);
}
```

Add to `crates/core/src/index.rs` tests:

```rust
#[tokio::test]
async fn index_quiet_on_empty_dir_returns_zero() {
    let dir = tempfile::tempdir().unwrap();
    let indexer = Indexer::new(dir.path().to_path_buf(), crate::config::Config::default());
    // No files → no embedding needed → succeeds without network, returns 0.
    let refreshed = indexer.index_quiet().await.unwrap();
    assert_eq!(refreshed, 0);
}
```

(If index.rs has no `#[tokio::test]` yet, add `tokio.workspace = true` under core's `[dev-dependencies]` — dev-only, allowed; note it in your report. Production code still must not depend on tokio.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core 'auto_index|index_quiet' 2>&1 | tail -3`
Expected: compile errors — field and method don't exist.

- [ ] **Step 3: Implement**

`crates/core/src/config.rs` — add to `QueryConfig`:

```rust
pub struct QueryConfig {
    pub default_limit: usize,
    pub min_similarity: f32,
    pub output_format: String,
    /// Run the incremental index pass before every query in this index
    /// (quiet; non-fatal). Set per-project for fast-changing document repos.
    pub auto_index: bool,
}
```

and `auto_index: false,` in its `Default`.

`crates/core/src/index.rs` — refactor `Indexer::index` into a shared internal with a quiet flag:

```rust
    /// Index (or incrementally reindex) with normal progress output.
    pub async fn index(&self) -> Result<()> {
        self.run_index(false).await.map(|_| ())
    }

    /// Incremental index pass with no progress bars and no stdout — for
    /// auto-index-before-query. Returns the number of files whose chunks
    /// were (re)embedded, so the caller can print one status line iff > 0.
    pub async fn index_quiet(&self) -> Result<usize> {
        self.run_index(true).await
    }

    async fn run_index(&self, quiet: bool) -> Result<usize> {
        // ... existing body of index(), transformed as below ...
    }
```

Transformation rules for the existing body (apply to each listed site):
1. Every bare `println!(...)` in the function (currently at approx. lines 194 [header], 210 [migration notice], 220 "Scanning files...", 222 "Found N files.", 312 "Cleaning up N deleted files...", 337 "No new files to index.", 341 "Embedding N chunks...") → wrap as `if !quiet { println!(...); }`.
2. Both `ProgressBar::new(...)` constructions (file-processing bar at ~:224 and the embedding bar later) → `let pb = if quiet { ProgressBar::hidden() } else { ProgressBar::new(...) };` — `ProgressBar::hidden()` accepts all the same calls (`inc`, `finish_with_message`) as no-ops, so no other lines change.
3. `eprintln!` calls (embed-batch failure at ~:431) stay — errors remain visible in quiet mode.
4. Track the refreshed-file count: declare `let mut refreshed_files: usize = 0;` before the file loop; increment it at the point where a file's chunks are added to `chunks_to_embed` (i.e. once per file that reaches `chunks_to_embed.extend(file_chunks);` with a non-empty `file_chunks`); `run_index` returns `Ok(refreshed_files)` at every exit path that currently returns `Ok(())` (including the early "No new files to index" return, which returns `Ok(0)` — wait: that path can still have deleted-file cleanup; return `Ok(refreshed_files)` uniformly, which is 0 there).

`crates/cli/src/main.rs` — Query arm gains the auto-index preamble (before `Searcher::new`), and passes the discovered root to the Searcher to avoid double discovery:

```rust
        Commands::Query {
            text,
            limit,
            format,
            rerank,
            path,
            after,
            before,
        } => {
            let current_dir = std::env::current_dir()?;
            let index_root = index::find_index_root(&current_dir)
                .context("No .msrch index found in directory tree")?;
            let config = config::Config::load_for_index(&index_root);

            if config.query.auto_index && !no_auto_index {
                let indexer = index::Indexer::new(index_root.clone(), config.clone());
                match indexer.index_quiet().await {
                    Ok(0) => {}
                    Ok(n) => println!("auto-index: refreshed {n} file(s)"),
                    Err(e) => eprintln!(
                        "warning: auto-index failed ({e}); searching the existing index"
                    ),
                }
            }

            let searcher = search::Searcher::new(Some(index_root))
                .await
                .context("Initialization failed")?;
            let opts = search::SearchOptions {
                limit: *limit,
                use_rerank: *rerank,
                path_contains: path.clone(),
                after: *after,
                before: *before,
            };
            let results = searcher
                .search(text, &opts)
                .await
                .context("Search failed")?;
            output::render(*format, text, &searcher.msrch_dir(), &results);
        }
```

(Requires `Config: Clone` — it already derives Clone. Rename Task 4's `_no_auto_index` binding back to `no_auto_index`. Note: the error path when no index exists now originates here rather than inside `Searcher::new` — the context string "No .msrch index found in directory tree" is identical, so user-visible output is unchanged.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 83 total green.

- [ ] **Step 5: Manual smoke + clippy + commit**

In the repo root (which has `.msrch/`): add `auto_index = true` under `[query]` in `.msrch/config.toml` (create the file if absent), `touch README.md`, run `cargo run -q -- "roadmap" --limit 1 -f filename` → expect the `auto-index: refreshed 1 file(s)` line then results; run again → no auto-index line. REMOVE the config change afterward (leave the repo as found) unless `.msrch/config.toml` already existed with other content — report what you did.

```bash
git add -A
git commit -m "feat: quiet non-fatal auto-index before query (query.auto_index)"
```

---

### Task 6: Docs + release 0.4.0

**Files:**
- Create: `docs/AGENTS-SNIPPET.md`
- Modify: `README.md`, `CHANGELOG.md`, `CLAUDE.md`, root `Cargo.toml` (version)

**Interfaces:** none (docs/metadata only).

- [ ] **Step 1: Version bump**

Root `Cargo.toml`: `version = "0.3.0"` → `version = "0.4.0"`. Run `cargo build -q` to refresh Cargo.lock; commit the lock change with the rest.

- [ ] **Step 2: Create `docs/AGENTS-SNIPPET.md`**

```markdown
# msrch — snippet for AGENTS.md / CLAUDE.md of consuming repos

Copy the block below into a repo's agent instructions once the repo is
indexed (`msrch index .`).

---

## Semantic search: msrch

This repo has a semantic index. Use `msrch` for *concept* searches ("where is
retry handling?", "what did the March report say about budget?") and `grep`
for *identifier* searches. Typical flow: msrch finds the right files, grep
pins the exact lines.

    msrch "where do we configure retries?"            # ranked hits with snippets
    msrch "budget concerns" -f filename               # paths only (like grep -l)
    grep -n "max_retries" $(msrch "retry config" -f filename --limit 3)

Filters (query only):

    msrch "quarterly numbers" --path 2026/07          # path substring
    msrch "action items" --after 7d                   # modified in the last 7 days
    msrch "planning" --after 2026-07-01 --before 2026-08-01

Notes:
- `--format json` gives structured output (file_path, chunk_index, similarity,
  context, content).
- Indexed content includes extracted text from HTML, PDF (text layer), and
  .docx files — searchable even though grep can't read them.
- If this repo's `.msrch/config.toml` sets `query.auto_index = true`, results
  are always fresh; otherwise run `msrch index .` after big changes.
- `--rerank` trades speed for precision when the top hits look off.
```

- [ ] **Step 3: README updates**

In README's query/usage documentation, add the three filter flags with one-line descriptions and the boundary semantics (`--after` inclusive, `--before` exclusive, `Nd/Nw/Nm` relative forms, month ≈ 30 days), plus `query.auto_index` under the config documentation (`[query] auto_index = true — refresh the index before every query; quiet; failures fall back to the stale index`). Then add a short section:

```markdown
## Using msrch from coding agents

msrch is designed to be driven by shell-capable agents (no MCP required):
semantic hop with `msrch`, identifier hop with `grep`. See
[docs/AGENTS-SNIPPET.md](docs/AGENTS-SNIPPET.md) for a copy-paste block to add
to a repo's AGENTS.md / CLAUDE.md.
```

Match the README's existing heading style and flag-table format — read the relevant sections first and follow suit.

- [ ] **Step 4: CHANGELOG entry** (top, below intro, above [0.3.0])

```markdown
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
```

- [ ] **Step 5: CLAUDE.md updates**

- "Config Hierarchy" example flags line: `1. CLI flags: \`--limit\`, \`--rerank\`, \`--path\`, \`--after\`/\`--before\`, etc.`
- Key Modules `search.rs` bullet: append "; `SearchOptions` request struct (path/date filters)".
- Development Testing block: add one example line `cargo run -- query "search query" --path docs/ --after 7d`.

- [ ] **Step 6: Full suite + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 83 green.

```bash
git add -A
git commit -m "chore: release 0.4.0 — query filters + auto-index (see CHANGELOG)"
```

**Post-merge (controller/human):** on main, `git tag v0.4.0 && git push --tags`, `make install`, then real-repo smoke: enable `auto_index` in the reports repo, edit a file, query, confirm the one-line refresh + fresh results; try `--after 7d` and `--path` against real reports.

---

## Self-review notes

- Spec coverage: CLI surface incl. boundary semantics + parse errors (Tasks 1, 4), SearchOptions with CLI-side parsing (Tasks 1–2), path predicate DB-side with escaping (Task 2), manifest join + over-fetch + rerank-after-filter + missing-entry exclusion + hard-error-on-missing-manifest (Task 3), auto-index config/quiet/non-fatal/escape-hatch (Task 5), agent snippet + README + versioning 0.4.0 no-schema-change (Task 6). Over-fetch starving accepted as known limitation (spec) — no task, correct. ✓
- Type consistency: `SearchOptions` fields, `parse_date_arg -> Result<SystemTime, String>`, `load_file_mtimes -> Result<HashMap<PathBuf, SystemTime>>`, `index_quiet -> Result<usize>` used consistently across tasks. ✓
- Placeholder scan: all code steps carry code; the run_index transformation is specified as exact per-site rules over an enumerated line inventory rather than reproducing the 250-line function. ✓
- Behavior-preservation guarantees stated where the structure changes (Task 3 notes; Task 5 note on the relocated no-index error context). ✓
