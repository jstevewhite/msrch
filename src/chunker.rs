use crate::config::ChunkingConfig;
use anyhow::{Context, Result};
use tiktoken_rs::cl100k_base;
use uuid::Uuid;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: Uuid,
    pub file_path: PathBuf,
    pub chunk_index: usize,
    pub content: String,
    pub token_count: usize,
    // Add more metadata fields as needed from HLD (byte_range, etc) if we can easily compute them
}

pub struct Chunker {
    config: ChunkingConfig,
}

impl Chunker {
    pub fn new(config: ChunkingConfig) -> Self {
        Self { config }
    }

    pub fn chunk_file(&self, file_path: &PathBuf, content: &str) -> Result<Vec<Chunk>> {
        // For POC, we use a simple sliding window strategy for everything.
        // In future, branch by file extension for clearer semantic splits.
        
        let bpe = cl100k_base().context("Failed to get tokenizer")?;
        let tokens = bpe.encode_with_special_tokens(content);
        
        let max_tokens = self.config.max_chunk_tokens;
        let overlap = self.config.overlap_tokens;
        
        if tokens.is_empty() {
             return Ok(vec![]);
        }

        let mut chunks = Vec::new();
        let mut start_idx = 0;
        let mut chunk_idx = 0;

        while start_idx < tokens.len() {
            let end_idx = std::cmp::min(start_idx + max_tokens, tokens.len());
            let chunk_tokens = &tokens[start_idx..end_idx];
            
            // Decode back to string
            let chunk_text = bpe.decode(chunk_tokens.to_vec())?;
            
            chunks.push(Chunk {
                id: Uuid::new_v4(),
                file_path: file_path.clone(),
                chunk_index: chunk_idx,
                content: chunk_text,
                token_count: chunk_tokens.len(),
            });

            chunk_idx += 1;
            
            if end_idx == tokens.len() {
                break;
            }
            
            // Move forward by stride
            start_idx += max_tokens - overlap;
        }

        Ok(chunks)
    }
}
