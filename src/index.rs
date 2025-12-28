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
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::Instant;

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    files: HashMap<PathBuf, FileMetadata>,
}

#[derive(Serialize, Deserialize, Clone)]
struct FileMetadata {
    modified_at: SystemTime,
    chunk_ids: Vec<uuid::Uuid>,
}

pub struct Indexer {
    config: Config,
    root_path: PathBuf,
}

impl Indexer {
    pub fn new(root_path: PathBuf, config: Config) -> Self {
        Self { root_path, config }
    }

    pub async fn index(&self) -> Result<()> {
        let msrch_dir = self.root_path.join(".msrch");
        fs::create_dir_all(&msrch_dir).context("Failed to create .msrch dir")?;

        let crawler = Crawler::new(self.config.indexing.clone()); // TODO: pass actual config
        let chunker = Chunker::new(self.config.chunking.clone());
        let embedder = EmbeddingClient::new(self.config.embedding.clone())?;
        println!("Using embedding endpoint: {}", self.config.embedding.endpoint);
        let db = VectorDB::new(msrch_dir.join("index.db")).await?;

        // Collection will be initialized on first embedding (to detect dimension)

        let manifest_path = msrch_dir.join("manifest.json");
        let mut manifest: Manifest = if manifest_path.exists() {
            let file = fs::File::open(&manifest_path)?;
            serde_json::from_reader(file).unwrap_or_default()
        } else {
            Manifest::default()
        };

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
            pb.set_message(format!("Processing {:?}", file_path.file_name().unwrap_or_default()));
            
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
                    debug!("Deleting {} stale chunks for modified file: {:?}",
                           existing_meta.chunk_ids.len(), file_path);
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
            
            new_manifest_entries.insert(file_path.clone(), FileMetadata {
                modified_at: modified,
                chunk_ids,
            });

            chunks_to_embed.extend(file_chunks);
            pb.inc(1);
        }
        pb.finish_with_message("Done processing files.");

        // Handle deleted files: remove chunks for files that no longer exist
        let deleted_files: Vec<_> = manifest.files.keys()
            .filter(|path| !new_manifest_entries.contains_key(*path))
            .cloned()
            .collect();

        if !deleted_files.is_empty() {
            println!("Cleaning up {} deleted files...", deleted_files.len());
            for deleted_path in &deleted_files {
                if let Some(meta) = manifest.files.get(deleted_path) {
                    if !meta.chunk_ids.is_empty() {
                        debug!("Deleting {} chunks for removed file: {:?}",
                               meta.chunk_ids.len(), deleted_path);
                        if let Err(e) = db.delete_by_ids(&meta.chunk_ids).await {
                            warn!("Failed to delete chunks for deleted file {:?}: {}", deleted_path, e);
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
        info!("Starting batch embedding: {} batches of size {}", total_batches, batch_size);

        let mut collection_initialized = false;

        for (batch_num, batch) in chunks_to_embed.chunks(batch_size).enumerate() {
            debug!("Processing batch {}/{} ({} chunks)", batch_num + 1, total_batches, batch.len());

            let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
            debug!("Extracted {} texts from batch", texts.len());

            let start = Instant::now();
            match embedder.embed(texts).await {
                Ok(embeddings) => {
                    let duration = start.elapsed();
                    debug!("Embedding batch {}/{} completed in {:?}", batch_num + 1, total_batches, duration);
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
                        });
                        points.push((chunk.id, embedding, payload));
                    }
                    debug!("Prepared {} points for upsert", points.len());

                    match db.upsert_chunks(points).await {
                        Ok(_) => debug!("Batch {}/{} upserted successfully", batch_num + 1, total_batches),
                        Err(e) => {
                            error!("Failed to upsert batch {}/{}: {:?}", batch_num + 1, total_batches, e);
                            return Err(e.context(format!("Batch {} upsert failed", batch_num + 1)));
                        }
                    }
                },
                Err(e) => {
                    error!("Failed to embed batch {}/{}: {:?}", batch_num + 1, total_batches, e);
                    eprintln!("Failed to embed batch {}: {}", batch_num + 1, e);
                    return Err(e);
                },
            }
        }
        info!("All {} batches processed successfully", total_batches);

        manifest.files = new_manifest_entries;
        let file = fs::File::create(&manifest_path)?;
        serde_json::to_writer_pretty(file, &manifest)?;

        Ok(())
    }
}
