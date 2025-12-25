use crate::chunker::{Chunker, Chunk};
use crate::config::Config;
use crate::crawler::Crawler;
use crate::db::VectorDB;
use crate::embedding::EmbeddingClient;
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

        // Initialize Collection (assume default dim for now, or fetch from first embed)
        db.init_collection(1024).await?; // mxbai-large is 1024 dims

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
                // TODO: Delete old chunks from DB if reindexing (requires delete support in DB wrapper)
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

        if chunks_to_embed.is_empty() {
            println!("No new files to index.");
            // Update manifest just in case deletions happened (not handled fully here yet)
            return Ok(());
        }

        println!("Embedding {} chunks...", chunks_to_embed.len());
        
        // Batch embedding
        let batch_size = self.config.embedding.batch_size;
        for batch in chunks_to_embed.chunks(batch_size) {
            let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
            match embedder.embed(texts).await {
                Ok(embeddings) => {
                     let mut points = Vec::new();
                     for (chunk, embedding) in batch.iter().zip(embeddings) {
                         let payload = json!({
                             "file_path": chunk.file_path,
                             "content": chunk.content,
                             "chunk_index": chunk.chunk_index,
                             // Add more metadata
                         });
                         points.push((chunk.id, embedding, payload));
                     }
                     db.upsert_chunks(points).await?;
                },
                Err(e) => eprintln!("Failed to embed batch: {}", e),
            }
        }

        manifest.files = new_manifest_entries;
        let file = fs::File::create(&manifest_path)?;
        serde_json::to_writer_pretty(file, &manifest)?;

        Ok(())
    }
}
