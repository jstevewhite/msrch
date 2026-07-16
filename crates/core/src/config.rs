use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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
    /// Run the incremental index pass before every query in this index
    /// (quiet; non-fatal). Set per-project for fast-changing document repos.
    pub auto_index: bool,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            min_similarity: 0.5,
            output_format: "context".to_string(),
            auto_index: false,
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

/// Global config path: `$XDG_CONFIG_HOME/msrch/config.toml` when set and
/// non-empty, else `$HOME/.config/msrch/config.toml`. Pure for testability.
fn resolve_global_config_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(xdg) = xdg
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("msrch").join("config.toml"));
    }
    home.map(|h| {
        PathBuf::from(h)
            .join(".config")
            .join("msrch")
            .join("config.toml")
    })
}

/// Where confy (pre-0.5.0) kept the global config on macOS. On Linux the
/// confy path coincides with the new path, so this only ever exists on macOS.
fn legacy_global_config_path(home: Option<&OsStr>) -> Option<PathBuf> {
    home.map(|h| {
        PathBuf::from(h)
            .join("Library/Application Support/rs.msrch")
            .join("config.toml")
    })
}

/// Copy-once migration from the legacy confy location. Returns the path this
/// run should read. Never modifies or removes the legacy file; every failure
/// degrades to reading the legacy path directly.
fn migrate_legacy_config(new_path: &Path, legacy_path: &Path) -> PathBuf {
    if new_path.exists() || !legacy_path.exists() {
        return new_path.to_path_buf();
    }
    if let Some(dir) = new_path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!(
            "warning: could not create {} ({e}); reading legacy config at {}",
            dir.display(),
            legacy_path.display()
        );
        return legacy_path.to_path_buf();
    }
    match std::fs::copy(legacy_path, new_path) {
        Ok(_) => {
            eprintln!(
                "migrated global config to {} (old file left in place at {})",
                new_path.display(),
                legacy_path.display()
            );
            new_path.to_path_buf()
        }
        Err(e) => {
            eprintln!(
                "warning: could not migrate global config to {} ({e}); reading {}",
                new_path.display(),
                legacy_path.display()
            );
            legacy_path.to_path_buf()
        }
    }
}

impl Config {
    /// Load the global config from `~/.config/msrch/config.toml` (XDG-aware),
    /// after a one-time copy migration from the legacy confy location.
    /// A missing file yields defaults; unlike confy, no file is auto-created.
    pub fn load_global_config() -> anyhow::Result<Self> {
        let xdg = std::env::var_os("XDG_CONFIG_HOME");
        let home = std::env::var_os("HOME");
        let new_path = resolve_global_config_path(xdg.as_deref(), home.as_deref())
            .context("cannot determine home directory for global config")?;
        let read_path = match legacy_global_config_path(home.as_deref()) {
            Some(legacy) => migrate_legacy_config(&new_path, &legacy),
            None => new_path,
        };
        Self::load_global_config_from(&read_path)
    }

    /// Read a global config file: absent → defaults; malformed → Err (the
    /// caller `load_global_config_or_default` turns that into warn+defaults).
    fn load_global_config_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read global config at {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse global config at {}", path.display()))
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
                eprintln!("warning: failed to load config, falling back to defaults: {e:#}");
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
    fn auto_index_defaults_false_and_loads_from_toml() {
        assert!(!QueryConfig::default().auto_index);
        let config: Config = toml::from_str("[query]\nauto_index = true\n").unwrap();
        assert!(config.query.auto_index);
    }

    #[test]
    fn resolve_global_config_path_prefers_nonempty_xdg() {
        use std::ffi::OsStr;
        assert_eq!(
            resolve_global_config_path(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/xdg/msrch/config.toml"))
        );
        // Empty XDG_CONFIG_HOME is treated as unset (XDG spec):
        assert_eq!(
            resolve_global_config_path(Some(OsStr::new("")), Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/home/u/.config/msrch/config.toml"))
        );
        assert_eq!(
            resolve_global_config_path(None, Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/home/u/.config/msrch/config.toml"))
        );
        assert_eq!(resolve_global_config_path(None, None), None);
    }

    #[test]
    fn migrate_legacy_config_copies_once_and_prefers_new() {
        let dir = tempfile::tempdir().unwrap();
        let new_path = dir.path().join("new/config.toml");
        let legacy = dir.path().join("legacy/config.toml");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "[query]\ndefault_limit = 3\n").unwrap();

        // Legacy exists, new absent → copy happens, new path returned, legacy intact.
        let read = migrate_legacy_config(&new_path, &legacy);
        assert_eq!(read, new_path);
        assert_eq!(
            std::fs::read_to_string(&new_path).unwrap(),
            "[query]\ndefault_limit = 3\n"
        );
        assert!(legacy.exists(), "legacy file must never be removed");

        // New exists → no re-copy, even when contents differ.
        std::fs::write(&new_path, "[query]\ndefault_limit = 9\n").unwrap();
        let read = migrate_legacy_config(&new_path, &legacy);
        assert_eq!(read, new_path);
        assert_eq!(
            std::fs::read_to_string(&new_path).unwrap(),
            "[query]\ndefault_limit = 9\n",
            "existing new-path config must not be overwritten"
        );

        // Legacy absent → new path returned untouched.
        let lonely = dir.path().join("lonely/config.toml");
        assert_eq!(
            migrate_legacy_config(&lonely, &dir.path().join("nope.toml")),
            lonely
        );
    }

    #[test]
    fn migrate_legacy_config_copy_failure_falls_back_to_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy.toml");
        std::fs::write(&legacy, "[query]\ndefault_limit = 3\n").unwrap();
        // Make the new path's parent an ordinary FILE so create_dir_all fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"file, not dir").unwrap();
        let new_path = blocker.join("config.toml");

        let read = migrate_legacy_config(&new_path, &legacy);
        assert_eq!(
            read, legacy,
            "copy failure must fall back to reading legacy"
        );
        assert!(legacy.exists());
    }

    #[test]
    fn load_global_config_from_reads_missing_as_default_and_file_as_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("nope.toml");
        let config = Config::load_global_config_from(&absent).unwrap();
        assert_eq!(
            config.query.default_limit,
            Config::default().query.default_limit
        );

        let present = dir.path().join("config.toml");
        std::fs::write(&present, "[query]\ndefault_limit = 4\n").unwrap();
        let config = Config::load_global_config_from(&present).unwrap();
        assert_eq!(config.query.default_limit, 4);

        let malformed = dir.path().join("bad.toml");
        std::fs::write(&malformed, "not [valid").unwrap();
        assert!(Config::load_global_config_from(&malformed).is_err());
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
