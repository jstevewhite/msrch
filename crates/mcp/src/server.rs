//! Tool handlers and transport startup. Handlers are plain async methods
//! (`*_impl`) returning Result<serde_json::Value, ToolError> so they
//! unit-test without a transport; thin #[tool] wrappers adapt them to rmcp,
//! mapping ToolError to the right MCP error code.

use crate::registry::{IndexEntry, IndexRegistry};
use anyhow::{Context, Result};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ErrorData as McpError, ServerCapabilities, ServerInfo},
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
    /// Raw `--index` flag values (parsed by IndexRegistry::from_flags);
    /// empty → walk-up discovery from the current directory.
    pub index_flags: Vec<String>,
    /// http only; None → "127.0.0.1:7920".
    pub bind: Option<String>,
}

pub const DEFAULT_BIND: &str = "127.0.0.1:7920";

/// Tool-handler error, classified so the MCP layer reports the right code:
/// InvalidParams = the client's arguments are wrong (don't retry as-is);
/// Internal = the server/backends failed (arguments were fine).
#[derive(Debug)]
pub(crate) enum ToolError {
    InvalidParams(String),
    Internal(String),
}

impl ToolError {
    /// Only used by tests (production call sites match on the variant
    /// directly via `From<ToolError> for McpError`), but kept as the
    /// documented accessor for inspecting the underlying message.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn message(&self) -> &str {
        match self {
            ToolError::InvalidParams(m) | ToolError::Internal(m) => m,
        }
    }
}

impl From<ToolError> for McpError {
    fn from(e: ToolError) -> Self {
        match e {
            ToolError::InvalidParams(msg) => McpError::invalid_params(msg, None),
            ToolError::Internal(msg) => McpError::internal_error(msg, None),
        }
    }
}

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

