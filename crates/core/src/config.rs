use serde::{Deserialize, Serialize};
use std::path::Path;

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
    pub api_key: Option<String>,
    pub top_n: usize, // How many candidates to fetch for reranking
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:7995/rerank".to_string(),
            model: "BAAI/bge-reranker-large".to_string(),
            api_key: None,
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
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
