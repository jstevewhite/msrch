use crate::config::Config;
use crate::db::{ScoredPoint, VectorDB};
use crate::embedding::EmbeddingClient;
use crate::reranker::RerankerClient;
use anyhow::{Context, Result};
use log::debug;
use serde::Serialize;
use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// True when `mtime` satisfies the (optional) bounds: `after` is inclusive,
/// `before` is exclusive — matching `--after`/`--before` CLI semantics.
fn passes_date_filter(
    mtime: SystemTime,
    after: Option<SystemTime>,
    before: Option<SystemTime>,
) -> bool {
    if let Some(a) = after
        && mtime < a
    {
        return false;
    }
    if let Some(b) = before
        && mtime >= b
    {
        return false;
    }
    true
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

        // Global config overlaid with this index's .msrch/config.toml.
        let config = Config::load_for_index(&index_root);

        Ok(Self { config, index_root })
    }

    /// The `.msrch` directory this searcher operates on.
    pub fn msrch_dir(&self) -> PathBuf {
        self.index_root.join(".msrch")
    }

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

        let date_filtering = opts.after.is_some() || opts.before.is_some();

        // Date filters post-filter the hit list, so over-fetch to avoid
        // starving `limit`. Path-only filtering is exact (DB-side predicate).
        let base_fetch = if date_filtering {
            limit.saturating_mul(10).max(100)
        } else {
            limit
        };
        let fetch_limit = if reranker.is_enabled() {
            base_fetch.max(reranker.top_n())
        } else {
            base_fetch
        };

        let mut results = db
            .search(
                query_vector,
                fetch_limit as u64,
                min_score,
                predicate.as_deref(),
            )
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
    }
}

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
        let results = db.search(query_vector, 20, 0.0, None).await?;

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
}
