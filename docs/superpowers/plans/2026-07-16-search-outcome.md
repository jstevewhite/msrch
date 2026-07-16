# SearchOutcome Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Search results carry `score_kind` and in-band `warnings`, and rerank becomes a true tri-state override across CLI and MCP (spec: `docs/superpowers/specs/2026-07-16-search-outcome-design.md`). Release 0.7.0.

**Architecture:** Core's `Searcher::search` returns `SearchOutcome { results, score_kind, warnings }`; the rerank tail of `search()` is extracted into a private `apply_rerank` stage so the fallback path is hermetically testable (dead reranker endpoint, no embedding needed). Front-ends adapt: CLI re-prints core warnings to stderr and adds JSON/header fields; MCP passes `rerank` through and assembles a response via a pure, testable builder.

**Tech Stack:** No new dependencies. Rust 2024 workspace as-is.

## Global Constraints

- Spec semantics verbatim: `ScoreKind::Reranker` ONLY when reranking ran and succeeded (fallback = `Vector` + warning); `SearchOptions.rerank: Option<bool>` — `Some(true)` force-on, `Some(false)` force-off overriding config, `None` = config; resolution is `opts.rerank.unwrap_or(config.reranker.enabled)`.
- Warning strings for pre-existing notices stay byte-identical to their current stderr text: core fallback = `Reranking failed, using vector scores: <err>`; MCP auto-index failure keeps its full current stderr line, and its in-band entry is the same text minus the `warning: ` prefix.
- JSON/MCP additions are additive: `similarity` and all existing fields unchanged; `score_kind` always present (`"vector"` / `"reranker"`); `warnings` omitted when empty. Declared CLI display change: context header reads `Found N results (reranked):` for reranked sets; vector sets byte-identical to today.
- No change to how scores are computed, filtered, capped, or truncated; `stats`/`list_indexes`/`similar` untouched.
- The rerank-fallback path MUST have an automated test needing no live endpoints (mechanism: `apply_rerank` extraction + dead endpoint 127.0.0.1:1).
- `cargo test --workspace` green at every commit (baseline 104); clippy no new warnings (~24 baseline); no production `unwrap()`.
- Version 0.6.0 → 0.7.0 in Task 4; `git tag v0.7.0` on main after merge (controller/human). No schema change.

## File Structure (end state)

```
crates/core/src/search.rs   # ScoreKind, SearchOutcome, rerank: Option<bool>, apply_rerank + tests
crates/cli/src/output.rs    # render(&SearchOutcome), JsonOutput score_kind/warnings, context_header + test
crates/cli/src/main.rs      # --no-rerank both forms, rerank_flag() helper, warnings→stderr, tests
crates/mcp/src/server.rs    # rerank pass-through, run_auto_index + build_search_response (pure) + tests
README.md / docs/AGENTS-SNIPPET.md / CLAUDE.md / CHANGELOG.md / Cargo.toml   # Task 4
```

---

### Task 1: Core `SearchOutcome` + `apply_rerank`, front-ends plumbed (flag semantics unchanged)

**Files:**
- Modify: `crates/core/src/search.rs` (types, signature, extraction, tests)
- Modify: `crates/cli/src/output.rs` (render takes `&SearchOutcome`; JSON fields; header helper + test)
- Modify: `crates/cli/src/main.rs` (Query arm: temp rerank mapping, warnings to stderr)
- Modify: `crates/mcp/src/server.rs` (minimal: `rerank: args.rerank` pass-through, consume `outcome.results` — score_kind/warnings response fields land in Task 3)

**Interfaces:**
- Produces: `search::ScoreKind { Vector, Reranker }` with `pub fn as_str(&self) -> &'static str` (`"vector"` / `"reranker"`), derives `Debug, Clone, Copy, PartialEq, Eq`.
- Produces: `search::SearchOutcome { pub results: Vec<SearchResult>, pub score_kind: ScoreKind, pub warnings: Vec<String> }` (derives `Debug`).
- Produces: `Searcher::search(&self, query_text: &str, opts: &SearchOptions) -> Result<SearchOutcome>`.
- Produces: `SearchOptions.rerank: Option<bool>` (REPLACES `use_rerank: bool`).
- Produces (cli): `output::render(format: OutputFormat, query: &str, msrch_dir: &Path, outcome: &SearchOutcome)`; crate-internal `output::context_header(n: usize, kind: ScoreKind) -> String`.

