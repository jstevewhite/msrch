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
