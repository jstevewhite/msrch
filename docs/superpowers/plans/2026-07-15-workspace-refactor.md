# msrch Workspace Refactor (Roadmap Item 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the single-crate msrch binary into a Cargo workspace (`crates/core` lib + `crates/cli` bin) with zero logic in command handlers, and wire project-level `.msrch/config.toml` into the config hierarchy.

**Architecture:** First refactor the presentation boundary *in place* (search returns data; rendering moves to an `output` module; the `similar` command's 90 lines of logic move into `Searcher`), so the crate split afterward is purely mechanical. Then split into `msrch-core` (all logic) and `msrch` (clap + rendering). Finally add config merging: global config overlaid field-by-field with the project's `.msrch/config.toml`.

**Tech Stack:** Rust 2024, clap 4, LanceDB, toml 0.9, confy 2, tokio. New dev-dependency: `tempfile` (core only).

## Global Constraints

- The installed binary name stays `msrch`; `target/release/msrch` must exist after `make build` (workspace target dir keeps this true — do not change the Makefile `install` recipe's source path).
- JSON output field names are a public contract: results use `similarity` (NOT `score`), plus `file_path`, `chunk_index`, `context`, `content`, wrapped in `{query, index_path, results}`. Do not change them.
- All user-facing text (result formatting, warnings, stats layout) must be byte-identical to current output except where a task explicitly says otherwise.
- `cargo test` (later `cargo test --workspace`) must pass at every commit. `cargo clippy` must introduce no new warnings.
- Config precedence when done: CLI flags > project `.msrch/config.toml` > global confy config > `Default` impls.
- No new runtime dependencies. `tempfile` is dev-only.
- Every fallible path uses `anyhow::Result` + `.context()`; no `unwrap()` in production code paths.
- Commit at the end of every task at minimum; commit message style: `refactor:`/`feat:`/`test:` prefixes as shown.

## File Structure (end state)

```
msrch/
├── Cargo.toml                    # virtual workspace manifest + [workspace.dependencies]
├── Makefile                      # unchanged
├── crates/
│   ├── core/                     # package msrch-core (lib) — ALL logic
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # pub mod declarations only
│   │       ├── chunker.rs        # moved verbatim from src/
│   │       ├── config.rs         # + deep_merge, load_for_index, overlay_project_config
│   │       ├── crawler.rs        # moved verbatim
│   │       ├── db.rs             # moved verbatim
│   │       ├── embedding.rs      # moved verbatim
│   │       ├── index.rs          # moved; IndexStats::print REMOVED (goes to cli)
│   │       ├── reranker.rs       # moved verbatim
│   │       └── search.rs         # returns SearchResult/SimilarFile data; no display
│   └── cli/                      # package msrch (bin msrch)
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs           # clap structs + dispatch; thin arms only
│           └── output.rs         # OutputFormat, render(), print_similar(), print_stats()
└── src/                          # deleted by Task 3
```

---

### Task 1: Search returns data; rendering moves to `src/output.rs`

Done in the current single-crate layout so the later split is mechanical.

**Files:**
- Create: `src/output.rs`
- Modify: `src/search.rs` (remove all display code, return `Vec<SearchResult>`)
- Modify: `src/main.rs` (remove `OutputFormat` enum; add `mod output;`; Query arm renders via `output::render`)

**Interfaces:**
- Produces: `search::SearchResult { file_path: String, chunk_index: u64, score: f32, context: String, content: String }` (all fields `pub`, derives `Debug, Clone, Serialize`)
- Produces: `Searcher::search(&self, query_text: &str, limit: Option<usize>, use_rerank: bool) -> Result<Vec<SearchResult>>` (note: `format` parameter is GONE)
- Produces: `Searcher::msrch_dir(&self) -> PathBuf`
- Produces: `output::OutputFormat` (same four variants, same clap `ValueEnum` derive), `output::render(format: OutputFormat, query: &str, msrch_dir: &Path, results: &[SearchResult])`

- [ ] **Step 1: Write the failing test for `SearchResult::from_point`**

Add to the `tests` module at the bottom of `src/search.rs` (replacing the existing `unique_file_paths_dedupes_preserving_first_seen_order` test, which moves to output.rs in Step 5):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_point_extracts_payload_fields() {
        let point = ScoredPoint {
            id: "id".to_string(),
            score: 0.87,
            payload: json!({
                "file_path": "src/chunker.rs",
                "chunk_index": 4,
                "context": "impl::Chunker::fn::chunk_file",
                "content": "pub fn chunk_file(...)"
            }),
        };
        let result = SearchResult::from_point(&point);
        assert_eq!(result.file_path, "src/chunker.rs");
        assert_eq!(result.chunk_index, 4);
        assert_eq!(result.score, 0.87);
        assert_eq!(result.context, "impl::Chunker::fn::chunk_file");
        assert_eq!(result.content, "pub fn chunk_file(...)");
    }

    #[test]
    fn from_point_defaults_missing_payload_fields() {
        let point = ScoredPoint {
            id: "id".to_string(),
            score: 0.5,
            payload: json!({}),
        };
        let result = SearchResult::from_point(&point);
        assert_eq!(result.file_path, "unknown");
        assert_eq!(result.chunk_index, 0);
        assert_eq!(result.context, "");
        assert_eq!(result.content, "");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test from_point -- --nocapture`
Expected: compile error — `SearchResult` not found.

- [ ] **Step 3: Rewrite `src/search.rs` — data out, no display**

Replace the entire file contents above the `tests` module with:

```rust
use crate::config::Config;
use crate::db::{ScoredPoint, VectorDB};
use crate::embedding::EmbeddingClient;
use crate::reranker::RerankerClient;
use anyhow::{Context, Result};
use log::debug;
use serde::Serialize;
use std::env;
use std::path::PathBuf;

/// One search hit, fully extracted from the stored payload.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub file_path: String,
    pub chunk_index: u64,
    pub score: f32,
    pub context: String,
    pub content: String,
}

impl SearchResult {
    fn from_point(point: &ScoredPoint) -> Self {
        let p = &point.payload;
        Self {
            file_path: p
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            chunk_index: p.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0),
            score: point.score,
            context: p
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            content: p
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }
    }
}

pub struct Searcher {
    config: Config,
    index_root: PathBuf,
}

impl Searcher {
    pub async fn new(explicit_index: Option<PathBuf>) -> Result<Self> {
        let index_root = if let Some(path) = explicit_index {
            path
        } else {
            // Shared walk-up root discovery (see `index::find_index_root`).
            crate::index::find_index_root(&env::current_dir()?)
                .context("No .msrch index found in directory tree")?
        };

        let config = Config::load_global_config_or_default();

        Ok(Self { config, index_root })
    }

    /// The `.msrch` directory this searcher operates on.
    pub fn msrch_dir(&self) -> PathBuf {
        self.index_root.join(".msrch")
    }

    pub async fn search(
        &self,
        query_text: &str,
        limit: Option<usize>,
        use_rerank: bool,
    ) -> Result<Vec<SearchResult>> {
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;

        // Create reranker config, overriding enabled flag if --rerank passed
        let mut reranker_config = self.config.reranker.clone();
        if use_rerank {
            reranker_config.enabled = true;
        }
        let reranker = RerankerClient::new(reranker_config)?;

        let db = VectorDB::new(self.msrch_dir().join("index.db")).await?;

        let embedding = embedder.embed(vec![query_text.to_string()]).await?;
        let query_vector = embedding.first().context("No embedding generated")?.clone();

        let limit = limit.unwrap_or(self.config.query.default_limit);
        let min_score = self.config.query.min_similarity;

        // If reranker enabled, fetch more candidates for reranking
        let fetch_limit = if reranker.is_enabled() {
            reranker.top_n().max(limit)
        } else {
            limit
        };

        let mut results = db
            .search(query_vector, fetch_limit as u64, min_score)
            .await?;

        // Apply reranking if enabled
        if reranker.is_enabled() && !results.is_empty() {
            debug!("Reranking {} candidates", results.len());

            // Extract document contents for reranking
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

                    // Reorder results based on reranker scores
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

                    // Take top limit
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
        }

        Ok(results.iter().map(SearchResult::from_point).collect())
    }
}
```

Notes:
- `use colored::*;`, `use std::collections::HashSet;`, `JsonOutput`, `JsonResult`, all `display_*` methods, and `unique_file_paths` are deleted from this file (they move to `output.rs` in Step 5).
- The empty-results early return is deleted — `render` handles empties now.

- [ ] **Step 4: Run the search tests**

Run: `cargo test from_point`
Expected: 2 passed. (`main.rs` won't compile yet — that's Step 5/6. If the whole-crate build blocks the test, do Steps 5–6 first, then run.)

- [ ] **Step 5: Create `src/output.rs` with all rendering**

```rust
use crate::index::IndexStats;
use crate::search::SearchResult;
use clap::ValueEnum;
use colored::*;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputFormat {
    /// File paths only
    Plain,
    /// File paths with code snippets (default)
    #[default]
    Context,
    /// JSON output for scripting
    Json,
    /// Deduplicated file paths only (like `grep -l`)
    Filename,
}

#[derive(Serialize)]
struct JsonOutput {
    query: String,
    index_path: String,
    results: Vec<JsonResult>,
}

#[derive(Serialize)]
struct JsonResult {
    file_path: String,
    chunk_index: u64,
    similarity: f32,
    context: String,
    content: String,
}

/// Render search results in the requested format. Handles the empty case.
pub fn render(format: OutputFormat, query: &str, msrch_dir: &Path, results: &[SearchResult]) {
    if results.is_empty() {
        match format {
            OutputFormat::Json => println!(
                "{}",
                serde_json::json!({
                    "query": query,
                    "index_path": msrch_dir.display().to_string(),
                    "results": []
                })
            ),
            _ => println!("No results found."),
        }
        return;
    }

    match format {
        OutputFormat::Plain => display_plain(results),
        OutputFormat::Context => display_context(results),
        OutputFormat::Json => display_json(query, msrch_dir, results),
        OutputFormat::Filename => display_filename(results),
    }
}

fn display_plain(results: &[SearchResult]) {
    for result in results {
        println!("{}:{}", result.file_path, result.chunk_index);
    }
}

fn display_context(results: &[SearchResult]) {
    println!("{}", format!("Found {} results:", results.len()).bold());
    for result in results {
        let context_suffix = if result.context.is_empty() {
            String::new()
        } else {
            format!("  {}", result.context.dimmed())
        };

        println!(
            "\n{} {}:{}{}",
            format!("{:.2}", result.score).yellow(),
            result.file_path.cyan(),
            result.chunk_index,
            context_suffix
        );

        for line in result.content.lines().take(3) {
            println!("  │ {}", line);
        }
    }
}

fn display_json(query: &str, msrch_dir: &Path, results: &[SearchResult]) {
    let json_results: Vec<JsonResult> = results
        .iter()
        .map(|r| JsonResult {
            file_path: r.file_path.clone(),
            chunk_index: r.chunk_index,
            similarity: r.score,
            context: r.context.clone(),
            content: r.content.clone(),
        })
        .collect();

    let output = JsonOutput {
        query: query.to_string(),
        index_path: msrch_dir.display().to_string(),
        results: json_results,
    };

    match serde_json::to_string_pretty(&output) {
        Ok(text) => println!("{}", text),
        Err(e) => eprintln!("Failed to serialize results: {}", e),
    }
}

fn display_filename(results: &[SearchResult]) {
    for file_path in unique_file_paths(results) {
        println!("{}", file_path);
    }
}

/// Collect the distinct `file_path` values from results, preserving the order
/// in which each path is first seen (so the most relevant file leads).
fn unique_file_paths(results: &[SearchResult]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for result in results {
        if seen.insert(result.file_path.clone()) {
            paths.push(result.file_path.clone());
        }
    }
    paths
}

/// Pretty-print index statistics (moved from `IndexStats::print`).
pub fn print_stats(stats: &IndexStats) {
    println!("{}", "Index Statistics".bold().underline());
    println!();
    println!("  {:<18} {}", "Index:".cyan(), stats.index_path.display());
    println!("  {:<18} {}", "Root:".cyan(), stats.root_path.display());
    println!("  {:<18} {}", "Files:".cyan(), stats.file_count);
    println!("  {:<18} {}", "Chunks:".cyan(), stats.chunk_count);
    println!("  {:<18} ~{}", "Est. tokens:".cyan(), stats.estimated_tokens);
    println!("  {:<18} {}", "Model:".cyan(), stats.model);
    println!("  {:<18} {}", "Endpoint:".cyan(), stats.endpoint);

    if let Some(last) = stats.last_indexed {
        if let Ok(duration) = last.duration_since(std::time::SystemTime::UNIX_EPOCH) {
            let datetime = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            println!("  {:<18} {}", "Last indexed:".cyan(), datetime);
        }
    }

    let size_str = if stats.size_on_disk >= 1024 * 1024 {
        format!("{:.1} MB", stats.size_on_disk as f64 / (1024.0 * 1024.0))
    } else if stats.size_on_disk >= 1024 {
        format!("{:.1} KB", stats.size_on_disk as f64 / 1024.0)
    } else {
        format!("{} bytes", stats.size_on_disk)
    };
    println!("  {:<18} {}", "Size on disk:".cyan(), size_str);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(file_path: &str) -> SearchResult {
        SearchResult {
            file_path: file_path.to_string(),
            chunk_index: 0,
            score: 1.0,
            context: String::new(),
            content: String::new(),
        }
    }

    #[test]
    fn unique_file_paths_dedupes_preserving_first_seen_order() {
        let results = vec![
            result("src/a.rs"),
            result("src/b.rs"),
            result("src/a.rs"),
            result("src/c.rs"),
            result("src/b.rs"),
        ];
        assert_eq!(
            unique_file_paths(&results),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
    }
}
```

Note: `SearchResult` field construction in the test requires its fields to be `pub` (they are, per Task 1 Step 3). While this file lives in the same crate, `use crate::...` paths are correct; Task 3 changes them to `use msrch_core::...`.

Also in this step: delete the now-moved `print` method from `impl IndexStats` in `src/index.rs` (keep the struct and its pub fields; delete the whole `impl IndexStats { ... }` block, lines 54–84; keep `use colored::*;` — the rest of index.rs still uses it). The `chrono` usage leaves index.rs with this deletion.

- [ ] **Step 6: Update `src/main.rs`**

- Delete the `OutputFormat` enum (lines 14–25).
- Add `mod output;` to the module list and `use output::OutputFormat;` below the existing `use` lines.
- Replace the Query arm:

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
    let results = searcher
        .search(text, *limit, *rerank)
        .await
        .context("Search failed")?;
    output::render(*format, text, &searcher.msrch_dir(), &results);
}
```

- Replace the Stats arm:

```rust
Commands::Stats => {
    let current_dir = std::env::current_dir()?;
    let stats = index::get_stats(&current_dir).await?;
    output::print_stats(&stats);
}
```

- [ ] **Step 7: Full test suite + behavior check**

Run: `cargo test`
Expected: all tests pass, including the three existing CLI-parsing tests in main.rs (they use `OutputFormat` via `use super::*;` → now resolved through `use output::OutputFormat;`).

Run: `cargo clippy 2>&1 | tail -5`
Expected: no new warnings.

Run: `cargo run -q -- --help`
Expected: same help output as before.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: search returns data; rendering moves to output module"
```

---

### Task 2: Move `similar` logic into `Searcher`; fix UTF-8 truncation panic

The Similar arm in main.rs is ~90 lines of logic (embedding, DB search, dedup) — the largest zero-logic violation. It also contains a latent panic: `content[..8000]` slices at a byte offset that may not be a char boundary (any multibyte file > 8000 bytes panics).

**Files:**
- Modify: `src/search.rs` (add `SimilarFile`, `Searcher::find_similar`, `truncate_at_char_boundary`, `dedupe_similar`)
- Modify: `src/output.rs` (add `print_similar`)
- Modify: `src/main.rs` (Similar arm shrinks to path checks + calls)

**Interfaces:**
- Consumes: `Searcher::new`, `Searcher::msrch_dir` from Task 1.
- Produces: `search::SimilarFile { file_path: String, score: f32 }` (fields `pub`, derives `Debug, Clone`)
- Produces: `Searcher::find_similar(&self, file: &Path, max_results: usize) -> Result<Vec<SimilarFile>>`
- Produces: `output::print_similar(results: &[SimilarFile])`

**Deliberate behavior changes (document in commit):** (1) if the embedding call fails, `similar` previously printed to stderr and exited 0; now the error propagates and the process exits non-zero. (2) The "Finding files similar to" header prints after index discovery but before file read/embedding, so a missing `.msrch` index emits only the original error text (byte-identical to the old binary), while errors from reading the file, embedding, or opening the index db appear after the header — a declared residual of the core/CLI split.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/search.rs`:

```rust
#[test]
fn truncate_respects_char_boundaries() {
    // 'é' is 2 bytes in UTF-8; byte 5 falls mid-char.
    let s = "ééééé"; // 10 bytes, 5 chars
    let t = truncate_at_char_boundary(s, 5);
    assert_eq!(t, "éé"); // 4 bytes; byte 5 would split the third 'é'
    assert_eq!(truncate_at_char_boundary(s, 100), s); // shorter than max: untouched
    assert_eq!(truncate_at_char_boundary("", 8000), "");
}

#[test]
fn dedupe_similar_excludes_query_file_dedupes_and_caps() {
    fn point(file_path: &str, score: f32) -> ScoredPoint {
        ScoredPoint {
            id: "id".to_string(),
            score,
            payload: serde_json::json!({ "file_path": file_path }),
        }
    }
    let results = vec![
        point("/repo/self.rs", 1.0),  // the query file: excluded
        point("/repo/a.rs", 0.9),
        point("/repo/a.rs", 0.8),     // duplicate: dropped, first score kept
        point("/repo/b.rs", 0.7),
        point("/repo/c.rs", 0.6),
    ];
    let out = dedupe_similar(&results, "/repo/self.rs", 2);
    assert_eq!(out.len(), 2); // capped at max
    assert_eq!(out[0].file_path, "/repo/a.rs");
    assert_eq!(out[0].score, 0.9);
    assert_eq!(out[1].file_path, "/repo/b.rs");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test 'truncate_respects|dedupe_similar' 2>&1 | tail -5`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement in `src/search.rs`**

Add `use std::collections::HashSet;` and `use std::path::Path;` to the imports, then below the `Searcher` impl:

```rust
/// A similar-file hit for `msrch similar`.
#[derive(Debug, Clone)]
pub struct SimilarFile {
    pub file_path: String,
    pub score: f32,
}

impl Searcher {
    /// Find files semantically similar to `file` (the file itself is excluded).
    pub async fn find_similar(&self, file: &Path, max_results: usize) -> Result<Vec<SimilarFile>> {
        let content = std::fs::read_to_string(file).context("Failed to read file")?;
        if content.trim().is_empty() {
            anyhow::bail!("File is empty");
        }

        // Truncate to fit model limits without splitting a UTF-8 char.
        let truncated = truncate_at_char_boundary(&content, 8000);

        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;
        let embeddings = embedder.embed(vec![truncated.to_string()]).await?;
        let query_vector = embeddings
            .into_iter()
            .next()
            .context("No embedding generated")?;

        let db = VectorDB::new(self.msrch_dir().join("index.db")).await?;
        let results = db.search(query_vector, 20, 0.0).await?;

        Ok(dedupe_similar(
            &results,
            &file.display().to_string(),
            max_results,
        ))
    }
}

/// Largest prefix of `s` that is at most `max_bytes` long and ends on a char boundary.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    let mut end = s.len().min(max_bytes);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Deduplicate by file path (best score first-seen), excluding the query file,
/// capped at `max` entries.
fn dedupe_similar(results: &[ScoredPoint], exclude_path: &str, max: usize) -> Vec<SimilarFile> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for result in results {
        let file_path = result
            .payload
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if file_path == exclude_path || !seen.insert(file_path.clone()) {
            continue;
        }
        out.push(SimilarFile {
            file_path,
            score: result.score,
        });
        if out.len() >= max {
            break;
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test 'truncate_respects|dedupe_similar'`
Expected: 2 passed.

- [ ] **Step 5: Add `print_similar` to `src/output.rs`**

```rust
use crate::search::SimilarFile;
```
(merge into the existing `use crate::search::...` line: `use crate::search::{SearchResult, SimilarFile};`)

```rust
/// Print `msrch similar` results (moved from main.rs).
pub fn print_similar(results: &[SimilarFile]) {
    if results.is_empty() {
        println!("No similar files found.");
    } else {
        println!(
            "{}",
            format!("\nFound {} similar files:", results.len()).bold()
        );
        for similar in results {
            println!(
                "  {} {}",
                format!("{:.2}", similar.score).yellow(),
                similar.file_path.cyan()
            );
        }
    }
}
```

- [ ] **Step 6: Shrink the Similar arm in `src/main.rs`**

Replace the entire `Commands::Similar { file } => { ... }` arm with:

```rust
Commands::Similar { file } => {
    use colored::*;

    let file_path = std::fs::canonicalize(file).unwrap_or(file.clone());
    if !file_path.exists() {
        anyhow::bail!("File not found: {}", file_path.display());
    }

    let searcher = search::Searcher::new(None).await?;

    println!(
        "Finding files similar to: {}",
        file_path.display().to_string().cyan()
    );

    let results = searcher.find_similar(&file_path, 10).await?;
    output::print_similar(&results);
}
```

Also delete the now-unused `use std::collections::HashSet;` inside the old arm (it was local) and remove the `db`/`embedding` module references from main.rs if nothing else uses them (`mod db;` etc. must STAY — modules are declared in main.rs until Task 3 — but the `use`-free direct references disappear naturally with the arm rewrite).

- [ ] **Step 7: Full suite + clippy**

Run: `cargo test && cargo clippy 2>&1 | tail -3`
Expected: all pass; no new warnings.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: move similar-files logic into Searcher; fix UTF-8 truncation panic

The embed-failure path now propagates an error (non-zero exit) instead of
printing to stderr and exiting 0."
```

---

### Task 3: Split into `msrch-core` (lib) + `msrch` (cli) workspace crates

Pure mechanics now: no signatures change, files move, `crate::` paths in the cli become `msrch_core::`.

**Files:**
- Create: `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/cli/Cargo.toml`
- Move (git mv): `src/{chunker,config,crawler,db,embedding,index,reranker,search}.rs` → `crates/core/src/`; `src/{main,output}.rs` → `crates/cli/src/`
- Rewrite: `Cargo.toml` (workspace manifest)

**Interfaces:**
- Consumes: everything from Tasks 1–2.
- Produces: library crate `msrch-core` exposing `pub mod chunker, config, crawler, db, embedding, index, reranker, search`; binary crate `msrch`.

- [ ] **Step 1: Move the files (preserving git history)**

```bash
mkdir -p crates/core/src crates/cli/src
git mv src/chunker.rs src/config.rs src/crawler.rs src/db.rs src/embedding.rs src/index.rs src/reranker.rs src/search.rs crates/core/src/
git mv src/main.rs src/output.rs crates/cli/src/
rmdir src
```

- [ ] **Step 2: Write the workspace root `Cargo.toml`** (full replacement)

```toml
[workspace]
resolver = "3"
members = ["crates/core", "crates/cli"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
anyhow = "1.0.100"
arrow = "58.3.0"
chrono = "0.4"
clap = { version = "4.5.53", features = ["derive", "env"] }
colored = "3.0.0"
confy = "2.0.0"
env_logger = "0.11"
futures = "0.3.31"
ignore = "0.4.25"
indicatif = "0.18.3"
lancedb = "0.31.0"
log = "0.4"
reqwest = { version = "0.12.28", features = ["json", "blocking"] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.147"
tempfile = "3"
tiktoken-rs = "0.9.1"
tokio = { version = "1.48.0", features = ["full"] }
toml = "0.9.10"
tree-sitter = "0.26.3"
tree-sitter-go = "0.25.0"
tree-sitter-javascript = "0.25.0"
tree-sitter-python = "0.25.0"
tree-sitter-rust = "0.24.0"
tree-sitter-typescript = "0.23.2"
uuid = { version = "1.19.0", features = ["serde", "v4"] }
msrch-core = { path = "crates/core" }
```

- [ ] **Step 3: Write `crates/core/Cargo.toml`**

```toml
[package]
name = "msrch-core"
version.workspace = true
edition.workspace = true

[dependencies]
anyhow.workspace = true
arrow.workspace = true
confy.workspace = true
futures.workspace = true
ignore.workspace = true
indicatif.workspace = true
lancedb.workspace = true
log.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
tiktoken-rs.workspace = true
tokio.workspace = true
toml.workspace = true
tree-sitter.workspace = true
tree-sitter-go.workspace = true
tree-sitter-javascript.workspace = true
tree-sitter-python.workspace = true
tree-sitter-rust.workspace = true
tree-sitter-typescript.workspace = true
uuid.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

(`chrono` and `colored` are deliberately absent: their only uses moved to the cli in Task 1 — Task 1's review confirmed index.rs has no remaining colored-trait calls.)

- [ ] **Step 4: Write `crates/cli/Cargo.toml`**

```toml
[package]
name = "msrch"
version.workspace = true
edition.workspace = true

[[bin]]
name = "msrch"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
chrono.workspace = true
clap.workspace = true
colored.workspace = true
env_logger.workspace = true
log.workspace = true
msrch-core.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
```

- [ ] **Step 5: Write `crates/core/src/lib.rs`**

```rust
pub mod chunker;
pub mod config;
pub mod crawler;
pub mod db;
pub mod embedding;
pub mod index;
pub mod reranker;
pub mod search;
```

- [ ] **Step 6: Update `crates/cli/src/main.rs` and `output.rs` imports**

In `main.rs`: delete the eight `mod chunker; ... mod search;` lines; keep `mod output;`. Add:

```rust
use msrch_core::{config, index, search};
```

In `output.rs`: change the two `use crate::...` lines to:

```rust
use msrch_core::index::IndexStats;
use msrch_core::search::{SearchResult, SimilarFile};
```

- [ ] **Step 7: Build, test, smoke-test**

Run: `cargo build 2>&1 | tail -3`
Expected: clean build. If `crate::` paths inside crates/core files error, they are core-internal references and remain `crate::` (they resolve to msrch-core's own crate root via lib.rs) — only cli-side files switch to `msrch_core::`.

Run: `cargo test --workspace`
Expected: all tests pass across both crates.

Run: `cargo build --release && ls target/release/msrch && ./target/release/msrch --help | head -5`
Expected: binary exists at the same path the Makefile `install` target copies from; help unchanged.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: split into msrch-core lib and msrch cli crates"
```

---

### Task 4: Wire project-level `.msrch/config.toml` into the config hierarchy

**Files:**
- Modify: `crates/core/src/config.rs` (add `deep_merge`, `overlay_project_config`, `load_for_index`; delete unused `load_from_path`)
- Modify: `crates/core/src/search.rs` (`Searcher::new` uses `load_for_index`)
- Modify: `crates/cli/src/main.rs` (Index/Reindex/Config arms; Reindex stops deleting the project config)
- Modify: `CLAUDE.md`, `README.md` (config-hierarchy docs)
- Test: inline `#[cfg(test)]` in `crates/core/src/config.rs`

**Interfaces:**
- Produces: `Config::load_for_index(index_root: &Path) -> Config` — global config overlaid with `<index_root>/.msrch/config.toml`, field-by-field, project wins; tolerant of missing/malformed project file (warning to stderr, falls back to global). Callers pass the *index root* (the directory containing `.msrch/`), not the `.msrch` dir itself.

**Behavior change (document in commit):** `msrch reindex` previously deleted the entire `.msrch/` directory — which would destroy a project `config.toml`. It now deletes only `index.db/` and `manifest.json`.

- [ ] **Step 1: Write the failing tests**

Replace the `tests` module in `crates/core/src/config.rs` — keep the existing `config_tolerates_missing_fields_and_sections` test and add:

```rust
#[test]
fn deep_merge_overlays_nested_tables_field_by_field() {
    let mut base: toml::Value = toml::from_str(
        r#"
[query]
default_limit = 10
min_similarity = 0.5

[embedding]
model = "global-model"
"#,
    )
    .unwrap();
    let overlay: toml::Value = toml::from_str(
        r#"
[query]
default_limit = 3

[reranker]
enabled = true
"#,
    )
    .unwrap();

    deep_merge(&mut base, overlay);

    // Overlaid field wins:
    assert_eq!(base["query"]["default_limit"].as_integer(), Some(3));
    // Sibling field in the same table survives:
    assert_eq!(base["query"]["min_similarity"].as_float(), Some(0.5));
    // Untouched table survives:
    assert_eq!(base["embedding"]["model"].as_str(), Some("global-model"));
    // Table only in overlay is added:
    assert_eq!(base["reranker"]["enabled"].as_bool(), Some(true));
}

#[test]
fn overlay_project_config_missing_file_returns_base() {
    let dir = tempfile::tempdir().unwrap();
    let base = Config::default();
    let merged =
        Config::overlay_project_config(base, &dir.path().join(".msrch").join("config.toml"));
    assert_eq!(merged.query.default_limit, Config::default().query.default_limit);
    assert_eq!(merged.embedding.model, Config::default().embedding.model);
}

#[test]
fn overlay_project_config_partial_file_overrides_only_named_fields() {
    let dir = tempfile::tempdir().unwrap();
    let msrch_dir = dir.path().join(".msrch");
    std::fs::create_dir_all(&msrch_dir).unwrap();
    let config_path = msrch_dir.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[query]
default_limit = 3

[embedding]
model = "project-model"
"#,
    )
    .unwrap();

    let merged = Config::overlay_project_config(Config::default(), &config_path);

    // Project values win:
    assert_eq!(merged.query.default_limit, 3);
    assert_eq!(merged.embedding.model, "project-model");
    // Everything the project file doesn't name is untouched:
    assert_eq!(merged.query.min_similarity, Config::default().query.min_similarity);
    assert_eq!(merged.embedding.endpoint, Config::default().embedding.endpoint);
    assert_eq!(merged.chunking.max_chunk_tokens, Config::default().chunking.max_chunk_tokens);
}

#[test]
fn overlay_project_config_malformed_file_returns_base() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "this is not [valid toml").unwrap();

    let merged = Config::overlay_project_config(Config::default(), &config_path);
    assert_eq!(merged.query.default_limit, Config::default().query.default_limit);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core config:: 2>&1 | tail -5`
Expected: compile error — `deep_merge` / `overlay_project_config` not defined.

- [ ] **Step 3: Implement in `crates/core/src/config.rs`**

Add `use std::path::Path;` to imports (keep `PathBuf` only if still used — `load_from_path` is deleted below). Add inside `impl Config`, replacing the `load_from_path` method and the `// Example helper` comment:

```rust
/// Effective config for an index root: the global config overlaid with the
/// project's `.msrch/config.toml`, field by field (project wins).
/// Precedence overall: CLI flags > project config > global config > defaults;
/// the first is applied by callers, the rest by this function.
pub fn load_for_index(index_root: &Path) -> Self {
    let global = Self::load_global_config_or_default();
    let project_path = index_root.join(".msrch").join("config.toml");
    Self::overlay_project_config(global, &project_path)
}

/// Overlay the config file at `project_path` (if present) onto `base`.
/// Missing file is normal (returns base). A malformed file or invalid values
/// warn to stderr and return base, mirroring `load_global_config_or_default`.
fn overlay_project_config(base: Config, project_path: &Path) -> Config {
    let text = match std::fs::read_to_string(project_path) {
        Ok(text) => text,
        Err(_) => return base,
    };
    let overlay: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            eprintln!(
                "warning: failed to parse {}: {e}; using global config",
                project_path.display()
            );
            return base;
        }
    };
    // Round-trip the base through toml so we can merge at the value level;
    // this is what lets a project file override single fields without
    // clobbering whole sections.
    let base_text = match toml::to_string(&base) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("warning: failed to serialize config for merge: {e}");
            return base;
        }
    };
    let mut merged: toml::Value = match toml::from_str(&base_text) {
        Ok(value) => value,
        Err(e) => {
            eprintln!("warning: failed to re-parse config for merge: {e}");
            return base;
        }
    };
    deep_merge(&mut merged, overlay);
    let merged_text = match toml::to_string(&merged) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("warning: failed to serialize merged config: {e}");
            return base;
        }
    };
    match toml::from_str(&merged_text) {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "warning: invalid values in {}: {e}; using global config",
                project_path.display()
            );
            base
        }
    }
}
```

And as a free function below the impl block:

```rust
/// Merge `overlay` into `base`: tables merge recursively; every other value
/// type (including arrays) replaces wholesale.
fn deep_merge(base: &mut toml::Value, overlay: toml::Value) {
    match overlay {
        toml::Value::Table(overlay_map) => {
            if let toml::Value::Table(base_map) = base {
                for (key, value) in overlay_map {
                    match base_map.get_mut(&key) {
                        Some(existing) => deep_merge(existing, value),
                        None => {
                            base_map.insert(key, value);
                        }
                    }
                }
            } else {
                *base = toml::Value::Table(overlay_map);
            }
        }
        other => *base = other,
    }
}
```

Delete the now-superseded `load_from_path` method (it was never called).

Note: `overlay_project_config` is private but reachable from the tests module (`use super::*`); make it `pub(crate)` only if the compiler complains about test access — it should not, same module file.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p msrch-core config::`
Expected: 5 passed (4 new + 1 existing).

- [ ] **Step 5: Switch the callers**

`crates/core/src/search.rs`, in `Searcher::new` — replace:

```rust
        let config = Config::load_global_config_or_default();
```

with:

```rust
        // Global config overlaid with this index's .msrch/config.toml.
        let config = Config::load_for_index(&index_root);
```

(and delete the stale `// Load config from index or global?...POC` comment above it if still present).

`crates/cli/src/main.rs` — Index arm: replace `let config = config::Config::load_global_config_or_default();` with:

```rust
            let root_path = std::fs::canonicalize(path).unwrap_or(path.clone());
            let config = config::Config::load_for_index(&root_path);
```

(note the canonicalize line moves ABOVE the config load so the project config is found; delete the old duplicate canonicalize line below).

Reindex arm — full replacement:

```rust
        Commands::Reindex => {
            let current_dir = std::env::current_dir()?;
            let root_path = index::find_index_root(&current_dir)
                .context("No .msrch index found in directory tree")?;
            // Load the effective config BEFORE touching .msrch, and remove only
            // the index artifacts so a project config.toml survives the rebuild.
            let config = config::Config::load_for_index(&root_path);
            let msrch_dir = root_path.join(".msrch");
            let db_path = msrch_dir.join("index.db");
            if db_path.exists() {
                std::fs::remove_dir_all(&db_path).context("Failed to remove old index db")?;
            }
            let manifest_path = msrch_dir.join("manifest.json");
            if manifest_path.exists() {
                std::fs::remove_file(&manifest_path).context("Failed to remove old manifest")?;
            }
            let indexer = index::Indexer::new(root_path, config);
            indexer.index().await.context("Reindexing failed")?;
            println!("Reindexing completed successfully.");
        }
```

Config arm — full replacement:

```rust
        Commands::Config => {
            let current_dir = std::env::current_dir()?;
            match index::find_index_root(&current_dir) {
                Some(root) => {
                    println!("# effective config for index at {}", root.display());
                    println!("{:#?}", config::Config::load_for_index(&root));
                }
                None => {
                    println!("# global config (no .msrch index found)");
                    println!("{:#?}", config::Config::load_global_config_or_default());
                }
            }
        }
```

- [ ] **Step 6: Full suite + manual end-to-end check**

Run: `cargo test --workspace && cargo clippy 2>&1 | tail -3`
Expected: all pass, no new warnings.

Manual check (uses the scratch dir, no network needed for the config path):

```bash
cd "$(mktemp -d)" && mkdir -p .msrch && printf '[query]\ndefault_limit = 3\n' > .msrch/config.toml
<worktree>/target/debug/msrch config | grep -A2 "default_limit"
```

Expected: `default_limit: 3` (project override visible), other fields at global/default values, header line names the index root.

- [ ] **Step 7: Update the docs**

`CLAUDE.md` — three edits:
1. "Key Modules" bullet for `config.rs`: change `(project-specific overrides not yet wired up)` to `(global + project-level overrides)`.
2. "Config Hierarchy" section: replace the list and trailing note with:
```markdown
1. CLI flags: `--limit`, `--rerank`, etc.
2. Project config: `.msrch/config.toml` in the index root (field-by-field overlay)
3. Global User config: `~/.config/msrch/config.toml` (via `confy`)
4. Hardcoded defaults in `config.rs::Default` implementations
```
3. "Config Loading" section: replace the `load_from_path` bullet with: `- Project: merged via Config::load_for_index(index_root) — global config overlaid with .msrch/config.toml (project wins field-by-field; malformed project file warns and is ignored)`, and drop `project configs` from the pending-implementation note.

`README.md`: if it documents the config hierarchy (`grep -n "config" README.md`), mirror the same 4-level list; if it doesn't, add nothing.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: wire project-level .msrch/config.toml into config hierarchy

Precedence: CLI flags > project config > global config > defaults.
msrch reindex now removes only index.db and manifest.json so a project
config.toml survives a rebuild (previously the whole .msrch dir was deleted)."
```

---

## Out of scope (known, deliberate)

- `index.rs` still prints progress (`println!`, `indicatif`, `colored`) from core. Acceptable for the CLI; must be revisited before the MCP server exposes *indexing* over stdio (stdout is the protocol channel there). Tracked implicitly by ROADMAP item 4.
- `Searcher::search` still constructs its own `EmbeddingClient`/`VectorDB` per call; fine at CLI granularity, revisit for a long-running MCP server (ROADMAP item 4's index-lifecycle decision).

## Self-review notes

- Spec coverage: workspace split (Task 3), zero logic in handlers (Tasks 1–2 move search rendering + similar logic + stats printing out of core/main; remaining arms are load-call-print thin), project config wiring (Task 4), tests green throughout (every task ends with full suite + commit). ✓
- JSON contract preserved via `JsonResult.similarity` mapping in output.rs. ✓
- Type consistency: `SearchResult`/`SimilarFile`/`render`/`print_similar`/`print_stats`/`load_for_index` names and signatures match across all tasks. ✓
- Known behavior changes are called out in Task 2 (similar embed-failure exit code) and Task 4 (reindex preserves config.toml) and documented in their commit messages. ✓
