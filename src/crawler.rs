use crate::config::IndexingConfig;
use anyhow::Result;
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct Crawler {
    config: IndexingConfig,
}

impl Crawler {
    pub fn new(config: IndexingConfig) -> Self {
        Self { config }
    }

    pub fn crawl(&self, root_path: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let mut builder = WalkBuilder::new(root_path);

        // Configure ignore patterns from config logic is a bit complex with WalkBuilder
        // WalkBuilder automatically handles .gitignore.
        // We can add extra overrides.

        // Handling .msrchignore would typically require a custom override builder
        builder.add_custom_ignore_filename(".msrchignore");

        // Add defaults from config if needed, though WalkBuilder handles hidden files by default
        // We can explicitly add overrides if we want to force ignore specific common patterns
        // passed in config that aren't in .gitignore
        let mut overrides = OverrideBuilder::new(root_path);
        for pattern in &self.config.ignore_patterns {
            let pattern_str = format!("!{}", pattern);
            if let Err(e) = overrides.add(&pattern_str) {
                eprintln!("Failed to add ignore pattern '{}': {}", pattern, e);
            }
        }
        if let Ok(override_set) = overrides.build() {
            builder.overrides(override_set);
        }

        let walker = builder.build();

        for result in walker {
            match result {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_file() {
                        match self.is_binary(path) {
                            Ok(true) => {
                                // Skip binary files when configured.
                            }
                            Ok(false) => {
                                // Can also check file size here if needed
                                files.push(path.to_path_buf());
                            }
                            Err(e) => {
                                // Unreadable files are still returned so the indexer can
                                // retain a prior manifest entry instead of treating them
                                // as deleted (which would wipe their vectors).
                                eprintln!(
                                    "Warning: could not inspect {}: {} (including path for indexer)",
                                    path.display(),
                                    e
                                );
                                files.push(path.to_path_buf());
                            }
                        }
                    }
                }
                Err(err) => eprintln!("Error walking directory: {}", err),
            }
        }

        Ok(files)
    }

    fn is_binary(&self, path: &Path) -> Result<bool> {
        if !self.config.skip_binary {
            return Ok(false);
        }

        let mut file = File::open(path)?;
        let mut buffer = [0; 1024]; // Check first 1KB
        let n = file.read(&mut buffer)?;

        // Check for null byte
        if buffer[..n].contains(&0) {
            return Ok(true);
        }

        Ok(false)
    }
}
