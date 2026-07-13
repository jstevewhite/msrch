use crate::chunker::{Chunk, Chunker};
use crate::config::Config;
use crate::crawler::Crawler;
use crate::db::VectorDB;
use crate::embedding::{Embedder, EmbeddingClient};
use anyhow::{Context, Result};
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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

impl IndexStats {
    pub fn print(&self) {
        println!("{}", "Index Statistics".bold().underline());
        println!();
        println!("  {:<18} {}", "Index:".cyan(), self.index_path.display());
        println!("  {:<18} {}", "Root:".cyan(), self.root_path.display());
        println!("  {:<18} {}", "Files:".cyan(), self.file_count);
        println!("  {:<18} {}", "Chunks:".cyan(), self.chunk_count);
        println!("  {:<18} ~{}", "Est. tokens:".cyan(), self.estimated_tokens);
        println!("  {:<18} {}", "Model:".cyan(), self.model);
        println!("  {:<18} {}", "Endpoint:".cyan(), self.endpoint);

        if let Some(last) = self.last_indexed {
            if let Ok(duration) = last.duration_since(SystemTime::UNIX_EPOCH) {
                let datetime = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                println!("  {:<18} {}", "Last indexed:".cyan(), datetime);
            }
        }

        let size_str = if self.size_on_disk >= 1024 * 1024 {
            format!("{:.1} MB", self.size_on_disk as f64 / (1024.0 * 1024.0))
        } else if self.size_on_disk >= 1024 {
            format!("{:.1} KB", self.size_on_disk as f64 / 1024.0)
        } else {
            format!("{} bytes", self.size_on_disk)
        };
        println!("  {:<18} {}", "Size on disk:".cyan(), size_str);
    }
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

/// Stable string form used for the `file_path` column and delete filters.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Write `manifest.json` via temp file + rename so readers never see a partial file.
fn atomic_write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("Failed to create temporary manifest {}", tmp.display()))?;
        serde_json::to_writer_pretty(&mut file, manifest)
            .context("Failed to serialize manifest")?;
        file.sync_all().context("Failed to fsync temporary manifest")?;
    }
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "Failed to replace manifest {} with {}",
            path.display(),
            tmp.display()
        )
    })?;
    Ok(())
}