- [ ] **Step 1: Write the failing core tests**

Add to `crates/core/src/search.rs` tests (and UPDATE `search_options_default_is_all_off`: replace `assert!(!opts.use_rerank);` with `assert!(opts.rerank.is_none());`):

```rust
#[test]
fn rerank_resolution_table() {
    // flag × config → resolved enabled
    for (flag, config_enabled, expect) in [
        (None, false, false),
        (None, true, true),
        (Some(true), false, true),
        (Some(true), true, true),
        (Some(false), false, false),
        (Some(false), true, false), // force-off overrides config — the new capability
    ] {
        assert_eq!(
            resolved_rerank_enabled(flag, config_enabled),
            expect,
            "flag={flag:?} config={config_enabled}"
        );
    }
}

#[tokio::test]
async fn apply_rerank_disabled_reports_vector_and_no_warnings() {
    let reranker = RerankerClient::new(crate::config::RerankerConfig {
        enabled: false,
        ..crate::config::RerankerConfig::default()
    })
    .unwrap();
    let points = vec![point("a.rs"), point("b.rs"), point("c.rs")];
    let (results, kind, warnings) = apply_rerank(&reranker, "q", points, 2).await;
    assert_eq!(kind, ScoreKind::Vector);
    assert!(warnings.is_empty());
    assert_eq!(results.len(), 2, "truncated to limit");
}

#[tokio::test]
async fn apply_rerank_fallback_reports_vector_and_warning() {
    // Dead endpoint: enabled reranker fails instantly, hermetically.
    let reranker = RerankerClient::new(crate::config::RerankerConfig {
        enabled: true,
        endpoint: "http://127.0.0.1:1/rerank".to_string(),
        ..crate::config::RerankerConfig::default()
    })
    .unwrap();
    let points = vec![point("a.rs"), point("b.rs"), point("c.rs")];
    let (results, kind, warnings) = apply_rerank(&reranker, "q", points, 2).await;
    assert_eq!(kind, ScoreKind::Vector, "fallback is vector-scored");
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].starts_with("Reranking failed, using vector scores:"),
        "byte-compatible warning text: {}",
        warnings[0]
    );
    assert_eq!(results.len(), 2, "fallback still truncates to limit");
}
```

(The `point(file_path)` helper already exists in this tests module from the earlier `unique_file_paths`-era tests — reuse it; if its shape drifted, it builds a `ScoredPoint { id, score: 1.0, payload: json!({"file_path": ...}) }`.)

- [ ] **Step 2: RED**

Run: `cargo test -p msrch-core 'rerank_resolution|apply_rerank' 2>&1 | tail -3`
Expected: compile errors — `resolved_rerank_enabled`, `apply_rerank`, `ScoreKind` undefined.

- [ ] **Step 3: Implement core**

In `crates/core/src/search.rs`:

Replace `SearchOptions.use_rerank`:

```rust
    /// Rerank override: `Some(true)` forces reranking on, `Some(false)`
    /// forces it off (overriding config), `None` uses `reranker.enabled`.
    pub rerank: Option<bool>,
```

Add near `SearchResult`:

```rust
/// What the `score` field of each result means for this query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKind {
    /// Cosine similarity from the vector search (1.0 − distance).
    Vector,
    /// Cross-encoder relevance from the reranker (its own scale).
    Reranker,
}

impl ScoreKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScoreKind::Vector => "vector",
            ScoreKind::Reranker => "reranker",
        }
    }
}

/// A search's results plus the metadata front-ends need to present them
/// honestly: which score scale applies, and any degradations that occurred.
#[derive(Debug)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    /// Reranker ONLY when reranking ran and succeeded.
    pub score_kind: ScoreKind,
    /// Human-readable degradation notices (front-ends decide the channel).
    pub warnings: Vec<String>,
}

/// `Some(flag)` overrides config in either direction; `None` defers to it.
fn resolved_rerank_enabled(flag: Option<bool>, config_enabled: bool) -> bool {
    flag.unwrap_or(config_enabled)
}
```