/// State shared by every MsrchServer instance (HTTP sessions each build
/// their own server; the registry and per-root auto-index locks must be
/// process-wide).
pub(crate) struct SharedState {
    pub(crate) registry: IndexRegistry,
    /// One lock per registered root, keyed by index name. Guards the
    /// auto-index write path only; searches never block on it.
    pub(crate) auto_index_locks: std::collections::HashMap<String, tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
pub struct MsrchServer {
    state: Arc<SharedState>,
    tool_router: ToolRouter<Self>,
}

impl MsrchServer {
    pub(crate) fn new(state: Arc<SharedState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    fn resolve(&self, index: Option<&str>) -> Result<&IndexEntry, String> {
        self.state
            .registry
            .resolve(index)
            .map_err(|e| format!("{e:#}"))
    }

    pub(crate) fn list_indexes_impl(&self) -> Result<serde_json::Value, ToolError> {
        let list: Vec<serde_json::Value> = self
            .state
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

    pub(crate) async fn stats_impl(
        &self,
        index: Option<String>,
    ) -> Result<serde_json::Value, ToolError> {
        let entry = self
            .resolve(index.as_deref())
            .map_err(ToolError::InvalidParams)?;
        if let Some(v) = msrch_core::index::manifest_schema_version(&entry.root)
            && v != msrch_core::index::SCHEMA_VERSION
        {
            return Err(ToolError::Internal(format!(
                "index '{}' has schema v{v} but this msrch expects v{}; run 'msrch index .' in {} to migrate",
                entry.name,
                msrch_core::index::SCHEMA_VERSION,
                entry.root.display()
            )));
        }
        let stats = msrch_core::index::get_stats(&entry.root)
            .await
            .map_err(|e| ToolError::Internal(format!("{e:#}")))?;
        let last_indexed = stats
            .last_indexed
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
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

    pub(crate) async fn search_impl(
        &self,
        args: SearchArgs,
    ) -> Result<serde_json::Value, ToolError> {
        // Validate before any I/O.
        if let Some(m) = args.min_similarity
            && !(0.0..=1.0).contains(&m)
        {
            return Err(ToolError::InvalidParams(format!(
                "min_similarity {m} is out of range; must be between 0.0 and 1.0"
            )));
        }
        let after = args
            .after
            .as_deref()
            .map(msrch_core::dates::parse_date_arg)
            .transpose()
            .map_err(ToolError::InvalidParams)?;
        let before = args
            .before
            .as_deref()
            .map(msrch_core::dates::parse_date_arg)
            .transpose()
            .map_err(ToolError::InvalidParams)?;
        let entry = self
            .resolve(args.index.as_deref())
            .map_err(ToolError::InvalidParams)?;
        if let Some(v) = msrch_core::index::manifest_schema_version(&entry.root)
            && v != msrch_core::index::SCHEMA_VERSION
        {
            return Err(ToolError::Internal(format!(
                "index '{}' has schema v{v} but this msrch expects v{}; run 'msrch index .' in {} to migrate",
                entry.name,
                msrch_core::index::SCHEMA_VERSION,
                entry.root.display()
            )));
        }

        // Same quiet, non-fatal, schema-guarded freshness as the CLI.
        let config = msrch_core::config::Config::load_for_index(&entry.root);
        let mut refreshed = 0usize;
        // Locks are built from the registry, so `get` always finds an entry
        // for a resolved index; if `try_lock` fails, another request is
        // already refreshing this root — the quiet/non-fatal contract says
        // search what exists now rather than wait.
        if config.query.auto_index
            && let Some(lock) = self.state.auto_index_locks.get(&entry.name)
            && let Ok(_guard) = lock.try_lock()
        {
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
            .map_err(|e| ToolError::Internal(format!("{e:#}")))?;
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
            .map_err(|e| ToolError::Internal(format!("{e:#}")))?;

        let json_results: Vec<serde_json::Value> = results
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

#[tool_router]
impl MsrchServer {
    #[tool(
        description = "Semantic search over an indexed directory tree. Returns ranked chunks with file paths. Filters: path substring, modification-date bounds (YYYY-MM-DD or 7d/2w/3m), minimum similarity, optional reranking."
    )]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.search_impl(args).await {
            Ok(v) => Ok(CallToolResult::success(vec![ContentBlock::json(v)?])),
            Err(e) => Err(e.into()),
        }
    }

    #[tool(
        description = "Statistics for a registered index: file/chunk counts, last-indexed time, embedding model and endpoint."
    )]
    async fn stats(
        &self,
        Parameters(args): Parameters<StatsArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.stats_impl(args.index).await {
            Ok(v) => Ok(CallToolResult::success(vec![ContentBlock::json(v)?])),
            Err(e) => Err(e.into()),
        }
    }

    #[tool(
        description = "List the registered indexes: name, root path, and file count. Pass a name as 'index' to other tools when more than one is registered."
    )]
    async fn list_indexes(&self) -> Result<CallToolResult, McpError> {
        match self.list_indexes_impl() {
            Ok(v) => Ok(CallToolResult::success(vec![ContentBlock::json(v)?])),
            Err(e) => Err(e.into()),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MsrchServer {
    fn get_info(&self) -> ServerInfo {
        let names: Vec<&str> = self
            .state
            .registry
            .entries()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            format!(
                "msrch semantic search. Registered indexes: {}. Use 'search' for concept queries; 'grep' remains better for exact identifiers.",
                names.join(", ")
            ),
        )
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
    let auto_index_locks = registry
        .entries()
        .iter()
        .map(|e| (e.name.clone(), tokio::sync::Mutex::new(())))
        .collect();
    let state = Arc::new(SharedState {
        registry,
        auto_index_locks,
    });

    match options.transport {
        TransportKind::Stdio => {
            let service = MsrchServer::new(state).serve(stdio()).await?;
            service.waiting().await?;
        }
        TransportKind::Http => {
            use rmcp::transport::streamable_http_server::{
                StreamableHttpService, session::local::LocalSessionManager,
            };
            let bind = options.bind.as_deref().unwrap_or(DEFAULT_BIND).to_string();
            let service = StreamableHttpService::new(
                move || Ok(MsrchServer::new(state.clone())),
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
        let auto_index_locks = reg
            .entries()
            .iter()
            .map(|e| (e.name.clone(), tokio::sync::Mutex::new(())))
            .collect();
        let state = Arc::new(SharedState {
            registry: reg,
            auto_index_locks,
        });
        (dir, MsrchServer::new(state))
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
        assert!(
            matches!(err, ToolError::InvalidParams(_)),
            "ambiguous index name must be InvalidParams, got {err:?}"
        );
        assert!(
            err.message().contains("alpha") && err.message().contains("beta"),
            "{err:?}"
        );
        // Unknown name.
        let err = server.stats_impl(Some("nope".into())).await.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidParams(_)),
            "unknown index name must be InvalidParams, got {err:?}"
        );
        assert!(err.message().contains("unknown index 'nope'"), "{err:?}");
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
        assert!(
            matches!(err, ToolError::InvalidParams(_)),
            "out-of-range min_similarity must be InvalidParams, got {err:?}"
        );
        assert!(err.message().contains("between 0.0 and 1.0"), "{err:?}");

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
        assert!(
            matches!(err, ToolError::InvalidParams(_)),
            "unparseable date must be InvalidParams, got {err:?}"
        );
        assert!(
            err.message().contains("YYYY-MM-DD"),
            "date error lists forms: {err:?}"
        );
    }

    #[tokio::test]
    async fn search_backend_failure_is_internal_not_invalid_params() {
        let (dir, server) = fixture_server(&["alpha"]);
        // Point the index's own config at a dead endpoint (immediate refusal).
        std::fs::write(
            dir.path().join("alpha/.msrch/config.toml"),
            "[embedding]\nendpoint = \"http://127.0.0.1:1/embeddings\"\n",
        )
        .unwrap();
        let err = server
            .search_impl(SearchArgs {
                query: "q".into(),
                index: None,
                limit: None,
                rerank: None,
                min_similarity: None,
                path_contains: None,
                after: None,
                before: None,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Internal(_)),
            "backend outage must be Internal, got {err:?}"
        );
    }

    #[tokio::test]
    async fn search_rejects_stale_schema_before_any_io() {
        let (dir, server) = fixture_server(&["alpha"]);
        // Downgrade the manifest below SCHEMA_VERSION and point the endpoint
        // at a dead address — the schema check must fire before any network
        // I/O, so a stale manifest never gets this far.
        std::fs::write(
            dir.path().join("alpha/.msrch/manifest.json"),
            r#"{"version":4,"files":{"/a.md":{"modified_at":{"secs_since_epoch":100,"nanos_since_epoch":0},"chunk_ids":[]}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("alpha/.msrch/config.toml"),
            "[embedding]\nendpoint = \"http://127.0.0.1:1/embeddings\"\n",
        )
        .unwrap();
        let err = server
            .search_impl(SearchArgs {
                query: "q".into(),
                index: None,
                limit: None,
                rerank: None,
                min_similarity: None,
                path_contains: None,
                after: None,
                before: None,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Internal(_)),
            "stale schema is a server-side migration need, not a client fault: got {err:?}"
        );
        assert!(err.message().contains("run 'msrch index .'"), "{err:?}");
    }

    #[tokio::test]
    async fn stats_rejects_stale_schema() {
        let (dir, server) = fixture_server(&["alpha"]);
        std::fs::write(
            dir.path().join("alpha/.msrch/manifest.json"),
            r#"{"version":4,"files":{"/a.md":{"modified_at":{"secs_since_epoch":100,"nanos_since_epoch":0},"chunk_ids":[]}}}"#,
        )
        .unwrap();
        let err = server.stats_impl(Some("alpha".into())).await.unwrap_err();
        assert!(
            matches!(err, ToolError::Internal(_)),
            "stale schema is a server-side migration need, not a client fault: got {err:?}"
        );
        assert!(err.message().contains("run 'msrch index .'"), "{err:?}");
    }

    #[tokio::test]
    async fn auto_index_lock_skips_when_held() {
        let (dir, server) = fixture_server(&["alpha"]);
        // Enable auto-index and point the root's own config at a dead
        // endpoint, matching the existing backend-failure test's pattern.
        std::fs::write(
            dir.path().join("alpha/.msrch/config.toml"),
            "[query]\nauto_index = true\n[embedding]\nendpoint = \"http://127.0.0.1:1/embeddings\"\n",
        )
        .unwrap();
        let manifest_path = dir.path().join("alpha/.msrch/manifest.json");
        let mtime_before = std::fs::metadata(&manifest_path)
            .unwrap()
            .modified()
            .unwrap();

        // Simulate a concurrent request already refreshing this root.
        let _held = server
            .state
            .auto_index_locks
            .get("alpha")
            .unwrap()
            .try_lock()
            .unwrap();

        let err = server
            .search_impl(SearchArgs {
                query: "q".into(),
                index: None,
                limit: None,
                rerank: None,
                min_similarity: None,
                path_contains: None,
                after: None,
                before: None,
            })
            .await
            .unwrap_err();
        // Auto-index is skipped (lock held), so search falls through to the
        // query embedding call, which hits the same dead endpoint.
        assert!(
            matches!(err, ToolError::Internal(_)),
            "with the lock held, search should still reach the dead embedding backend: got {err:?}"
        );
        let mtime_after = std::fs::metadata(&manifest_path)
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "auto-index must not have touched the manifest while the lock was held"
        );
    }

    #[test]
    fn shared_state_is_shared_across_server_clones() {
        let (_dir, server) = fixture_server(&["alpha"]);
        let state = server.state.clone();
        let a = MsrchServer::new(state.clone());
        let b = MsrchServer::new(state.clone());
        assert!(Arc::ptr_eq(&a.state, &b.state));
    }
}
