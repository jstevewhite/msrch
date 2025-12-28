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
}

#[derive(Debug, Clone, PartialEq)]
enum FileType {
    Code,
    Markdown,
    Prose,
    Unknown,
}

pub struct Chunker {
    config: ChunkingConfig,
}

impl Chunker {
    pub fn new(config: ChunkingConfig) -> Self {
        Self { config }
    }

    fn determine_file_type(file_path: &PathBuf) -> FileType {
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();

        match extension.as_str() {
            // Code files
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "c" | "cpp" | "h" | "hpp"
            | "java" | "rb" | "sh" | "bash" | "zsh" | "swift" | "kt" | "scala" | "php"
            | "cs" | "lua" | "r" | "pl" | "pm" | "ex" | "exs" | "erl" | "hs" | "ml"
            | "vue" | "svelte" | "zig" | "nim" | "v" | "d" | "f90" | "jl" | "clj" | "lisp"
            | "el" | "vim" | "sql" | "graphql" | "proto" | "thrift" => FileType::Code,

            // Markdown
            "md" | "mdx" | "markdown" => FileType::Markdown,

            // Prose/text
            "txt" | "rst" | "adoc" | "asciidoc" | "org" | "tex" => FileType::Prose,

            // Unknown - use fallback
            _ => FileType::Unknown,
        }
    }