In `search()`: signature returns `Result<SearchOutcome>`; the config override becomes

```rust
        let mut reranker_config = self.config.reranker.clone();
        reranker_config.enabled =
            resolved_rerank_enabled(opts.rerank, reranker_config.enabled);
```

and the ENTIRE tail from `// Rerank the filter survivors...` through the final truncates (currently search.rs:194-240) is replaced by:

```rust
        let (results, score_kind, warnings) =
            apply_rerank(&reranker, query_text, results, limit).await;

        Ok(SearchOutcome {
            results: results.iter().map(SearchResult::from_point).collect(),
            score_kind,
            warnings,
        })
```

New private stage function (the moved tail — bodies identical except the `eprintln!` becomes a warning entry; keep the existing comments):

```rust
/// Rerank stage: caps candidates at top_n, reranks the survivors, and
/// truncates to `limit`. Reports which score scale the results carry and any
/// degradation. Extracted so the fallback path tests without live endpoints.
async fn apply_rerank(
    reranker: &RerankerClient,
    query_text: &str,
    mut results: Vec<ScoredPoint>,
    limit: usize,
) -> (Vec<ScoredPoint>, ScoreKind, Vec<String>) {
    if reranker.is_enabled() && !results.is_empty() {
        debug!("Reranking {} candidates", results.len());

        // Survivors are vector-score-ordered; rerank only the top_n best,
        // preserving top_n's contract as the cross-encoder candidate cap
        // even when date filtering over-fetched.
        results.truncate(reranker.top_n().max(limit));

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
                (reranked_results, ScoreKind::Reranker, Vec::new())
            }
            Err(e) => {
                // Front-ends choose the channel (CLI: stderr; MCP: in-band).
                let warning = format!("Reranking failed, using vector scores: {}", e);
                results.truncate(limit);
                (results, ScoreKind::Vector, vec![warning])
            }
        }
    } else {
        results.truncate(limit);
        (results, ScoreKind::Vector, Vec::new())
    }
}
```

- [ ] **Step 4: Write the failing output test, then adapt the CLI**

Add to `crates/cli/src/output.rs` tests:

```rust
#[test]
fn context_header_marks_reranked_sets() {
    use msrch_core::search::ScoreKind;
    assert_eq!(context_header(5, ScoreKind::Vector), "Found 5 results:");
    assert_eq!(context_header(5, ScoreKind::Reranker), "Found 5 results (reranked):");
}
```

Implement in `output.rs`:
- Imports gain `ScoreKind`/`SearchOutcome`: `use msrch_core::search::{ScoreKind, SearchOutcome, SearchResult, SimilarFile};`
- `JsonOutput` gains fields (after `index_path`):

```rust
    score_kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
```

- New helper + `display_context` uses it:

```rust
/// Header line for the context format; reranked sets are labeled so the
/// score scale is self-explanatory.
fn context_header(n: usize, kind: ScoreKind) -> String {
    match kind {
        ScoreKind::Reranker => format!("Found {n} results (reranked):"),
        ScoreKind::Vector => format!("Found {n} results:"),
    }
}
```

- `render` signature and body:

```rust
pub fn render(format: OutputFormat, query: &str, msrch_dir: &Path, outcome: &SearchOutcome) {
    let results = &outcome.results;
    if results.is_empty() {
        match format {
            OutputFormat::Json => {
                let mut empty = serde_json::json!({
                    "query": query,
                    "index_path": msrch_dir.display().to_string(),
                    "score_kind": outcome.score_kind.as_str(),
                    "results": []
                });
                if !outcome.warnings.is_empty() {
                    empty["warnings"] = serde_json::json!(outcome.warnings);
                }
                println!("{}", empty);
            }
            _ => println!("No results found."),
        }
        return;
    }

    match format {
        OutputFormat::Plain => display_plain(results),
        OutputFormat::Context => display_context(results, outcome.score_kind),
        OutputFormat::Json => display_json(query, msrch_dir, outcome),
        OutputFormat::Filename => display_filename(results),
    }
}
```

