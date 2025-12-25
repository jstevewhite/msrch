use crate::config::Config;

use crate::embedding::EmbeddingClient;
use anyhow::{Context, Result};
use colored::*;
use crate::db::{VectorDB, ScoredPoint};
use std::env;
use std::path::{Path, PathBuf};

pub struct Searcher {
    config: Config,
    index_root: PathBuf,
}

impl Searcher {
    pub async fn new(explicit_index: Option<PathBuf>) -> Result<Self> {
        let index_root = if let Some(path) = explicit_index {
            path
        } else {
            Self::find_index_root()?
        };

        // Load config from index or global? For Search, mixing both is good.
        // For POC, just use global defaults/config.
        let config = Config::load_global_config().unwrap_or_default();

        Ok(Self { config, index_root })
    }

    fn find_index_root() -> Result<PathBuf> {
        let mut current = env::current_dir()?;
        loop {
            let candidate = current.join(".msrch");
            if candidate.exists() && candidate.is_dir() {
                return Ok(current);
            }
            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => anyhow::bail!("No .msrch index found in directory tree"),
            }
        }
    }

    pub async fn search(&self, query_text: &str, limit: Option<usize>) -> Result<()> {
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?; // Use default from config
        
        let msrch_dir = self.index_root.join(".msrch");
        let db = VectorDB::new(msrch_dir.join("index.db")).await?;

        // Loading spinner?
        
        let embedding = embedder.embed(vec![query_text.to_string()]).await?;
        let query_vector = embedding.first().context("No embedding generated")?.clone();

        let limit = limit.unwrap_or(self.config.query.default_limit);
        let min_score = self.config.query.min_similarity;

        let results = db.search(query_vector, limit as u64, min_score).await?;

        if results.is_empty() {
            println!("No results found.");
            return Ok(());
        }

        self.display_results(results);

        Ok(())
    }

    fn display_results(&self, results: Vec<ScoredPoint>) {
        println!("{}", format!("Found {} results:", results.len()).bold());
        for result in results {
            let score = result.score;
            let payload = result.payload;
            
            // Payload is now standard serde_json::Value
            let file_path = payload.get("file_path").and_then(|v| v.as_str()).unwrap_or("unknown");
            let chunk_index = payload.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0);
            let content = payload.get("content").and_then(|v| v.as_str()).unwrap_or("");
            
            println!("\n{} {}:{}", 
                format!("{:.2}", score).yellow(),
                file_path.cyan(),
                chunk_index
            );
            
            // Basic context display (first 2 lines of content)
            for line in content.lines().take(3) {
                println!("  │ {}", line);
            }
        }
    }
}
