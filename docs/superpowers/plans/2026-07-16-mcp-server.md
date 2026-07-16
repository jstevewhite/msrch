# MCP Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `msrch mcp` serves search/stats/list_indexes over MCP (stdio + streamable-HTTP) via a new `crates/mcp` thin front-end (spec: `docs/superpowers/specs/2026-07-16-mcp-server-design.md`). Release 0.6.0.

**Architecture:** `crates/mcp` (package `msrch-mcp`) owns rmcp, an immutable startup-built `IndexRegistry` of named roots, three tool handlers that are plain async methods (directly unit-testable without any transport), and `serve(McpOptions)`. The CLI gains one `Mcp` subcommand whose arm makes one call. The `dates` parser relocates from cli to core so both front-ends parse date strings identically.

**Tech Stack:** rmcp 2.2.0 (features: `server`, `macros`, `transport-io`, `transport-streamable-http-server`, `schemars` — verified via `cargo info`), axum 0.8.9 (HTTP host for rmcp's tower service), schemars for tool arg schemas, chrono (moves into core with dates; also used by mcp for ISO timestamps).

## Global Constraints

- Registry semantics verbatim from spec: `--index name=path` (split on FIRST `=`) or bare `path` (name = directory basename); no flags → walk-up discovery from cwd; every root must contain `.msrch/` (startup error naming the path); duplicate names error; `resolve(None)` with >1 entries errors listing names; unknown name errors listing valid names.
- Tools: `search` (query, index?, limit?, rerank?, min_similarity? 0.0..=1.0, path_contains?, after?/before? as `YYYY-MM-DD`/`Nd`/`Nw`/`Nm` strings), `stats(index?)`, `list_indexes()`. Search result JSON mirrors the CLI contract (`similarity` not `score`) plus `index`, `query`, and `auto_index_refreshed` (only when > 0).
- Per-request open: no held Searcher/DB/table handles between calls.
- Auto-index inside `search`: same quiet non-fatal `index_quiet` machinery, config-gated per root; failures warn to the SERVER's stderr and the search proceeds.
- HTTP default bind `127.0.0.1:7920`; `--bind` with `--transport stdio` is a startup error. No auth in v1.
- `msrch-core` stays tokio-free in `[dependencies]` (mcp and cli are front-ends — tokio is fine there). Clients never supply filesystem paths — registry names only.
- CLI behavior byte-identical after the dates move (same error strings, same flags).
- `cargo test --workspace` green at every commit (baseline 89); clippy no new warnings (~24 baseline); no production `unwrap()`; `anyhow` + `.context()`.
- Version 0.5.0 → 0.6.0 in Task 5; `git tag v0.6.0` on main after merge (controller/human). No schema change.
- **rmcp adaptation clause:** Task 3's rmcp/axum code is written against rmcp 2.2.0's documented API (`#[tool_router]`/`#[tool]`/`#[tool_handler]` macros, `Parameters<T>` extractor, `ServiceExt::serve` with `rmcp::transport::stdio()`, `StreamableHttpService` + `LocalSessionManager` mounted on axum). If names/signatures differ at compile time, consult docs.rs/rmcp/2.2.0 and adapt MECHANICALLY, noting every adaptation. If rmcp 2.2.0 cannot express a required piece (no stdio transport, no streamable-http server service, no schema-carrying tool macro), STOP and report BLOCKED with what you found.

## File Structure (end state)

```
crates/
├── core/src/dates.rs        # MOVED from crates/cli (chrono joins core deps)
├── core/src/lib.rs          # + pub mod dates;
├── cli/src/main.rs          # dates::* → msrch_core::dates::*; + Mcp subcommand
├── cli/Cargo.toml           # + msrch-mcp dep
└── mcp/                     # NEW package msrch-mcp
    ├── Cargo.toml
    └── src/
        ├── lib.rs           # pub use options/serve; mod registry; mod server;
        ├── registry.rs      # IndexEntry, IndexRegistry (+tests)
        └── server.rs        # McpOptions, tool handlers, serve() (+handler tests)
docs/AGENTS-SNIPPET.md / README.md / CLAUDE.md / CHANGELOG.md   # Task 5
```

---

### Task 1: Relocate `dates` from cli to core

**Files:**
- Move: `crates/cli/src/dates.rs` → `crates/core/src/dates.rs` (git mv)
- Modify: `crates/core/src/lib.rs` (+ `pub mod dates;` alphabetical), `crates/core/Cargo.toml` (+ `chrono.workspace = true`), `crates/cli/src/main.rs` (drop `mod dates;`, point the two value_parsers at core)

**Interfaces:**
- Produces: `msrch_core::dates::parse_date_arg(s: &str) -> Result<std::time::SystemTime, String>` — signature, behavior, and error strings BYTE-IDENTICAL to today (Task 3 consumes it; the CLI keeps consuming it).

- [ ] **Step 1: Move the file and rewire**

```bash
git mv crates/cli/src/dates.rs crates/core/src/dates.rs
```

- `crates/core/src/lib.rs`: add `pub mod dates;` between `crawler` and `db`.
- `crates/core/Cargo.toml` `[dependencies]`: add `chrono.workspace = true` (alphabetical). (chrono is already in the workspace table; the module's only external dep.)
- `crates/core/src/dates.rs`: change `pub fn parse_date_arg` doc comment's first line to `/// Date-argument parser shared by the CLI (clap value_parser) and MCP front-ends.` — no code changes. The `resolve_with_now` helper and all four tests move as-is.
- `crates/cli/src/main.rs`: delete the `mod dates;` line; change both `value_parser = dates::parse_date_arg` occurrences (Cli global + Query variant) to `value_parser = msrch_core::dates::parse_date_arg`.

- [ ] **Step 2: Verify — suite green, counts shifted, CLI text identical**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 89 total (4 dates tests now under core: core 79, cli 9, integration 1).
Run: `cargo run -q -- "q" --after tomorrow 2>&1 | head -2`
Expected: byte-identical error to 0.5.0 (`could not parse date 'tomorrow'; accepted forms: YYYY-MM-DD, or relative 7d / 2w / 3m (days/weeks/months ago)`).
Run: `cargo clippy 2>&1 | tail -3` — no new warnings.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor: move dates parser into core for front-end sharing"
```
(Append the standard Claude Code footer.)

---

### Task 2: `crates/mcp` scaffold — options + index registry (no rmcp usage yet)

**Files:**
- Create: `crates/mcp/Cargo.toml`, `crates/mcp/src/lib.rs`, `crates/mcp/src/registry.rs`
- Modify: root `Cargo.toml` (workspace member + deps table entries)

**Interfaces:**
- Produces: `msrch_mcp::registry::{IndexEntry { pub name: String, pub root: PathBuf }, IndexRegistry}` with
  `IndexRegistry::from_flags(flags: &[String]) -> anyhow::Result<Self>`,
  `IndexRegistry::discover(cwd: &Path) -> anyhow::Result<Self>`,
  `IndexRegistry::resolve(&self, index: Option<&str>) -> anyhow::Result<&IndexEntry>`,
  `IndexRegistry::entries(&self) -> &[IndexEntry]`.

- [ ] **Step 1: Manifests**

Root `Cargo.toml`: `members = ["crates/core", "crates/cli", "crates/mcp"]`; `[workspace.dependencies]` gains (alphabetical):

```toml
axum = "0.8.9"
rmcp = { version = "2.2.0", features = ["server", "macros", "transport-io", "transport-streamable-http-server", "schemars"] }
schemars = "1"
msrch-mcp = { path = "crates/mcp" }
```

(If `schemars = "1"` conflicts with the version rmcp 2.2.0 re-exports/expects, match rmcp's — check `cargo tree -i schemars` after the first build and note the adaptation.)

`crates/mcp/Cargo.toml`:

```toml
[package]
name = "msrch-mcp"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
axum.workspace = true
chrono.workspace = true
log.workspace = true
msrch-core.workspace = true
rmcp.workspace = true
schemars.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/mcp/src/lib.rs`:

```rust
//! MCP front-end for msrch: index registry, tool handlers, and transport
//! startup. All search/index logic lives in msrch-core; this crate is a
//! thin protocol adapter, like the CLI.

pub mod registry;
pub mod server;

pub use server::{McpOptions, TransportKind, serve};
```

(Leave `server.rs` as a compiling stub for this task: the `McpOptions`/`TransportKind` types and a `serve` that `anyhow::bail!("serve: implemented in the rmcp task")` — full definitions below in Step 3 so the crate builds and Task 4 can reference the types.)

- [ ] **Step 2: Write the failing registry tests**

`crates/mcp/src/registry.rs`, tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_index_root(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join(".msrch")).unwrap();
        root
    }

    #[test]
    fn from_flags_parses_named_and_bare_forms() {
        let dir = tempfile::tempdir().unwrap();
        let a = make_index_root(&dir, "alpha");
        let b = make_index_root(&dir, "reports-2026");
        let flags = vec![
            format!("work={}", a.display()),
            b.display().to_string(),
        ];
        let reg = IndexRegistry::from_flags(&flags).unwrap();
        assert_eq!(reg.entries().len(), 2);
        assert_eq!(reg.entries()[0].name, "work");
        assert_eq!(reg.entries()[0].root, a);
        assert_eq!(reg.entries()[1].name, "reports-2026", "bare path → basename");
    }

    #[test]
    fn from_flags_rejects_missing_index_and_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();
        let no_index = dir.path().join("plain");
        std::fs::create_dir_all(&no_index).unwrap();
        let err = IndexRegistry::from_flags(&[no_index.display().to_string()]).unwrap_err();
        assert!(format!("{err:#}").contains("no .msrch index"), "{err:#}");

        let a = make_index_root(&dir, "one");
        let b = make_index_root(&dir, "two");
        let err = IndexRegistry::from_flags(&[
            format!("same={}", a.display()),
            format!("same={}", b.display()),
        ])
        .unwrap_err();
        assert!(format!("{err:#}").contains("duplicate index name"), "{err:#}");
    }

    #[test]
    fn discover_walks_up_like_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let root = make_index_root(&dir, "proj");
        let nested = root.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let reg = IndexRegistry::discover(&nested).unwrap();
        assert_eq!(reg.entries().len(), 1);
        assert_eq!(reg.entries()[0].root, root);
        assert_eq!(reg.entries()[0].name, "proj");

        let bare = tempfile::tempdir().unwrap();
        let err = IndexRegistry::discover(bare.path()).unwrap_err();
        assert!(format!("{err:#}").contains("No .msrch index"), "{err:#}");
    }

    #[test]
    fn resolve_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let a = make_index_root(&dir, "alpha");
        let b = make_index_root(&dir, "beta");
        let one = IndexRegistry::from_flags(&[a.display().to_string()]).unwrap();
        assert_eq!(one.resolve(None).unwrap().name, "alpha");
        assert_eq!(one.resolve(Some("alpha")).unwrap().name, "alpha");

        let two = IndexRegistry::from_flags(&[
            a.display().to_string(),
            b.display().to_string(),
        ])
        .unwrap();
        let err = two.resolve(None).unwrap_err();
        assert!(
            format!("{err:#}").contains("alpha") && format!("{err:#}").contains("beta"),
            "ambiguous resolve lists names: {err:#}"
        );
        let err = two.resolve(Some("nope")).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown index 'nope'")
                && format!("{err:#}").contains("alpha"),
            "unknown name lists valid ones: {err:#}"
        );
    }
}
```

- [ ] **Step 3: Implement**

`crates/mcp/src/registry.rs`:

```rust
//! Named index roots, built once at startup. Clients address indexes by
//! name only — the server never resolves client-supplied filesystem paths.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug)]
pub struct IndexRegistry {
    entries: Vec<IndexEntry>,
}

impl IndexRegistry {
    /// Build from `--index` flags: `name=path` (split on the FIRST '=') or a
    /// bare `path` whose name is the directory basename. Every root must
    /// contain `.msrch/`.
    pub fn from_flags(flags: &[String]) -> Result<Self> {
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(flags.len());
        for flag in flags {
            let (name, raw_path) = match flag.split_once('=') {
                Some((name, path)) => (name.to_string(), path.to_string()),
                None => {
                    let path = PathBuf::from(flag);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .with_context(|| format!("cannot derive an index name from '{flag}'"))?;
                    (name, flag.clone())
                }
            };
            let root = std::fs::canonicalize(&raw_path)
                .with_context(|| format!("index root '{raw_path}' does not exist"))?;
            if !root.join(".msrch").is_dir() {
                bail!(
                    "no .msrch index at '{}' — run 'msrch index .' there first",
                    root.display()
                );
            }
            if entries.iter().any(|e| e.name == name) {
                bail!("duplicate index name '{name}'");
            }
            entries.push(IndexEntry { name, root });
        }
        if entries.is_empty() {
            bail!("no indexes registered");
        }
        Ok(Self { entries })
    }

    /// CLI-identical walk-up discovery from `cwd`; single unnamed root whose
    /// name is the root directory's basename.
    pub fn discover(cwd: &Path) -> Result<Self> {
        let root = msrch_core::index::find_index_root(cwd)
            .context("No .msrch index found in directory tree")?;
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string());
        Ok(Self {
            entries: vec![IndexEntry { name, root }],
        })
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// `None` with one entry → that entry; `None` with several → error
    /// listing names; `Some(name)` → match or error listing valid names.
    pub fn resolve(&self, index: Option<&str>) -> Result<&IndexEntry> {
        match index {
            Some(name) => self.entries.iter().find(|e| e.name == name).with_context(|| {
                format!(
                    "unknown index '{name}'; registered indexes: {}",
                    self.names().join(", ")
                )
            }),
            None if self.entries.len() == 1 => Ok(&self.entries[0]),
            None => bail!(
                "multiple indexes registered — pass 'index' with one of: {}",
                self.names().join(", ")
            ),
        }
    }

    fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }
}
```

`crates/mcp/src/server.rs` (compiling stub for this task):

```rust
//! Tool handlers and transport startup. Completed in the rmcp task.

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Http,
}

#[derive(Debug)]
pub struct McpOptions {
    pub transport: TransportKind,
    /// Raw `--index` flag values (parsed by IndexRegistry::from_flags);
    /// empty → walk-up discovery from the current directory.
    pub index_flags: Vec<String>,
    /// http only; None → "127.0.0.1:7920".
    pub bind: Option<String>,
}

pub async fn serve(_options: McpOptions) -> Result<()> {
    anyhow::bail!("serve: implemented in the rmcp task")
}
```

Note on the walk-up test: `discover` must NOT require the `.msrch` of a PARENT of the tempdir to leak in — tempdirs under /tmp have no `.msrch` ancestors. If the bare-dir discovery test flakes because some ancestor has one, create the bare dir under the first tempdir's `plain/` instead and report it.

- [ ] **Step 4: Run tests to verify RED→GREEN**

RED first (functions undefined) via `cargo test -p msrch-mcp 2>&1 | tail -3`, then implement, then:
Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 93 total (89 + 4 registry tests).

- [ ] **Step 5: Clippy + commit**

```bash
git add -A
git commit -m "feat: msrch-mcp crate scaffold — index registry with named roots"
```
(Append the standard Claude Code footer.)

---

### Task 3: rmcp tools + transports

**Files:**
- Rewrite: `crates/mcp/src/server.rs` (tool handlers + serve; keep `McpOptions`/`TransportKind` exactly as Task 2 defined them)

**Interfaces:**
- Consumes: `IndexRegistry` (Task 2), `msrch_core::{search::{Searcher, SearchOptions}, index::{Indexer, get_stats, load_file_mtimes}, config::Config, dates::parse_date_arg}`.
- Produces: working `serve(McpOptions) -> Result<()>`; internally `MsrchServer` whose tool methods are plain async fns (unit-testable without transport).

- [ ] **Step 1: Write the failing handler tests** (transport-free — construct the server, call methods)

Append to `crates/mcp/src/server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::IndexRegistry;
    use std::sync::Arc;

    fn fixture_server(names: &[&str]) -> (tempfile::TempDir, MsrchServer) {
        let dir = tempfile::tempdir().unwrap();
        let mut flags = Vec::new();
        for name in names {
            let root = dir.path().join(name);
            std::fs::create_dir_all(root.join(".msrch")).unwrap();
            std::fs::write(
                root.join(".msrch/manifest.json"),
                r#"{"version":5,"files":{"/a.md":{"modified_at":{"secs_since_epoch":100,"nanos_since_epoch":0},"chunk_ids":[]}}}"#,
            )
            .unwrap();
            flags.push(format!("{name}={}", root.display()));
        }
        let reg = IndexRegistry::from_flags(&flags).unwrap();
        (dir, MsrchServer::new(Arc::new(reg)))
    }

    #[tokio::test]
    async fn list_indexes_reports_names_roots_and_file_counts() {
        let (_dir, server) = fixture_server(&["alpha", "beta"]);
        let value = server.list_indexes_impl().unwrap();
        let list = value.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["name"], "alpha");
        assert_eq!(list[0]["files"], 1);
        assert!(list[0]["root"].as_str().unwrap().ends_with("alpha"));
    }

    #[tokio::test]
    async fn stats_resolves_index_and_errors_helpfully() {
        let (_dir, server) = fixture_server(&["alpha", "beta"]);
        // Ambiguous: two indexes, no name.
        let err = server.stats_impl(None).await.unwrap_err();
        assert!(err.contains("alpha") && err.contains("beta"), "{err}");
        // Unknown name.
        let err = server.stats_impl(Some("nope".into())).await.unwrap_err();
        assert!(err.contains("unknown index 'nope'"), "{err}");
        // Valid: stats over the fixture manifest (no DB → chunk_count 0).
        let value = server.stats_impl(Some("alpha".into())).await.unwrap();
        assert_eq!(value["file_count"], 1);
    }

    #[tokio::test]
    async fn search_validates_args_before_touching_the_network() {
        let (_dir, server) = fixture_server(&["alpha"]);
        let err = server
            .search_impl(SearchArgs {
                query: "q".into(),
                index: None,
                limit: None,
                rerank: None,
                min_similarity: Some(1.5),
                path_contains: None,
                after: None,
                before: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("between 0.0 and 1.0"), "{err}");

        let err = server
            .search_impl(SearchArgs {
                query: "q".into(),
                index: None,
                limit: None,
                rerank: None,
                min_similarity: None,
                path_contains: None,
                after: Some("tomorrow".into()),
                before: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("YYYY-MM-DD"), "date error lists forms: {err}");
    }
}
```

- [ ] **Step 2: RED** — `cargo test -p msrch-mcp 2>&1 | tail -3` → compile errors (MsrchServer etc. undefined).

- [ ] **Step 3: Implement `server.rs`**

Structure (best-knowledge rmcp 2.2.0 — adaptation clause applies to every rmcp/axum name):

```rust
//! Tool handlers and transport startup. Handlers are plain async methods
//! (`*_impl`) returning Result<serde_json::Value, String> so they unit-test
//! without a transport; thin #[tool] wrappers adapt them to rmcp.

use crate::registry::{IndexEntry, IndexRegistry};
use anyhow::{Context, Result};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ErrorData as McpError, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Http,
}

#[derive(Debug)]
pub struct McpOptions {
    pub transport: TransportKind,
    pub index_flags: Vec<String>,
    pub bind: Option<String>,
}

pub const DEFAULT_BIND: &str = "127.0.0.1:7920";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Natural-language search query.
    pub query: String,
    /// Registered index name; omit when only one index is registered.
    pub index: Option<String>,
    /// Max results (default: the index's configured default_limit).
    pub limit: Option<usize>,
    /// Cross-encoder rerank for precision (slower).
    pub rerank: Option<bool>,
    /// Minimum vector similarity, 0.0-1.0.
    pub min_similarity: Option<f32>,
    /// Only files whose path contains this substring.
    pub path_contains: Option<String>,
    /// Only files modified on/after this date (YYYY-MM-DD or 7d/2w/3m).
    pub after: Option<String>,
    /// Only files modified before this date (YYYY-MM-DD or 7d/2w/3m).
    pub before: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StatsArgs {
    /// Registered index name; omit when only one index is registered.
    pub index: Option<String>,
}

#[derive(Clone)]
pub struct MsrchServer {
    registry: Arc<IndexRegistry>,
    tool_router: ToolRouter<Self>,
}
```

Handler implementations (the testable core — errors are `String` so tests read them and the tool wrappers wrap them):

```rust
impl MsrchServer {
    pub fn new(registry: Arc<IndexRegistry>) -> Self {
        Self { registry, tool_router: Self::tool_router() }
    }

    fn resolve(&self, index: Option<&str>) -> Result<&IndexEntry, String> {
        self.registry.resolve(index).map_err(|e| format!("{e:#}"))
    }

    pub(crate) fn list_indexes_impl(&self) -> Result<serde_json::Value, String> {
        let list: Vec<serde_json::Value> = self
            .registry
            .entries()
            .iter()
            .map(|e| {
                let files = msrch_core::index::load_file_mtimes(&e.root)
                    .map(|m| m.len())
                    .unwrap_or(0);
                serde_json::json!({
                    "name": e.name,
                    "root": e.root.display().to_string(),
                    "files": files,
                })
            })
            .collect();
        Ok(serde_json::Value::Array(list))
    }

    pub(crate) async fn stats_impl(&self, index: Option<String>) -> Result<serde_json::Value, String> {
        let entry = self.resolve(index.as_deref())?;
        let stats = msrch_core::index::get_stats(&entry.root)
            .await
            .map_err(|e| format!("{e:#}"))?;
        let last_indexed = stats.last_indexed.map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
        });
        Ok(serde_json::json!({
            "index": entry.name,
            "index_path": stats.index_path.display().to_string(),
            "root_path": stats.root_path.display().to_string(),
            "file_count": stats.file_count,
            "chunk_count": stats.chunk_count,
            "estimated_tokens": stats.estimated_tokens,
            "last_indexed": last_indexed,
            "size_on_disk": stats.size_on_disk,
            "model": stats.model,
            "endpoint": stats.endpoint,
        }))
    }

    pub(crate) async fn search_impl(&self, args: SearchArgs) -> Result<serde_json::Value, String> {
        // Validate before any I/O.
        if let Some(m) = args.min_similarity
            && !(0.0..=1.0).contains(&m)
        {
            return Err(format!("min_similarity {m} is out of range; must be between 0.0 and 1.0"));
        }
        let after = args.after.as_deref().map(msrch_core::dates::parse_date_arg).transpose()?;
        let before = args.before.as_deref().map(msrch_core::dates::parse_date_arg).transpose()?;
        let entry = self.resolve(args.index.as_deref())?;

        // Same quiet, non-fatal, schema-guarded freshness as the CLI.
        let config = msrch_core::config::Config::load_for_index(&entry.root);
        let mut refreshed = 0usize;
        if config.query.auto_index {
            let indexer = msrch_core::index::Indexer::new(entry.root.clone(), config.clone());
            match indexer.index_quiet().await {
                Ok(n) => refreshed = n,
                Err(e) => eprintln!(
                    "warning: auto-index failed for '{}' ({e:#}); searching the existing index",
                    entry.name
                ),
            }
        }

        let searcher = msrch_core::search::Searcher::new(Some(entry.root.clone()))
            .await
            .map_err(|e| format!("{e:#}"))?;
        let opts = msrch_core::search::SearchOptions {
            limit: args.limit,
            use_rerank: args.rerank.unwrap_or(false),
            min_similarity: args.min_similarity,
            path_contains: args.path_contains.clone(),
            after,
            before,
        };
        let results = searcher
            .search(&args.query, &opts)
            .await
            .map_err(|e| format!("{e:#}"))?;

        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| serde_json::json!({
                "file_path": r.file_path,
                "chunk_index": r.chunk_index,
                "similarity": r.score,
                "context": r.context,
                "content": r.content,
            }))
            .collect();
        let mut out = serde_json::json!({
            "index": entry.name,
            "query": args.query,
            "results": json_results,
        });
        if refreshed > 0 {
            out["auto_index_refreshed"] = serde_json::json!(refreshed);
        }
        Ok(out)
    }
}
```

rmcp tool wrappers + handler + serve:

```rust
#[tool_router]
impl MsrchServer {
    #[tool(description = "Semantic search over an indexed directory tree. Returns ranked chunks with file paths. Filters: path substring, modification-date bounds (YYYY-MM-DD or 7d/2w/3m), minimum similarity, optional reranking.")]
    async fn search(&self, Parameters(args): Parameters<SearchArgs>) -> Result<CallToolResult, McpError> {
        match self.search_impl(args).await {
            Ok(v) => Ok(CallToolResult::success(vec![Content::json(v)?])),
            Err(msg) => Err(McpError::invalid_params(msg, None)),
        }
    }

    #[tool(description = "Statistics for a registered index: file/chunk counts, last-indexed time, embedding model and endpoint.")]
    async fn stats(&self, Parameters(args): Parameters<StatsArgs>) -> Result<CallToolResult, McpError> {
        match self.stats_impl(args.index).await {
            Ok(v) => Ok(CallToolResult::success(vec![Content::json(v)?])),
            Err(msg) => Err(McpError::invalid_params(msg, None)),
        }
    }

    #[tool(description = "List the registered indexes: name, root path, and file count. Pass a name as 'index' to other tools when more than one is registered.")]
    async fn list_indexes(&self) -> Result<CallToolResult, McpError> {
        match self.list_indexes_impl() {
            Ok(v) => Ok(CallToolResult::success(vec![Content::json(v)?])),
            Err(msg) => Err(McpError::internal_error(msg, None)),
        }
    }
}

#[tool_handler]
impl ServerHandler for MsrchServer {
    fn get_info(&self) -> ServerInfo {
        let names: Vec<&str> = self.registry.entries().iter().map(|e| e.name.as_str()).collect();
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(format!(
                "msrch semantic search. Registered indexes: {}. Use 'search' for concept queries; 'grep' remains better for exact identifiers.",
                names.join(", ")
            )),
            ..Default::default()
        }
    }
}

pub async fn serve(options: McpOptions) -> Result<()> {
    if options.transport == TransportKind::Stdio && options.bind.is_some() {
        anyhow::bail!("--bind only applies to --transport http");
    }
    let registry = if options.index_flags.is_empty() {
        IndexRegistry::discover(&std::env::current_dir()?)?
    } else {
        IndexRegistry::from_flags(&options.index_flags)?
    };
    let registry = Arc::new(registry);

    match options.transport {
        TransportKind::Stdio => {
            let service = MsrchServer::new(registry).serve(stdio()).await?;
            service.waiting().await?;
        }
        TransportKind::Http => {
            use rmcp::transport::streamable_http_server::{
                StreamableHttpService, session::local::LocalSessionManager,
            };
            let bind = options.bind.as_deref().unwrap_or(DEFAULT_BIND).to_string();
            let service = StreamableHttpService::new(
                move || Ok(MsrchServer::new(registry.clone())),
                LocalSessionManager::default().into(),
                Default::default(),
            );
            let router = axum::Router::new().nest_service("/mcp", service);
            let listener = tokio::net::TcpListener::bind(&bind)
                .await
                .with_context(|| format!("cannot bind {bind}"))?;
            eprintln!("msrch mcp listening on http://{bind}/mcp");
            axum::serve(listener, router).await?;
        }
    }
    Ok(())
}
```

(Every rmcp/axum item above is subject to the Global Constraints adaptation clause — check docs.rs/rmcp/2.2.0 on any compile error and adapt mechanically; report each adaptation. `Content::json` may be `Content::json(value)?` returning Result or an infallible constructor — adapt. The validation `search_impl` performs before I/O is a REQUIREMENT: the two failing-arg tests must pass without an embedding endpoint.)

- [ ] **Step 4: GREEN** — `cargo test --workspace 2>&1 | grep "test result"`
Expected: 96 total (93 + 3 handler tests). Clippy: no new warnings.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: MCP server — search/stats/list_indexes over stdio and streamable HTTP"
```
(Append the standard Claude Code footer.)

---

### Task 4: `msrch mcp` subcommand

**Files:**
- Modify: `crates/cli/Cargo.toml` (+ `msrch-mcp.workspace = true`), `crates/cli/src/main.rs` (Mcp variant + arm + parse tests)

**Interfaces:**
- Consumes: `msrch_mcp::{serve, McpOptions, TransportKind}` (Task 2/3 signatures).

- [ ] **Step 1: Write the failing parse tests**

```rust
#[test]
fn mcp_subcommand_parses_transports_indexes_and_bind() {
    let cli = Cli::try_parse_from(["msrch", "mcp"]).expect("bare mcp parses");
    match cli.command {
        Some(Commands::Mcp { transport, index, bind }) => {
            assert_eq!(transport, McpTransportArg::Stdio);
            assert!(index.is_empty());
            assert!(bind.is_none());
        }
        other => panic!("expected Mcp, got {other:?}"),
    }

    let cli = Cli::try_parse_from([
        "msrch", "mcp", "--transport", "http",
        "--index", "work=/data/reports", "--index", "/code/msrch",
        "--bind", "0.0.0.0:7920",
    ])
    .expect("full http form parses");
    match cli.command {
        Some(Commands::Mcp { transport, index, bind }) => {
            assert_eq!(transport, McpTransportArg::Http);
            assert_eq!(index, vec!["work=/data/reports".to_string(), "/code/msrch".to_string()]);
            assert_eq!(bind.as_deref(), Some("0.0.0.0:7920"));
        }
        other => panic!("expected Mcp, got {other:?}"),
    }
}
```

- [ ] **Step 2: RED**, then implement:

`Commands` enum gains (after `Config`):

```rust
    /// Serve search over the Model Context Protocol (stdio or HTTP)
    Mcp {
        /// Transport: stdio (per-project child process) or http (long-running)
        #[arg(long, value_enum, default_value_t = McpTransportArg::Stdio)]
        transport: McpTransportArg,
        /// Register an index root: 'name=path' or bare 'path' (name = dir
        /// basename). Repeatable. Default: walk-up discovery from cwd.
        #[arg(long)]
        index: Vec<String>,
        /// http only: listen address (default 127.0.0.1:7920)
        #[arg(long)]
        bind: Option<String>,
    },
```

Top-level:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum McpTransportArg {
    Stdio,
    Http,
}
```

Match arm (thin — one call):

```rust
        Commands::Mcp { transport, index, bind } => {
            let options = msrch_mcp::McpOptions {
                transport: match transport {
                    McpTransportArg::Stdio => msrch_mcp::TransportKind::Stdio,
                    McpTransportArg::Http => msrch_mcp::TransportKind::Http,
                },
                index_flags: index.clone(),
                bind: bind.clone(),
            };
            msrch_mcp::serve(options).await.context("MCP server failed")?;
        }
```

`crates/cli/Cargo.toml` `[dependencies]`: add `msrch-mcp.workspace = true` (alphabetical).

- [ ] **Step 3: GREEN + manual startup checks**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 97 total.
Run: `cargo run -q -- mcp --bind 1.2.3.4:1 2>&1 | head -2` — expect the "--bind only applies to --transport http" error (exit non-zero).
Run: `printf '' | cargo run -q -- mcp 2>&1 | head -2` — starts in this repo (walk-up finds .msrch), exits cleanly on closed stdin (or reports how it behaves — capture it).
Clippy: no new warnings.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: msrch mcp subcommand wiring both transports"
```
(Append the standard Claude Code footer.)

---

### Task 5: Docs + release 0.6.0

**Files:**
- Modify: `README.md`, `CLAUDE.md`, `docs/AGENTS-SNIPPET.md`, `CHANGELOG.md`, root `Cargo.toml`

- [ ] **Step 1: Version bump** — 0.5.0 → 0.6.0; `cargo build -q`; commit lock delta with the rest.

- [ ] **Step 2: README — new "MCP server" section** (after "Using msrch from coding agents"; match existing heading style):

````markdown
## MCP server

`msrch mcp` exposes search over the Model Context Protocol — same core, same
results as the CLI.

**Per-project (stdio):** add to the repo's `.mcp.json` (Claude Code) or MCP
client config; the server discovers the index by walking up from its working
directory, exactly like the CLI:

```json
{
  "mcpServers": {
    "msrch": { "command": "msrch", "args": ["mcp"] }
  }
}
```

**Shared server (HTTP):** one long-running process can front several indexes
by name:

```bash
msrch mcp --transport http \
  --index reports=/data/reports \
  --index code=/code/msrch \
  --bind 127.0.0.1:7920      # default; use a tailnet address to share
```

Tools: `search` (full filter set: `path_contains`, `after`/`before`,
`min_similarity`, `rerank`), `stats`, `list_indexes`. When several indexes
are registered, pass `index` by name. Clients never supply filesystem paths.
There is no authentication in v1 — bind to localhost or a trusted network
(e.g. tailnet) only.
````

- [ ] **Step 3: AGENTS-SNIPPET.md** — add after the Notes list:

```markdown
- If your agent supports MCP, `msrch mcp` in this repo's MCP config exposes
  the same search as a `search` tool — otherwise the shell commands above
  are the interface.
```

- [ ] **Step 4: CLAUDE.md** — Essential Commands gains `cargo run -- mcp   # MCP server (stdio; --transport http for shared)` in Build & Run; File Structure tree gains `crates/mcp/` with its three files; Key Modules gains `- **crates/mcp** - MCP front-end: index registry (named roots), search/stats/list_indexes tools, stdio + streamable-HTTP transports (rmcp)`.

- [ ] **Step 5: CHANGELOG** (top, above [0.5.0]):

```markdown
## [0.6.0] - 2026-07-16

### Added
- **MCP server**: `msrch mcp` serves `search` (full filter set), `stats`, and
  `list_indexes` over the Model Context Protocol. stdio transport for
  per-project use (index discovered by walk-up, CLI-style) and streamable
  HTTP (`--transport http`) for a shared server fronting multiple indexes by
  name (`--index name=path`, repeatable). Auto-index freshness applies per
  request where the index's config enables it. Default bind 127.0.0.1:7920;
  no authentication in v1 — localhost/trusted-network only.

No index schema change — existing indexes work as-is.
```

- [ ] **Step 6: Full suite + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 97 green.

```bash
git add -A
git commit -m "chore: release 0.6.0 — MCP server (see CHANGELOG)"
```
(Append the standard Claude Code footer.)

**Post-merge (controller/human):** `git tag v0.6.0 && git push --tags`; `make install`; write `.mcp.json` in a test repo and drive search/stats/list_indexes from Claude Code (stdio acceptance); start the HTTP form with two `--index` roots and verify per-name search plus the helpful ambiguity error (HTTP acceptance).

---

## Self-review notes

- Spec coverage: registry semantics + validation (Task 2), three tools with exact result shapes incl. `auto_index_refreshed` (Task 3), per-request open (Task 3 — Searcher::new per call, no held state), transports + bind rules + no-auth posture (Tasks 3/5), dates relocation with byte-identical CLI (Task 1), subcommand thinness (Task 4), docs/release (Task 5). YAGNI ledger: no auth/similar/reindex/runtime-registration/watch/resources — none appear in any task. ✓
- Type consistency: `McpOptions{transport, index_flags, bind}`, `TransportKind`, `serve`, `SearchArgs` fields, `*_impl -> Result<serde_json::Value, String>` used identically across Tasks 2–4. ✓
- Test-without-network invariant: handler tests only exercise validation/registry/manifest paths; search's arg validation explicitly precedes all I/O. ✓
- rmcp uncertainty is fenced: one adaptation clause in Global Constraints, referenced at every rmcp-touching step, with a BLOCKED escape hatch. ✓
