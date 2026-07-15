use crate::chunker::Chunker;
use crate::config::Config;
use crate::crawler::Crawler;
use crate::db::VectorDB;
use crate::embedding::EmbeddingClient;
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::time::SystemTime;

/// Bump this whenever the on-disk vector schema OR chunk content/embedding
/// semantics change (e.g. adding a column, or fixing how chunks are built).
/// Indexes written with an older version are wiped and rebuilt on the next index run.
///
/// v1: added the `context` column.
/// v2: fixed Rust doc-comment over-collection (changes stored content/embeddings).
/// v3: resolve type/impl/Go-type names in the context path (was "anonymous").
const SCHEMA_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    /// Schema version of the index this manifest describes. Missing in manifests
    /// written before versioning existed, which deserialize to 0 and trigger a rebuild.
    #[serde(default)]
    version: u32,
    files: HashMap<PathBuf, FileMetadata>,
}

#[derive(Serialize, Deserialize, Clone)]
struct FileMetadata {
    modified_at: SystemTime,
    chunk_ids: Vec<uuid::Uuid>,
}

pub struct IndexStats {
    pub index_path: PathBuf,
    pub root_path: PathBuf,
    pub file_count: usize,
    pub chunk_count: usize,
    pub estimated_tokens: usize,
    pub last_indexed: Option<SystemTime>,
    pub size_on_disk: u64,
    pub model: String,
    pub endpoint: String,
}