- `display_context(results: &[SearchResult], kind: ScoreKind)` — its first line becomes `println!("{}", context_header(results.len(), kind).bold());` (everything else unchanged).
- `display_json(query: &str, msrch_dir: &Path, outcome: &SearchOutcome)` — builds `JsonResult`s from `outcome.results` as today and fills the two new `JsonOutput` fields from `outcome.score_kind.as_str()` / `outcome.warnings.clone()`.

`crates/cli/src/main.rs` Query arm — temporary flag mapping preserving today's force-on-only semantics (Task 2 replaces it), plus the stderr re-print:

```rust
            let opts = search::SearchOptions {
                limit: *limit,
                use_rerank: /* DELETED */
                rerank: (*rerank).then_some(true), // Task 2 makes this tri-state
                min_similarity: *min_similarity,
                path_contains: path.clone(),
                after: *after,
                before: *before,
            };
            let outcome = searcher
                .search(text, &opts)
                .await
                .context("Search failed")?;
            for warning in &outcome.warnings {
                eprintln!("{warning}"); // e.g. "Reranking failed, using vector scores: ..."
            }
            output::render(*format, text, &searcher.msrch_dir(), &outcome);
```

`crates/mcp/src/server.rs` minimal compile adaptation (Task 3 finishes the response):
- `use_rerank: args.rerank.unwrap_or(false),` → `rerank: args.rerank,` (this IS the final tri-state pass-through; its response-visible effects are tested in Task 3).
- `let results = searcher.search(...)` → `let outcome = searcher.search(...)`; `json_results` maps over `outcome.results`; nothing else consumed yet.
- Update `SearchArgs.rerank`'s doc comment: `/// Rerank override: true forces on, false forces off, omitted uses the index's config.`

- [ ] **Step 5: GREEN**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 108 total (104 + 3 core + 1 output). Clippy: no new warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: SearchOutcome — score_kind and warnings from core; rerank Option<bool>"
```
(Append the standard Claude Code footer.)

---

### Task 2: CLI tri-state — `--no-rerank`

**Files:**
- Modify: `crates/cli/src/main.rs` (flags both forms, conflict, helper, arm, tests)

**Interfaces:**
- Consumes: `SearchOptions.rerank: Option<bool>` (Task 1).
- Produces (cli-internal): `fn rerank_flag(rerank: bool, no_rerank: bool) -> Option<bool>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn rerank_flags_parse_tri_state_and_conflict() {
    // Neither → None (config decides).
    let cli = Cli::try_parse_from(["msrch", "q"]).unwrap();
    assert!(!cli.rerank && !cli.no_rerank);
    assert_eq!(rerank_flag(cli.rerank, cli.no_rerank), None);
    // --rerank → Some(true).
    let cli = Cli::try_parse_from(["msrch", "q", "--rerank"]).unwrap();
    assert_eq!(rerank_flag(cli.rerank, cli.no_rerank), Some(true));
    // --no-rerank → Some(false).
    let cli = Cli::try_parse_from(["msrch", "q", "--no-rerank"]).unwrap();
    assert_eq!(rerank_flag(cli.rerank, cli.no_rerank), Some(false));
    // Both → clap conflict error.
    let err = Cli::try_parse_from(["msrch", "q", "--rerank", "--no-rerank"]).unwrap_err();
    assert!(err.to_string().contains("cannot be used with"), "{err}");
    // Subcommand form parses too.
    let cli = Cli::try_parse_from(["msrch", "query", "q", "--no-rerank"]).unwrap();
    match cli.command {
        Some(Commands::Query { no_rerank, .. }) => assert!(no_rerank),
        other => panic!("expected Query, got {other:?}"),
    }
}
```

- [ ] **Step 2: RED**, then implement:

Helper near `parse_min_similarity`:

```rust
/// --rerank / --no-rerank → the tri-state core override. clap's
/// conflicts_with guarantees at most one is set.
fn rerank_flag(rerank: bool, no_rerank: bool) -> Option<bool> {
    match (rerank, no_rerank) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}
```

Cli struct, directly after the `rerank` field (and mirror in the Query variant after its `rerank`):