fn load_manifest(path: &Path) -> Result<Manifest> {
    if !path.exists() {
        return Ok(Manifest::default());
    }
    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open manifest {}", path.display()))?;
    let manifest = serde_json::from_reader(file).unwrap_or_default();
    Ok(manifest)
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
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;
        println!(
            "Using embedding endpoint: {}",
            self.config.embedding.endpoint
        );
        self.index_with_embedder(&embedder).await
    }

    /// Index using an arbitrary [`Embedder`]. Used by production and tests.
    pub async fn index_with_embedder<E: Embedder>(&self, embedder: &E) -> Result<()> {
        let msrch_dir = self.root_path.join(".msrch");
        fs::create_dir_all(&msrch_dir).context("Failed to create .msrch dir")?;

        let crawler = Crawler::new(self.config.indexing.clone());
        let mut chunker = Chunker::new(self.config.chunking.clone());

        let manifest_path = msrch_dir.join("manifest.json");
        let db_path = msrch_dir.join("index.db");
        let mut prior_manifest = load_manifest(&manifest_path)?;

        // Rebuild from scratch if the on-disk schema predates the current version.
        if Self::migrate_if_needed(&db_path, &mut prior_manifest)? {
            println!(
                "Index schema is outdated; rebuilding from scratch (schema v{}).",
                SCHEMA_VERSION
            );
            // Persist the wiped state so a crash mid-rebuild doesn't leave a
            // version-0 manifest pointing at a missing/partial DB.
            atomic_write_manifest(&manifest_path, &prior_manifest)?;
        }

        // Connect after any migration so the table is created with the current schema.
        let db = VectorDB::new(db_path).await?;

        println!("Scanning files...");
        let files = crawler.crawl(&self.root_path)?;
        println!("Found {} files.", files.len());

        let file_set: std::collections::HashSet<PathBuf> = files.iter().cloned().collect();

        // Working manifest is rebuilt intentionally; only fully committed files land here.
        let mut working = Manifest {
            version: SCHEMA_VERSION,
            files: HashMap::new(),
        };

        // 1) Unchanged files: keep prior entry, no DB touch.
        for path in &files {
            let modified = fs::metadata(path)
                .with_context(|| format!("Failed to stat {}", path.display()))?
                .modified()
                .with_context(|| format!("Failed to read mtime for {}", path.display()))?;
            if let Some(meta) = prior_manifest.files.get(path) {
                if meta.modified_at == modified {
                    working.files.insert(path.clone(), meta.clone());
                }
            }
        }

        // 2) Deleted files: remove vectors, then drop from store of record.
        let deleted: Vec<PathBuf> = prior_manifest
            .files
            .keys()
            .filter(|p| !file_set.contains(*p))
            .cloned()
            .collect();

        if !deleted.is_empty() {
            println!("Cleaning up {} deleted files...", deleted.len());
            for path in &deleted {
                let key = path_key(path);
                debug!("Deleting vectors for removed file: {}", key);
                db.delete_by_file_path(&key).await.with_context(|| {
                    format!(
                        "Failed to delete vectors for removed file {}",
                        path.display()
                    )
                })?;
            }
        }

        // Persist after deletes + unchanged copy so a later dirty-file failure
        // still leaves removed files gone and unchanged files recorded.
        atomic_write_manifest(&manifest_path, &working)?;

        let dirty: Vec<PathBuf> = files
            .iter()
            .filter(|p| !working.files.contains_key(*p))
            .cloned()
            .collect();

        if dirty.is_empty() {
            println!("No new files to index.");
            return Ok(());
        }

        println!("Indexing {} changed/new file(s)...", dirty.len());
        let pb = ProgressBar::new(dirty.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}")
                .unwrap()
                .progress_chars("#>-"),
        );

        let mut collection_initialized = false;
        let mut embedded_files = 0usize;

        for path in dirty {
            pb.set_message(format!(
                "{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));

            let modified = fs::metadata(&path)
                .with_context(|| format!("Failed to stat {}", path.display()))?
                .modified()
                .with_context(|| format!("Failed to read mtime for {}", path.display()))?;

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    // Keep prior index entry so flaky permissions don't wipe vectors.
                    if let Some(prev) = prior_manifest.files.get(&path) {
                        warn!(
                            "Skipping unreadable {}: {} (keeping previous index entry)",
                            path.display(),
                            e
                        );
                        working.files.insert(path.clone(), prev.clone());
                        atomic_write_manifest(&manifest_path, &working)?;
                    } else {
                        warn!("Skipping unreadable new file {}: {}", path.display(), e);
                    }
                    pb.inc(1);
                    continue;
                }
            };

            let chunks = chunker.chunk_file(&path, &content)?;
            let key = path_key(&path);

            // Clear prior vectors + any orphans from a partial prior run.
            db.delete_by_file_path(&key).await.with_context(|| {
                format!("Failed to delete stale vectors for {}", path.display())
            })?;

            if !chunks.is_empty() {
                self.embed_and_store_chunks(
                    &db,
                    embedder,
                    &chunks,
                    &mut collection_initialized,
                )
                .await
                .with_context(|| format!("Failed to embed/store {}", path.display()))?;
            }

            let chunk_ids: Vec<uuid::Uuid> = chunks.iter().map(|c| c.id).collect();
            working.files.insert(
                path.clone(),
                FileMetadata {
                    modified_at: modified,
                    chunk_ids,
                },
            );
            // Commit unit: this file is fully in the DB before its manifest entry is durable.
            atomic_write_manifest(&manifest_path, &working)?;
            embedded_files += 1;
            pb.inc(1);
        }

        pb.finish_with_message("Done.");
        info!("Committed {} file(s) to the index", embedded_files);
        Ok(())
    }

    /// Embed and append all chunks for one file (may use multiple API batches).
    /// Caller must already have deleted any prior rows for this file's path.
    async fn embed_and_store_chunks<E: Embedder>(
        &self,
        db: &VectorDB,
        embedder: &E,
        chunks: &[Chunk],
        collection_initialized: &mut bool,
    ) -> Result<()> {
        let batch_size = self.config.embedding.batch_size.max(1);
        let total_batches = (chunks.len() + batch_size - 1) / batch_size;

        for (batch_num, batch) in chunks.chunks(batch_size).enumerate() {
            debug!(
                "Embedding batch {}/{} ({} chunks)",
                batch_num + 1,
                total_batches,
                batch.len()
            );

            // Embed the semantic context path alongside content so symbol names
            // influence the vector. Stored/displayed content remains the raw text.
            let texts: Vec<String> = batch
                .iter()
                .map(|c| match &c.context {
                    Some(ctx) if !ctx.is_empty() => format!("{}\n{}", ctx, c.content),
                    _ => c.content.clone(),
                })
                .collect();

            let embeddings = embedder.embed(texts).await.map_err(|e| {
                error!(
                    "Failed to embed batch {}/{}: {:?}",
                    batch_num + 1,
                    total_batches,
                    e
                );
                e
            })?;

            if !*collection_initialized {
                if let Some(first_emb) = embeddings.first() {
                    let dim = first_emb.len();
                    info!("Detected embedding dimension: {}", dim);
                    db.init_collection(dim).await?;
                    *collection_initialized = true;
                }
            }

            let mut points = Vec::with_capacity(batch.len());
            for (chunk, embedding) in batch.iter().zip(embeddings) {
                let payload = json!({
                    "file_path": path_key(&chunk.file_path),
                    "content": chunk.content,
                    "chunk_index": chunk.chunk_index,
                    "context": chunk.context.clone().unwrap_or_default(),
                });
                points.push((chunk.id, embedding, payload));
            }

            db.upsert_chunks(points).await.with_context(|| {
                format!("Batch {} upsert failed", batch_num + 1)
            })?;
            debug!(
                "Batch {}/{} upserted successfully",
                batch_num + 1,
                total_batches
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::FakeEmbedder;
    use std::time::Duration;

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

    fn test_config() -> Config {
        let mut config = Config::default();
        config.embedding.batch_size = 32;
        config.chunking.use_treesitter = false;
        config.chunking.max_chunk_tokens = 512;
        config
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn load_test_manifest(root: &Path) -> Manifest {
        load_manifest(&root.join(".msrch/manifest.json")).unwrap()
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

    #[test]
    fn atomic_write_manifest_round_trips() {
        let dir = temp_dir();
        let path = dir.join("manifest.json");
        let mut manifest = Manifest {
            version: SCHEMA_VERSION,
            files: HashMap::new(),
        };
        let id = uuid::Uuid::new_v4();
        manifest.files.insert(
            PathBuf::from("a.rs"),
            FileMetadata {
                modified_at: SystemTime::UNIX_EPOCH,
                chunk_ids: vec![id],
            },
        );

        atomic_write_manifest(&path, &manifest).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());

        let loaded = load_manifest(&path).unwrap();
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files[&PathBuf::from("a.rs")].chunk_ids, vec![id]);

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn indexes_two_files_and_records_manifest() {
        let root = temp_dir();
        write_file(&root.join("a.txt"), "alpha content for indexing");
        write_file(&root.join("b.txt"), "beta content for indexing");

        let indexer = Indexer::new(root.clone(), test_config());
        let embedder = FakeEmbedder::new(8);
        indexer.index_with_embedder(&embedder).await.unwrap();

        let manifest = load_test_manifest(&root);
        assert_eq!(manifest.version, SCHEMA_VERSION);
        assert_eq!(manifest.files.len(), 2);

        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        assert_eq!(db.count().await.unwrap(), 2);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn reindex_file_replaces_vectors_without_duplicates() {
        let root = temp_dir();
        let file = root.join("doc.txt");
        write_file(&file, "version one of the document text");

        let indexer = Indexer::new(root.clone(), test_config());
        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        let count_after_first = db.count().await.unwrap();
        assert!(count_after_first >= 1);

        // Ensure mtime advances on filesystems with coarse timestamps.
        std::thread::sleep(Duration::from_millis(20));
        write_file(&file, "version two of the document text which is different");

        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        // Reconnect so we observe commits from the second indexer connection.
        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        let count_after_second = db.count().await.unwrap();
        assert_eq!(
            count_after_second, count_after_first,
            "re-index must replace vectors, not append duplicates"
        );

        let key = path_key(&file);
        assert_eq!(db.count_by_file_path(&key).await.unwrap(), count_after_first);

        let manifest = load_test_manifest(&root);
        assert_eq!(manifest.files.len(), 1);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn failed_embed_keeps_prior_committed_files() {
        let root = temp_dir();
        // Distinct names so crawl order is deterministic enough; we still
        // only require that *some* file is committed before the failure.
        write_file(&root.join("1_first.txt"), "first file body");
        write_file(&root.join("2_second.txt"), "second file body");

        let indexer = Indexer::new(root.clone(), test_config());
        // Fail on the second embed call (second dirty file).
        let embedder = FakeEmbedder::fail_at(8, 1);
        let err = indexer.index_with_embedder(&embedder).await;
        assert!(err.is_err(), "second embed should fail the run");

        let manifest = load_test_manifest(&root);
        assert_eq!(
            manifest.files.len(),
            1,
            "only the first successfully committed file should be in the manifest"
        );

        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        assert_eq!(db.count().await.unwrap(), 1);

        // Recovery: full re-run succeeds and indexes both.
        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();
        let manifest = load_test_manifest(&root);
        assert_eq!(manifest.files.len(), 2);
        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        assert_eq!(db.count().await.unwrap(), 2);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn orphan_rows_cleaned_on_reindex() {
        let root = temp_dir();
        let file = root.join("solo.txt");
        write_file(&file, "solo document content");

        let indexer = Indexer::new(root.clone(), test_config());
        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        let key = path_key(&file);

        // Simulate a partial failure: orphan row with unknown id for same path.
        let orphan_id = uuid::Uuid::new_v4();
        db.upsert_chunks(vec![(
            orphan_id,
            vec![0.5_f32; 8],
            json!({
                "file_path": key,
                "content": "orphan",
                "chunk_index": 99u64,
                "context": "",
            }),
        )])
        .await
        .unwrap();
        assert_eq!(db.count_by_file_path(&key).await.unwrap(), 2);

        std::thread::sleep(Duration::from_millis(20));
        write_file(&file, "solo document content updated");

        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        assert_eq!(
            db.count_by_file_path(&key).await.unwrap(),
            1,
            "path-delete before re-add must clear orphans"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn deleted_file_removes_vectors_and_manifest_entry() {
        let root = temp_dir();
        let keep = root.join("keep.txt");
        let drop = root.join("drop.txt");
        write_file(&keep, "keep me around");
        write_file(&drop, "delete me later");

        let indexer = Indexer::new(root.clone(), test_config());
        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        assert_eq!(db.count().await.unwrap(), 2);

        fs::remove_file(&drop).unwrap();
        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        assert_eq!(db.count().await.unwrap(), 1);
        assert_eq!(db.count_by_file_path(&path_key(&drop)).await.unwrap(), 0);
        assert_eq!(db.count_by_file_path(&path_key(&keep)).await.unwrap(), 1);

        let manifest = load_test_manifest(&root);
        assert_eq!(manifest.files.len(), 1);
        assert!(manifest.files.contains_key(&keep));

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn unreadable_previously_indexed_file_keeps_entry() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir();
        let file = root.join("locked.txt");
        write_file(&file, "initially readable content");

        let indexer = Indexer::new(root.clone(), test_config());
        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        let prior = load_test_manifest(&root);
        let prior_meta = prior.files.get(&file).cloned().expect("file was indexed");
        let prior_count = {
            let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
            db.count().await.unwrap()
        };

        // Bump mtime then remove all permissions so the path is dirty but unreadable.
        std::thread::sleep(Duration::from_millis(20));
        write_file(&file, "changed but will be locked");
        let mut perms = fs::metadata(&file).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&file, perms).unwrap();

        indexer
            .index_with_embedder(&FakeEmbedder::new(8))
            .await
            .unwrap();

        // Restore perms for cleanup/assertions.
        let mut perms = fs::metadata(&file).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&file, perms).unwrap();

        let after = load_test_manifest(&root);
        let after_meta = after.files.get(&file).expect("entry must be retained");
        // Kept the previous chunk ids (could not re-read to re-embed).
        assert_eq!(after_meta.chunk_ids, prior_meta.chunk_ids);

        let db = VectorDB::new(root.join(".msrch/index.db")).await.unwrap();
        assert_eq!(db.count().await.unwrap(), prior_count);

        fs::remove_dir_all(&root).ok();
    }
}
