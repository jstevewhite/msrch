use crate::OutputFormat;
use crate::config::Config;
use crate::db::{ScoredPoint, VectorDB};
use crate::embedding::EmbeddingClient;
use crate::reranker::RerankerClient;
use anyhow::{Context, Result};
use colored::*;
use log::debug;
use serde::Serialize;
use std::collections::HashSet;
use std::env;
use std::path::PathBuf;

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

        // Load config from index or global? For Search, mixing both is good.
        // For POC, just use global defaults/config.
        let config = Config::load_global_config_or_default();

        Ok(Self { config, index_root })
    }

    pub async fn search(
        &self,
        query_text: &str,
        limit: Option<usize>,
        format: OutputFormat,
        use_rerank: bool,
    ) -> Result<()> {
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;

        // Create reranker config, overriding enabled flag if --rerank passed
        let mut reranker_config = self.config.reranker.clone();
        if use_rerank {
            reranker_config.enabled = true;
        }
        let reranker = RerankerClient::new(reranker_config)?;

        let msrch_dir = self.index_root.join(".msrch");
        let db = VectorDB::new(msrch_dir.join("index.db")).await?;

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
                    eprintln!("Reranking failed, using vector scores: {}", e);
                    // Fall back to vector search results
                    results.truncate(limit);
                }
            }
        }

        if results.is_empty() {
            match format {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::json!({
                        "query": query_text,
                        "index_path": msrch_dir.display().to_string(),
                        "results": []
                    })
                ),
                _ => println!("No results found."),
            }
            return Ok(());
        }

        match format {
            OutputFormat::Plain => self.display_plain(&results),
            OutputFormat::Context => self.display_context(&results),
            OutputFormat::Json => self.display_json(query_text, &msrch_dir, &results),
            OutputFormat::Filename => self.display_filename(&results),
        }

        Ok(())
    }

    fn display_plain(&self, results: &[ScoredPoint]) {
        for result in results {
            let payload = &result.payload;
            let file_path = payload
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let chunk_index = payload
                .get("chunk_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!("{}:{}", file_path, chunk_index);
        }
    }

    fn display_context(&self, results: &[ScoredPoint]) {
        println!("{}", format!("Found {} results:", results.len()).bold());
        for result in results {
            let score = result.score;
            let payload = &result.payload;

            let file_path = payload
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let chunk_index = payload
                .get("chunk_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let content = payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let context = payload
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let context_suffix = if context.is_empty() {
                String::new()
            } else {
                format!("  {}", context.dimmed())
            };

            println!(
                "\n{} {}:{}{}",
                format!("{:.2}", score).yellow(),
                file_path.cyan(),
                chunk_index,
                context_suffix
            );

            for line in content.lines().take(3) {
                println!("  │ {}", line);
            }
        }
    }

    fn display_json(&self, query: &str, index_path: &PathBuf, results: &[ScoredPoint]) {
        let json_results: Vec<JsonResult> = results
            .iter()
            .map(|r| {
                let payload = &r.payload;
                JsonResult {
                    file_path: payload
                        .get("file_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    chunk_index: payload
                        .get("chunk_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    similarity: r.score,
                    context: payload
                        .get("context")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    content: payload
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                }
            })
            .collect();

        let output = JsonOutput {
            query: query.to_string(),
            index_path: index_path.display().to_string(),
            results: json_results,
        };

        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    }

    fn display_filename(&self, results: &[ScoredPoint]) {
        for file_path in unique_file_paths(results) {
            println!("{}", file_path);
        }
    }
}

/// Collect the distinct `file_path` values from results, preserving the order
/// in which each path is first seen (so the most relevant file leads).
fn unique_file_paths(results: &[ScoredPoint]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for result in results {
        let file_path = result
            .payload
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        if seen.insert(file_path.clone()) {
            paths.push(file_path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn point(file_path: &str) -> ScoredPoint {
        ScoredPoint {
            id: "id".to_string(),
            score: 1.0,
            payload: json!({ "file_path": file_path }),
        }
    }

    #[test]
    fn unique_file_paths_dedupes_preserving_first_seen_order() {
        let results = vec![
            point("src/a.rs"),
            point("src/b.rs"),
            point("src/a.rs"),
            point("src/c.rs"),
            point("src/b.rs"),
        ];
        assert_eq!(
            unique_file_paths(&results),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
    }
}