    /// Split code on blank lines (double newlines) which typically separate functions/blocks
    fn split_code<'a>(&self, content: &'a str) -> Vec<&'a str> {
        let segments: Vec<&str> = content
            .split("\n\n")
            .flat_map(|s| s.split("\r\n\r\n"))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        if segments.is_empty() {
            vec![content]
        } else {
            segments
        }
    }

    /// Split markdown on headers and paragraph breaks
    fn split_markdown<'a>(&self, content: &'a str) -> Vec<&'a str> {
        let mut segments = Vec::new();
        let mut current_start = 0;
        let bytes = content.as_bytes();

        let mut i = 0;
        while i < bytes.len() {
            // Check for newline
            if bytes[i] == b'\n' {
                let next_idx = i + 1;
                if next_idx < bytes.len() {
                    // Check for header: \n#
                    if bytes[next_idx] == b'#' {
                        let segment = &content[current_start..i];
                        if !segment.trim().is_empty() {
                            segments.push(segment.trim());
                        }
                        current_start = next_idx;
                    }
                    // Check for double newline (paragraph break)
                    else if bytes[next_idx] == b'\n' {
                        let segment = &content[current_start..i];
                        if !segment.trim().is_empty() {
                            segments.push(segment.trim());
                        }
                        // Skip past the double newline
                        current_start = next_idx + 1;
                        i = next_idx;
                    }
                }
            }
            i += 1;
        }

        // Don't forget the last segment
        if current_start < content.len() {
            let segment = &content[current_start..];
            if !segment.trim().is_empty() {
                segments.push(segment.trim());
            }
        }

        if segments.is_empty() {
            vec![content]
        } else {
            segments
        }
    }

    /// Split prose on paragraph breaks (double newlines)
    fn split_prose<'a>(&self, content: &'a str) -> Vec<&'a str> {
        content
            .split("\n\n")
            .flat_map(|s| s.split("\r\n\r\n"))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Return whole content as single segment - let normalize_segments handle token-based splitting
    fn split_default<'a>(&self, content: &'a str) -> Vec<&'a str> {
        vec![content]
    }

    /// Split content by tokens using sliding window (used for oversized segments)
    fn split_by_tokens(
        &self,
        content: &str,
        bpe: &tiktoken_rs::CoreBPE,
        max_tokens: usize,
        overlap: usize,
    ) -> Result<Vec<String>> {
        let tokens = bpe.encode_with_special_tokens(content);
        let mut result = Vec::new();
        let mut start_idx = 0;

        while start_idx < tokens.len() {
            let end_idx = std::cmp::min(start_idx + max_tokens, tokens.len());
            let chunk_tokens = &tokens[start_idx..end_idx];

            // Handle potential UTF-8 decode errors from slicing multi-byte chars
            let chunk_text = match bpe.decode(chunk_tokens.to_vec()) {
                Ok(text) => text,
                Err(_) => {
                    // Fallback: use lossy string conversion from raw bytes
                    String::from_utf8_lossy(
                        &chunk_tokens.iter()
                            .flat_map(|&t| bpe.decode(vec![t]).unwrap_or_default().into_bytes())
                            .collect::<Vec<u8>>()
                    ).to_string()
                }
            };
            result.push(chunk_text);

            if end_idx == tokens.len() {
                break;
            }
            start_idx += max_tokens.saturating_sub(overlap);
        }

        Ok(result)
    }

    /// Normalize segments to respect token limits:
    /// - Merge small segments
    /// - Split oversized segments with sliding window
    fn normalize_segments(
        &self,
        file_path: &PathBuf,
        segments: Vec<&str>,
        bpe: &tiktoken_rs::CoreBPE,
    ) -> Result<Vec<Chunk>> {
        let max_tokens = self.config.max_chunk_tokens;
        let overlap = self.config.overlap_tokens;

        let mut chunks = Vec::new();
        let mut pending_content = String::new();
        let mut pending_tokens = 0;
        let mut chunk_idx = 0;

        for segment in segments {
            let segment_tokens = bpe.encode_with_special_tokens(segment);
            let segment_token_count = segment_tokens.len();

            // Case 1: Segment is too large - needs splitting with sliding window
            if segment_token_count > max_tokens {
                // First, flush any pending content
                if !pending_content.is_empty() {
                    chunks.push(Chunk {
                        id: Uuid::new_v4(),
                        file_path: file_path.clone(),
                        chunk_index: chunk_idx,
                        content: pending_content.clone(),
                        token_count: pending_tokens,
                    });
                    chunk_idx += 1;
                    pending_content.clear();
                    pending_tokens = 0;
                }

                // Split this large segment using sliding window
                let sub_chunks = self.split_by_tokens(segment, bpe, max_tokens, overlap)?;
                for sub_chunk in sub_chunks {
                    let token_count = bpe.encode_with_special_tokens(&sub_chunk).len();
                    chunks.push(Chunk {
                        id: Uuid::new_v4(),
                        file_path: file_path.clone(),
                        chunk_index: chunk_idx,
                        content: sub_chunk,
                        token_count,
                    });
                    chunk_idx += 1;
                }
                continue;
            }

            // Case 2: Adding this segment would exceed max - flush pending first
            if pending_tokens + segment_token_count > max_tokens && !pending_content.is_empty() {
                chunks.push(Chunk {
                    id: Uuid::new_v4(),
                    file_path: file_path.clone(),
                    chunk_index: chunk_idx,
                    content: pending_content.clone(),
                    token_count: pending_tokens,
                });
                chunk_idx += 1;
                pending_content.clear();
                pending_tokens = 0;
            }

            // Case 3: Accumulate segment
            if !pending_content.is_empty() {
                pending_content.push_str("\n\n"); // Preserve semantic separation
                pending_tokens += 2; // Account for separator tokens (approximate)
            }
            pending_content.push_str(segment);
            pending_tokens += segment_token_count;
        }

        // Flush remaining
        if !pending_content.is_empty() {
            chunks.push(Chunk {
                id: Uuid::new_v4(),
                file_path: file_path.clone(),
                chunk_index: chunk_idx,
                content: pending_content,
                token_count: pending_tokens,
            });
        }

        Ok(chunks)
    }

    pub fn chunk_file(&self, file_path: &PathBuf, content: &str) -> Result<Vec<Chunk>> {
        let bpe = cl100k_base().context("Failed to get tokenizer")?;

        if content.trim().is_empty() {
            return Ok(vec![]);
        }

        // Step 1: Split content into semantic segments based on file type
        let file_type = Self::determine_file_type(file_path);
        let raw_segments: Vec<&str> = match file_type {
            FileType::Code => self.split_code(content),
            FileType::Markdown => self.split_markdown(content),
            FileType::Prose => self.split_prose(content),
            FileType::Unknown => self.split_default(content),
        };

        // Step 2: Normalize segments to respect token limits
        let chunks = self.normalize_segments(file_path, raw_segments, &bpe)?;

        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chunker() -> Chunker {
        Chunker::new(ChunkingConfig {
            max_chunk_tokens: 100, // Small for testing
            overlap_tokens: 10,
            max_file_size_mb: 10,
        })
    }

    #[test]
    fn test_file_type_detection() {
        assert_eq!(Chunker::determine_file_type(&PathBuf::from("foo.rs")), FileType::Code);
        assert_eq!(Chunker::determine_file_type(&PathBuf::from("bar.py")), FileType::Code);
        assert_eq!(Chunker::determine_file_type(&PathBuf::from("README.md")), FileType::Markdown);
        assert_eq!(Chunker::determine_file_type(&PathBuf::from("notes.txt")), FileType::Prose);
        assert_eq!(Chunker::determine_file_type(&PathBuf::from("data.json")), FileType::Unknown);
        assert_eq!(Chunker::determine_file_type(&PathBuf::from("no_extension")), FileType::Unknown);
    }

    #[test]
    fn test_code_splitting_on_blank_lines() {
        let chunker = test_chunker();
        let content = "fn foo() {\n    1\n}\n\nfn bar() {\n    2\n}";
        let segments = chunker.split_code(content);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].contains("foo"));
        assert!(segments[1].contains("bar"));
    }

    #[test]
    fn test_code_no_blank_lines() {
        let chunker = test_chunker();
        let content = "fn foo() {\n    1\n}";
        let segments = chunker.split_code(content);
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn test_markdown_splitting_on_headers() {
        let chunker = test_chunker();
        let content = "# Title\n\nIntro paragraph.\n\n## Section\n\nSection content.";
        let segments = chunker.split_markdown(content);
        assert!(segments.len() >= 2);
    }

    #[test]
    fn test_prose_splitting() {
        let chunker = test_chunker();
        let content = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let segments = chunker.split_prose(content);
        assert_eq!(segments.len(), 3);
    }

    #[test]
    fn test_empty_content_returns_empty() {
        let chunker = test_chunker();
        let path = PathBuf::from("empty.txt");
        let chunks = chunker.chunk_file(&path, "").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_whitespace_only_returns_empty() {
        let chunker = test_chunker();
        let path = PathBuf::from("whitespace.txt");
        let chunks = chunker.chunk_file(&path, "   \n\n  \t  ").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_indices_are_sequential() {
        let chunker = test_chunker();
        let path = PathBuf::from("test.rs");
        let content = "fn one() {}\n\nfn two() {}\n\nfn three() {}";
        let chunks = chunker.chunk_file(&path, &content).unwrap();
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn test_chunk_ids_are_unique() {
        let chunker = test_chunker();
        let path = PathBuf::from("test.md");
        let content = "# One\n\nParagraph.\n\n# Two\n\nAnother.";
        let chunks = chunker.chunk_file(&path, &content).unwrap();
        let ids: std::collections::HashSet<_> = chunks.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), chunks.len(), "All chunk IDs should be unique");
    }

    #[test]
    fn test_large_segment_gets_split() {
        let chunker = test_chunker();
        let path = PathBuf::from("test.txt");
        // Create content that's definitely > 100 tokens
        let content = "word ".repeat(200);
        let chunks = chunker.chunk_file(&path, &content).unwrap();
        assert!(chunks.len() > 1, "Large content should produce multiple chunks");
        for chunk in &chunks {
            assert!(
                chunk.token_count <= 100,
                "Each chunk should respect max_tokens, got {}",
                chunk.token_count
            );
        }
    }
}