pub fn find_index_root(start_path: &Path) -> Option<PathBuf> {
    let mut current = start_path.to_path_buf();
    loop {
        let candidate = current.join(".msrch");
        if candidate.exists() && candidate.is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

pub async fn get_stats(start_path: &Path) -> Result<IndexStats> {
    let index_root = find_index_root(start_path)
        .ok_or_else(|| anyhow::anyhow!("No .msrch index found in directory tree"))?;

    let msrch_dir = index_root.join(".msrch");
    let manifest_path = msrch_dir.join("manifest.json");
    let db_path = msrch_dir.join("index.db");

    // Load manifest
    let manifest: Manifest = if manifest_path.exists() {
        let file = fs::File::open(&manifest_path)?;
        serde_json::from_reader(file).unwrap_or_default()
    } else {
        Manifest::default()
    };

    // Get chunk count from DB
    let chunk_count = if db_path.exists() {
        let db = VectorDB::new(db_path.clone()).await?;
        db.count().await.unwrap_or(0)
    } else {
        0
    };

    // Get last modified time
    let last_indexed = manifest.files.values().map(|m| m.modified_at).max();

    // Calculate index size
    fn dir_size(path: &Path) -> u64 {
        let mut size = 0;
        if path.is_dir() {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        size += dir_size(&path);
                    } else {
                        size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
        } else {
            size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        }
        size
    }
    let size_on_disk = dir_size(&msrch_dir);

    // Load config for model info
    let config = Config::load_global_config_or_default();

    Ok(IndexStats {
        index_path: msrch_dir,
        root_path: index_root,
        file_count: manifest.files.len(),
        chunk_count,
        estimated_tokens: chunk_count * 256, // Rough estimate
        last_indexed,
        size_on_disk,
        model: config.embedding.model,
        endpoint: config.embedding.endpoint,
    })
}

pub struct Indexer {
    config: Config,
    root_path: PathBuf,
}

impl Indexer {
    pub fn new(root_path: PathBuf, config: Config) -> Self {
        Self { root_path, config }
    }

    /// Wipe the on-disk index if its schema version doesn't match the current one,
    /// resetting the manifest so the next run does a full rebuild.
    ///
    /// Returns `Ok(true)` if an existing index was removed (a migration happened).
    /// Must run before connecting to the vector DB so the fresh table is created
    /// with the current schema.
    fn migrate_if_needed(db_path: &Path, manifest: &mut Manifest) -> Result<bool> {
        if manifest.version == SCHEMA_VERSION {
            return Ok(false);
        }

        let had_existing = db_path.exists();
        if had_existing {
            fs::remove_dir_all(db_path)
                .context("Failed to remove outdated index during schema migration")?;
        }

        *manifest = Manifest::default();
        manifest.version = SCHEMA_VERSION;
        Ok(had_existing)
    }

    pub async fn index(&self) -> Result<()> {
        let msrch_dir = self.root_path.join(".msrch");
        fs::create_dir_all(&msrch_dir).context("Failed to create .msrch dir")?;

        let crawler = Crawler::new(self.config.indexing.clone()); // TODO: pass actual config
        let mut chunker = Chunker::new(self.config.chunking.clone());
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;
        println!(
            "Using embedding endpoint: {}",
            self.config.embedding.endpoint
        );

        let manifest_path = msrch_dir.join("manifest.json");
        let db_path = msrch_dir.join("index.db");
        let mut manifest: Manifest = if manifest_path.exists() {
            let file = fs::File::open(&manifest_path)?;
            serde_json::from_reader(file).unwrap_or_default()
        } else {
            Manifest::default()
        };

        // Rebuild from scratch if the on-disk schema predates the current version.
        if Self::migrate_if_needed(&db_path, &mut manifest)? {
            println!(
                "Index schema is outdated; rebuilding from scratch (schema v{}).",
                SCHEMA_VERSION
            );
        }

        // Connect after any migration so the table is created with the current schema.
        let db = VectorDB::new(db_path).await?;
        // Collection will be initialized on first embedding (to detect dimension)

        println!("Scanning files...");
        let files = crawler.crawl(&self.root_path)?;
        println!("Found {} files.", files.len());

        let pb = ProgressBar::new(files.len() as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}")
            .unwrap()
            .progress_chars("#>-"));

        let mut chunks_to_embed = Vec::new();
        let mut new_manifest_entries = HashMap::new();

        for file_path in files {
            pb.set_message(format!(
                "Processing {:?}",
                file_path.file_name().unwrap_or_default()
            ));

            let metadata = fs::metadata(&file_path)?;
            let modified = metadata.modified()?;

            // Check if needs reindexing
            if let Some(existing_meta) = manifest.files.get(&file_path) {
                if existing_meta.modified_at == modified {
                    new_manifest_entries.insert(file_path.clone(), existing_meta.clone());
                    pb.inc(1);
                    continue;
                }
                // Delete old chunks from DB before reindexing
                if !existing_meta.chunk_ids.is_empty() {
                    debug!(
                        "Deleting {} stale chunks for modified file: {:?}",
                        existing_meta.chunk_ids.len(),
                        file_path
                    );
                    if let Err(e) = db.delete_by_ids(&existing_meta.chunk_ids).await {
                        warn!("Failed to delete stale chunks for {:?}: {}", file_path, e);
                    }
                }
            }

            let content = match fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(_) => {
                    pb.inc(1);
                    continue; // Skip non-utf8 for now
                }
            };

            let file_chunks = chunker.chunk_file(&file_path, &content)?;
            let chunk_ids: Vec<uuid::Uuid> = file_chunks.iter().map(|c| c.id).collect();

            new_manifest_entries.insert(
                file_path.clone(),
                FileMetadata {
                    modified_at: modified,
                    chunk_ids,
                },
            );

            chunks_to_embed.extend(file_chunks);
            pb.inc(1);
        }
        pb.finish_with_message("Done processing files.");

        // Handle deleted files: remove chunks for files that no longer exist
        let deleted_files: Vec<_> = manifest
            .files
            .keys()
            .filter(|path| !new_manifest_entries.contains_key(*path))
            .cloned()
            .collect();

        if !deleted_files.is_empty() {
            println!("Cleaning up {} deleted files...", deleted_files.len());
            for deleted_path in &deleted_files {
                if let Some(meta) = manifest.files.get(deleted_path) {
                    if !meta.chunk_ids.is_empty() {
                        debug!(
                            "Deleting {} chunks for removed file: {:?}",
                            meta.chunk_ids.len(),
                            deleted_path
                        );
                        if let Err(e) = db.delete_by_ids(&meta.chunk_ids).await {
                            warn!(
                                "Failed to delete chunks for deleted file {:?}: {}",
                                deleted_path, e
                            );
                        }
                    }
                }
            }
        }

        if chunks_to_embed.is_empty() {
            // Still save manifest to reflect deleted files
            manifest.files = new_manifest_entries;
            let file = fs::File::create(&manifest_path)?;
            serde_json::to_writer_pretty(file, &manifest)?;
            println!("No new files to index.");
            return Ok(());
        }

        println!("Embedding {} chunks...", chunks_to_embed.len());

        // Batch embedding
        let batch_size = self.config.embedding.batch_size;
        let total_batches = (chunks_to_embed.len() + batch_size - 1) / batch_size;
        info!(
            "Starting batch embedding: {} batches of size {}",
            total_batches, batch_size
        );

        let mut collection_initialized = false;

        for (batch_num, batch) in chunks_to_embed.chunks(batch_size).enumerate() {
            debug!(
                "Processing batch {}/{} ({} chunks)",
                batch_num + 1,
                total_batches,
                batch.len()
            );

            // Embed the semantic context path (e.g. "impl::Foo::fn::bar") alongside the
            // content so symbol/scope names influence the vector. The raw content is still
            // what gets stored and displayed.
            let texts: Vec<String> = batch
                .iter()
                .map(|c| match &c.context {
                    Some(ctx) if !ctx.is_empty() => format!("{}\n{}", ctx, c.content),
                    _ => c.content.clone(),
                })
                .collect();
            debug!("Extracted {} texts from batch", texts.len());

            let start = Instant::now();
            match embedder.embed(texts).await {
                Ok(embeddings) => {
                    let duration = start.elapsed();
                    debug!(
                        "Embedding batch {}/{} completed in {:?}",
                        batch_num + 1,
                        total_batches,
                        duration
                    );
                    debug!("Got {} embeddings", embeddings.len());

                    // Initialize collection on first batch using actual embedding dimension
                    if !collection_initialized {
                        if let Some(first_emb) = embeddings.first() {
                            let dim = first_emb.len();
                            info!("Detected embedding dimension: {}", dim);
                            db.init_collection(dim).await?;
                            collection_initialized = true;
                        }
                    }

                    let mut points = Vec::new();
                    for (chunk, embedding) in batch.iter().zip(embeddings) {
                        let payload = json!({
                            "file_path": chunk.file_path,
                            "content": chunk.content,
                            "chunk_index": chunk.chunk_index,
                            "context": chunk.context.clone().unwrap_or_default(),
                        });
                        points.push((chunk.id, embedding, payload));
                    }
                    debug!("Prepared {} points for upsert", points.len());

                    match db.upsert_chunks(points).await {
                        Ok(_) => debug!(
                            "Batch {}/{} upserted successfully",
                            batch_num + 1,
                            total_batches
                        ),
                        Err(e) => {
                            error!(
                                "Failed to upsert batch {}/{}: {:?}",
                                batch_num + 1,
                                total_batches,
                                e
                            );
                            return Err(e.context(format!("Batch {} upsert failed", batch_num + 1)));
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to embed batch {}/{}: {:?}",
                        batch_num + 1,
                        total_batches,
                        e
                    );
                    eprintln!("Failed to embed batch {}: {}", batch_num + 1, e);
                    return Err(e);
                }
            }
        }
        info!("All {} batches processed successfully", total_batches);

        manifest.files = new_manifest_entries;
        let file = fs::File::create(&manifest_path)?;
        serde_json::to_writer_pretty(file, &manifest)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        // uuid is already a dependency; use it to avoid parallel-test collisions.
        let dir = std::env::temp_dir().join(format!("msrch_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_index(db_path: &Path, marker: &[u8]) {
        fs::create_dir_all(db_path).unwrap();
        fs::write(db_path.join("data.lance"), marker).unwrap();
    }

    #[test]
    fn migrate_wipes_outdated_index() {
        let dir = temp_dir();
        let db_path = dir.join("index.db");
        seed_index(&db_path, b"stale");

        // An index written before this schema version, with tracked files.
        let mut manifest = Manifest {
            version: 0,
            files: HashMap::new(),
        };
        manifest.files.insert(
            PathBuf::from("foo.rs"),
            FileMetadata {
                modified_at: SystemTime::UNIX_EPOCH,
                chunk_ids: vec![uuid::Uuid::new_v4()],
            },
        );

        let migrated = Indexer::migrate_if_needed(&db_path, &mut manifest).unwrap();

        assert!(migrated, "should report that a migration happened");
        assert!(!db_path.exists(), "outdated index dir should be removed");
        assert_eq!(manifest.version, SCHEMA_VERSION);
        assert!(
            manifest.files.is_empty(),
            "manifest should be reset so every file is re-embedded"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrate_is_noop_when_version_current() {
        let dir = temp_dir();
        let db_path = dir.join("index.db");
        seed_index(&db_path, b"current");

        let mut manifest = Manifest {
            version: SCHEMA_VERSION,
            files: HashMap::new(),
        };
        manifest.files.insert(
            PathBuf::from("bar.rs"),
            FileMetadata {
                modified_at: SystemTime::UNIX_EPOCH,
                chunk_ids: vec![],
            },
        );

        let migrated = Indexer::migrate_if_needed(&db_path, &mut manifest).unwrap();

        assert!(!migrated, "no migration when the schema version matches");
        assert!(db_path.exists(), "current index dir should be preserved");
        assert_eq!(manifest.files.len(), 1, "manifest should be untouched");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrate_on_fresh_index_reports_no_wipe() {
        // No index on disk yet (fresh project): default manifest is version 0.
        let dir = temp_dir();
        let db_path = dir.join("index.db");
        let mut manifest = Manifest::default();

        let migrated = Indexer::migrate_if_needed(&db_path, &mut manifest).unwrap();

        assert!(!migrated, "nothing to wipe on a brand-new index");
        assert_eq!(
            manifest.version, SCHEMA_VERSION,
            "version should be stamped"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_manifest_json_reads_as_version_zero() {
        // Manifests written before versioning have no `version` field.
        let manifest: Manifest = serde_json::from_str(r#"{"files":{}}"#).unwrap();
        assert_eq!(
            manifest.version, 0,
            "a missing version must read as 0 so migration is triggered"
        );
    }
}
