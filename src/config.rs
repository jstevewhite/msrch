use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub embedding: EmbeddingConfig,
    pub chunking: ChunkingConfig,
    pub indexing: IndexingConfig,
    pub query: QueryConfig,
    pub display: DisplayConfig,
    pub reranker: RerankerConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            embedding: EmbeddingConfig::default(),
            chunking: ChunkingConfig::default(),
            indexing: IndexingConfig::default(),
            query: QueryConfig::default(),
            display: DisplayConfig::default(),
            reranker: RerankerConfig::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct EmbeddingConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub batch_size: usize,
    pub timeout_seconds: u64,
    pub max_retries: u32,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://r7.home.lab:7997/embeddings".to_string(), // Default as per user request
            model: "mixedbread-ai/mxbai-embed-large-v1".to_string(), // Default as per user request
            api_key: None,
            batch_size: 32,
            timeout_seconds: 30,
            max_retries: 3,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct ChunkingConfig {
    pub max_chunk_tokens: usize,
    pub overlap_tokens: usize,
    pub max_file_size_mb: u64,
    pub use_treesitter: bool,
    pub treesitter_languages: Vec<String>,
    pub fallback_to_tokens: bool,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            max_chunk_tokens: 512,
            overlap_tokens: 50,
            max_file_size_mb: 10,
            use_treesitter: true,
            treesitter_languages: vec![
                "rust".to_string(),
                "python".to_string(),
                "javascript".to_string(),
                "typescript".to_string(),
                "go".to_string(),
            ],
            fallback_to_tokens: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct IndexingConfig {
    pub skip_binary: bool,
    pub follow_symlinks: bool,
    pub ignore_patterns: Vec<String>,
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            skip_binary: true,
            follow_symlinks: false,
            ignore_patterns: vec![
                ".git/".to_string(),
                ".msrch/".to_string(),
                "node_modules/".to_string(),
                "target/".to_string(),
                "__pycache__/".to_string(),
                "*.pyc".to_string(),
                ".venv/".to_string(),
                "venv/".to_string(),
                ".DS_Store".to_string(),
            ],
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct QueryConfig {
    pub default_limit: usize,
    pub min_similarity: f32,
    pub output_format: String,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            min_similarity: 0.5,
            output_format: "context".to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct DisplayConfig {
    pub show_similarity_scores: bool,
    pub color_output: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            show_similarity_scores: true,
            color_output: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct RerankerConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub top_n: usize, // How many candidates to fetch for reranking
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:7995/rerank".to_string(),
            model: "BAAI/bge-reranker-large".to_string(),
            top_n: 50, // Fetch 50, rerank to top 10
        }
    }
}

impl Config {
    pub fn load_global_config() -> Result<Self, confy::ConfyError> {
        // This will load from OS specific config dir, e.g. ~/.config/msrch/msrch.toml or similar
        // We use "msrch" as app name, and "config" as the file name if we can control it, or just "msrch"
        confy::load("msrch", "config")
    }

    /// Load the global config, falling back to defaults with a warning if the file
    /// exists but can't be parsed. Prefer this over `.unwrap_or_default()` so an
    /// outdated or malformed config surfaces a message instead of silently
    /// masquerading as all-defaults. With `#[serde(default)]` on the config structs,
    /// a config that is merely missing newer fields still loads cleanly.
    pub fn load_global_config_or_default() -> Self {
        match Self::load_global_config() {
            Ok(config) => config,
            Err(e) => {
                eprintln!("warning: failed to load config, falling back to defaults: {e}");
                Self::default()
            }
        }
    }

    pub fn load_from_path(path: PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    // Example helper to merge/override configs could go here
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_tolerates_missing_fields_and_sections() {
        // A config written before newer ChunkingConfig fields existed, and with
        // the [embedding] section omitted entirely. This used to fail the whole
        // parse and silently fall back to all-defaults.
        let toml = r#"
[chunking]
max_chunk_tokens = 512
overlap_tokens = 50
max_file_size_mb = 10

[reranker]
enabled = true
endpoint = "http://example.test:7995/rerank"
top_n = 10
"#;

        let config: Config =
            toml::from_str(toml).expect("missing fields/sections should default, not fail");

        // Missing fields within a present section fall back to their defaults:
        assert!(config.chunking.use_treesitter);
        assert_eq!(config.chunking.treesitter_languages.len(), 5);
        assert!(config.chunking.fallback_to_tokens);

        // Values that ARE present are preserved (not clobbered by defaults):
        assert!(config.reranker.enabled);
        assert_eq!(config.reranker.endpoint, "http://example.test:7995/rerank");
        assert_eq!(config.reranker.top_n, 10);

        // A field missing from a present section defaults:
        assert_eq!(config.reranker.model, RerankerConfig::default().model);

        // An entirely-missing section defaults:
        assert_eq!(config.embedding.model, EmbeddingConfig::default().model);
    }
}