```rust
    /// Skip reranking even if the config enables it
    #[arg(long, global = true, conflicts_with = "rerank")]
    no_rerank: bool,
```

(Query-variant copy uses `#[arg(long, conflicts_with = "rerank")]`.) Implicit-query construction copies `no_rerank: cli.no_rerank,`. The Query arm binding gains `no_rerank`, and the SearchOptions line becomes:

```rust
                rerank: rerank_flag(*rerank, *no_rerank),
```

- [ ] **Step 3: GREEN + manual smoke**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 109 total.
Manual (this repo's config has reranker enabled globally): `cargo run -q -- "chunking" --limit 3 2>/dev/null | head -1` → header should say `(reranked)`; `cargo run -q -- "chunking" --limit 3 --no-rerank 2>/dev/null | head -1` → plain header, visibly cosine-scale scores. Report both actual outputs.
Clippy: no new warnings.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: --no-rerank — tri-state rerank override on the CLI"
```
(Append the standard Claude Code footer.)

---

### Task 3: MCP — score_kind + warnings in the search response

**Files:**
- Modify: `crates/mcp/src/server.rs` (run_auto_index extraction, response builder, search_impl wiring, tests)

**Interfaces:**
- Consumes: `SearchOutcome`/`ScoreKind` (Task 1).
- Produces (crate-internal): `MsrchServer::run_auto_index(&self, entry: &IndexEntry, config: &Config) -> (usize, Vec<String>)`; `fn build_search_response(index: &str, query: &str, outcome: &SearchOutcome, refreshed: usize, extra_warnings: Vec<String>) -> serde_json::Value`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn run_auto_index_reports_skip_and_failure_warnings() {
    // Fixture root with auto_index enabled and a dead embedding endpoint.
    let (dir, server) = fixture_server(&["alpha"]);
    std::fs::write(
        dir.path().join("alpha/.msrch/config.toml"),
        "[embedding]\nendpoint = \"http://127.0.0.1:1/embeddings\"\n[query]\nauto_index = true\n",
    )
    .unwrap();
    let entry = server.state.registry.resolve(Some("alpha")).unwrap().clone();
    let config = msrch_core::config::Config::load_for_index(&entry.root);

    // Lock held → skip warning, no refresh, manifest untouched.
    {
        let _g = server.state.auto_index_locks.get("alpha").unwrap().try_lock().unwrap();
        let (refreshed, warnings) = server.run_auto_index(&entry, &config).await;
        assert_eq!(refreshed, 0);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("already in flight"), "{warnings:?}");
    }

    // Lock free but endpoint dead → failure warning, non-fatal.
    let (refreshed, warnings) = server.run_auto_index(&entry, &config).await;
    assert_eq!(refreshed, 0);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].starts_with("auto-index failed for 'alpha'"), "{warnings:?}");
}

#[test]
fn build_search_response_shapes_fields_correctly() {
    use msrch_core::search::{ScoreKind, SearchOutcome, SearchResult};
    let outcome = SearchOutcome {
        results: vec![SearchResult {
            file_path: "/a.md".into(),
            chunk_index: 0,
            score: 0.5,
            context: String::new(),
            content: "x".into(),
        }],
        score_kind: ScoreKind::Reranker,
        warnings: vec!["core warning".into()],
    };
    let v = build_search_response("alpha", "q", &outcome, 2, vec!["mcp warning".into()]);
    assert_eq!(v["score_kind"], "reranker");
    assert_eq!(v["auto_index_refreshed"], 2);
    assert_eq!(v["warnings"][0], "core warning", "core warnings first");
    assert_eq!(v["warnings"][1], "mcp warning");
    assert_eq!(v["results"][0]["similarity"], 0.5);

    // Empty warnings + zero refreshed → both fields omitted; vector kind maps.
    let outcome = SearchOutcome { results: vec![], score_kind: ScoreKind::Vector, warnings: vec![] };
    let v = build_search_response("alpha", "q", &outcome, 0, vec![]);
    assert_eq!(v["score_kind"], "vector");
    assert!(v.get("warnings").is_none());
    assert!(v.get("auto_index_refreshed").is_none());
}
```

(If `IndexEntry` isn't `Clone` or `state`/`registry` visibility blocks the first test, widen to `pub(crate)` — note it. The dead-endpoint auto-index run makes real `index_quiet` crawl the tiny fixture; it fails at embedding, hermetically.)

- [ ] **Step 2: RED**, then implement:

Extract from `search_impl`'s current auto-index block:

```rust
    /// Config-gated, race-locked, non-fatal freshness pass. Returns the
    /// refreshed-file count and any degradation warnings (also mirrored to
    /// the server's stderr for operator visibility).
    pub(crate) async fn run_auto_index(
        &self,
        entry: &IndexEntry,
        config: &msrch_core::config::Config,
    ) -> (usize, Vec<String>) {
        let mut warnings = Vec::new();
        let mut refreshed = 0usize;
        if config.query.auto_index
            && let Some(lock) = self.state.auto_index_locks.get(&entry.name)
        {
            match lock.try_lock() {
                Ok(_guard) => {
                    let indexer =
                        msrch_core::index::Indexer::new(entry.root.clone(), config.clone());
                    match indexer.index_quiet().await {
                        Ok(n) => refreshed = n,
                        Err(e) => {
                            let msg = format!(
                                "auto-index failed for '{}' ({e:#}); searching the existing index",
                                entry.name
                            );
                            eprintln!("warning: {msg}");
                            warnings.push(msg);
                        }
                    }
                }
                // Another request is already refreshing this root; the
                // quiet/non-fatal contract says search what exists now.
                Err(_) => warnings.push(format!(
                    "auto-index skipped: a refresh is already in flight for '{}'",
                    entry.name
                )),
            }
        }
        (refreshed, warnings)
    }
```

Pure response builder (free function below the impl):

```rust
/// Assemble the search tool's response. Core warnings come first, then
/// MCP-layer ones; empty warnings and zero refreshed are omitted entirely.
fn build_search_response(
    index: &str,
    query: &str,
    outcome: &msrch_core::search::SearchOutcome,
    refreshed: usize,
    extra_warnings: Vec<String>,
) -> serde_json::Value {
    let json_results: Vec<serde_json::Value> = outcome
        .results
        .iter()
        .map(|r| {
            serde_json::json!({
                "file_path": r.file_path,
                "chunk_index": r.chunk_index,
                "similarity": r.score,
                "context": r.context,
                "content": r.content,
            })
        })
        .collect();
    let mut out = serde_json::json!({
        "index": index,
        "query": query,
        "score_kind": outcome.score_kind.as_str(),
        "results": json_results,
    });
    if refreshed > 0 {
        out["auto_index_refreshed"] = serde_json::json!(refreshed);
    }
    let mut warnings = outcome.warnings.clone();
    warnings.extend(extra_warnings);
    if !warnings.is_empty() {
        out["warnings"] = serde_json::json!(warnings);
    }
    out
}
```

`search_impl` rewires: replace its inline auto-index block with `let (refreshed, auto_warnings) = self.run_auto_index(entry, &config).await;` (adjust borrows — `entry` is a `&IndexEntry` from resolve), and replace the response-assembly tail with `Ok(build_search_response(&entry.name, &args.query, &outcome, refreshed, auto_warnings))`.

Update the `search` `#[tool(description = ...)]` to append: `Results carry score_kind (vector|reranker — reranker scores use their own scale) and a warnings array for degradations.`

- [ ] **Step 3: GREEN**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 111 total. Existing mcp tests (stale-schema, validation, lock-skip, backend-failure) must pass unchanged. Clippy: no new warnings.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: MCP search responses carry score_kind and in-band warnings"
```
(Append the standard Claude Code footer.)

---

### Task 4: Docs + release 0.7.0

**Files:**
- Modify: `README.md`, `docs/AGENTS-SNIPPET.md`, `CLAUDE.md`, `CHANGELOG.md`, root `Cargo.toml`, `crates/cli/src/main.rs` (doc-comment rider)

- [ ] **Step 1: Version bump** — 0.6.0 → 0.7.0; `cargo build -q`; commit lock delta with the rest. main.rs version_string doc example → `msrch 0.7.0 (index schema v5, commit a1b2c3d)`.

- [ ] **Step 2: README** — Query Options gains, next to the `--rerank` example:

```bash
# Force reranking OFF even where config enables it
msrch "auth logic" --no-rerank
```

and, near the JSON-output example, one sentence: `JSON output includes score_kind ("vector" cosine similarity, or "reranker" cross-encoder relevance — a different scale) and a warnings array when a degradation occurred (e.g. reranker unreachable).` Add `--no-rerank` to the Configuration section's CLI-flags precedence line.

- [ ] **Step 3: AGENTS-SNIPPET.md** — add to the Notes list:

```markdown
- Results carry `score_kind`: `reranker` scores use the cross-encoder's own
  scale (often ≪1) — don't compare them to `vector` cosine scores. Pass
  `rerank: false` (MCP) / `--no-rerank` (CLI) to force cosine scoring.
```

- [ ] **Step 4: CLAUDE.md** — Key Modules `search.rs` bullet: append `; returns SearchOutcome (results + score_kind + warnings)`. Query Pipeline step 5: `5. **Result Formatting** - Plain/Context/JSON output modes; results carry score_kind + degradation warnings`.

- [ ] **Step 5: CHANGELOG** (top, above [0.6.0]):

```markdown
## [0.7.0] - 2026-07-16

### Added
- **score_kind** on search results (CLI JSON and MCP): `"vector"` (cosine
  similarity) or `"reranker"` (cross-encoder relevance, its own scale) — set
  to reranker only when reranking actually ran and succeeded. The context
  format's header now reads `Found N results (reranked):` for reranked sets.
- **warnings** array (CLI JSON and MCP responses): in-band degradation
  notices — reranker fallback, auto-index failure, and auto-index skipped
  because a refresh was already in flight. Previously these were visible only
  on the server/CLI stderr, invisible to MCP clients.
- `--no-rerank` (CLI) and true tri-state `rerank` (MCP): `false` now forces
  reranking OFF even where config enables it; omitted defers to config.
  Previously the flag could only force it on.

No index schema change — existing indexes work as-is.
```

- [ ] **Step 6: Full suite + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 111 green.

```bash
git add -A
git commit -m "chore: release 0.7.0 — score_kind, warnings, tri-state rerank (see CHANGELOG)"
```
(Append the standard Claude Code footer.)

**Post-merge (controller/human):** `git tag v0.7.0 && git push --tags`; `make install`; live acceptance: `msrch "q" --limit 3` shows `(reranked)` header with reranker up; `--no-rerank` flips to cosine scores and plain header; kill the reranker endpoint → header plain + stderr fallback line + JSON/MCP `warnings` populated; MCP `rerank: false` visibly flips score scale.

---

## Self-review notes

- Spec coverage: SearchOutcome/ScoreKind/tri-state resolution + byte-identical fallback text (Task 1), CLI flags/stderr/JSON/header (Tasks 1–2), MCP pass-through + response fields + skip-warning visibility via `run_auto_index` (Tasks 1, 3), docs/release (Task 4). Spec's deferred test mechanism resolved: `apply_rerank` extraction with dead-endpoint fixture. ✓
- Deviation from spec's letter, noted: (a) the spec's in-band auto-index-failure text said "searched"; the plan uses "searching" so in-band and stderr texts stay identical (spec intent: mirroring) — one word. (b) The spec's testing sketch extended `auto_index_lock_skips_when_held` for the skip warning; that path ends in a search error before response assembly, so the plan tests `run_auto_index` and `build_search_response` directly instead — strictly stronger coverage of the same behaviors. Reviewers: judge these as the intended mechanism, but flag if you disagree.
- Type consistency: `apply_rerank(&RerankerClient, &str, Vec<ScoredPoint>, usize) -> (Vec<ScoredPoint>, ScoreKind, Vec<String>)`, `run_auto_index -> (usize, Vec<String>)`, `build_search_response(..., Vec<String>) -> Value`, `rerank_flag(bool, bool) -> Option<bool>` used identically across tasks. ✓
- Behavior preservation stated: Task 1 CLI mapping `(*rerank).then_some(true)` keeps force-on-only semantics until Task 2; vector-set context header byte-identical; warning texts byte-compatible. ✓
