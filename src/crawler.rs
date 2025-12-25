use crate::config::IndexingConfig;
use anyhow::Result;
use ignore::WalkBuilder;
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
        
        let walker = builder.build();

        for result in walker {
            match result {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_file() {
                        if !self.should_ignore_custom(path) && !self.is_binary(path)? {
                             // Can also check file size here if needed, but config check is better done deeply
                             // For now we just check existence and binary
                             files.push(path.to_path_buf());
                        }
                    }
                }
                Err(err) => eprintln!("Error walking directory: {}", err),
            }
        }

        Ok(files)
    }

    fn should_ignore_custom(&self, path: &Path) -> bool {
         // This is a simple check against simple string patterns in config
         // For more complex globbing we'd need to compile the patterns.
         // For this POC, we rely mostly on ignore crate's handling of .gitignore
         // and this simplistic check
         let path_str = path.to_string_lossy();
         for pattern in &self.config.ignore_patterns {
             // Very basic substring/glob check (incomplete but sufficient for simple excludes)
             // Ideally we use `globset` or rely entirely on ignore crate overrides
             // For now, let's just check if any part of the path matches a forbidden dir
             if path_str.contains(pattern.trim_end_matches('/')) {
                 return true;
             }
         }
         false
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
