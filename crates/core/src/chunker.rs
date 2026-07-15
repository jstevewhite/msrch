use crate::config::ChunkingConfig;
use anyhow::Result;
use log::{debug, warn};
use std::path::PathBuf;
use std::sync::LazyLock;
use tiktoken_rs::cl100k_base;
use tree_sitter::{Node, Parser};
use uuid::Uuid;

/// Shared tokenizer, built once. `cl100k_base` loads from data embedded in the
/// crate, so this is effectively infallible at runtime.
static BPE: LazyLock<tiktoken_rs::CoreBPE> =
    LazyLock::new(|| cl100k_base().expect("failed to initialize cl100k_base tokenizer"));

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: Uuid,
    pub file_path: PathBuf,
    pub chunk_index: usize,
    pub content: String,
    pub token_count: usize,
    pub context: Option<String>, // e.g., "mod auth::fn verify_token"
}

#[derive(Debug, Clone, PartialEq)]
enum FileType {
    Code,
    Markdown,
    Prose,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
enum CodeLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Unsupported,
}

pub struct Chunker {
    config: ChunkingConfig,
    rust_parser: Option<Parser>,
    python_parser: Option<Parser>,
    javascript_parser: Option<Parser>,
    typescript_parser: Option<Parser>,
    tsx_parser: Option<Parser>,
    go_parser: Option<Parser>,
}

impl Chunker {
    pub fn new(config: ChunkingConfig) -> Self {
        let mut chunker = Self {
            config,
            rust_parser: None,
            python_parser: None,
            javascript_parser: None,
            typescript_parser: None,
            tsx_parser: None,
            go_parser: None,
        };

        // Initialize tree-sitter parsers if enabled
        if chunker.config.use_treesitter {
            chunker.init_parsers();
        }

        chunker
    }

    fn init_parsers(&mut self) {
        // Clone the small language list so the loop doesn't hold a borrow on
        // `self.config` while we call `&mut self` helpers like `init_tsx_parser`.
        let languages = self.config.treesitter_languages.clone();
        for lang in &languages {
            match lang.as_str() {
                "rust" => {
                    let mut parser = Parser::new();
                    if parser
                        .set_language(&tree_sitter_rust::LANGUAGE.into())
                        .is_ok()
                    {
                        self.rust_parser = Some(parser);
                        debug!("Initialized Rust tree-sitter parser");
                    }
                }
                "python" => {
                    let mut parser = Parser::new();
                    if parser
                        .set_language(&tree_sitter_python::LANGUAGE.into())
                        .is_ok()
                    {
                        self.python_parser = Some(parser);
                        debug!("Initialized Python tree-sitter parser");
                    }
                }
                "javascript" => {
                    let mut parser = Parser::new();
                    if parser
                        .set_language(&tree_sitter_javascript::LANGUAGE.into())
                        .is_ok()
                    {
                        self.javascript_parser = Some(parser);
                        debug!("Initialized JavaScript tree-sitter parser");
                    }
                }
                "typescript" => {
                    let mut parser = Parser::new();
                    if parser
                        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
                        .is_ok()
                    {
                        self.typescript_parser = Some(parser);
                        debug!("Initialized TypeScript tree-sitter parser");
                    }
                    // `.tsx` needs the JSX-aware grammar, so enabling TypeScript also
                    // provisions the TSX parser.
                    self.init_tsx_parser();
                }
                "tsx" => self.init_tsx_parser(),
                "go" => {
                    let mut parser = Parser::new();
                    if parser
                        .set_language(&tree_sitter_go::LANGUAGE.into())
                        .is_ok()
                    {
                        self.go_parser = Some(parser);
                        debug!("Initialized Go tree-sitter parser");
                    }
                }
                _ => {
                    warn!("Unsupported tree-sitter language: {}", lang);
                }
            }
        }
    }

    fn init_tsx_parser(&mut self) {
        if self.tsx_parser.is_some() {
            return;
        }
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .is_ok()
        {
            self.tsx_parser = Some(parser);
            debug!("Initialized TSX tree-sitter parser");
        }
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
            | "java" | "rb" | "sh" | "bash" | "zsh" | "swift" | "kt" | "scala" | "php" | "cs"
            | "lua" | "r" | "pl" | "pm" | "ex" | "exs" | "erl" | "hs" | "ml" | "vue" | "svelte"
            | "zig" | "nim" | "v" | "d" | "f90" | "jl" | "clj" | "lisp" | "el" | "vim" | "sql"
            | "graphql" | "proto" | "thrift" => FileType::Code,

            // Markdown
            "md" | "mdx" | "markdown" => FileType::Markdown,

            // Prose/text
            "txt" | "rst" | "adoc" | "asciidoc" | "org" | "tex" => FileType::Prose,

            // Unknown - use fallback
            _ => FileType::Unknown,
        }
    }

    fn detect_code_language(file_path: &PathBuf) -> CodeLanguage {
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();

        match extension.as_str() {
            "rs" => CodeLanguage::Rust,
            "py" => CodeLanguage::Python,
            "js" | "jsx" => CodeLanguage::JavaScript,
            "ts" => CodeLanguage::TypeScript,
            "tsx" => CodeLanguage::Tsx,
            "go" => CodeLanguage::Go,
            _ => CodeLanguage::Unsupported,
        }
    }

    /// Extract semantic code chunks using tree-sitter
    fn chunk_with_treesitter(
        &mut self,
        file_path: &PathBuf,
        content: &str,
        language: CodeLanguage,
    ) -> Result<Option<Vec<Chunk>>> {
        let parser = match language {
            CodeLanguage::Rust => self.rust_parser.as_mut(),
            CodeLanguage::Python => self.python_parser.as_mut(),
            CodeLanguage::JavaScript => self.javascript_parser.as_mut(),
            CodeLanguage::TypeScript => self.typescript_parser.as_mut(),
            CodeLanguage::Tsx => self.tsx_parser.as_mut(),
            CodeLanguage::Go => self.go_parser.as_mut(),
            CodeLanguage::Unsupported => return Ok(None),
        };

        let parser = match parser {
            Some(p) => p,
            None => return Ok(None),
        };

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => {
                warn!("Failed to parse {} with tree-sitter", file_path.display());
                return Ok(None);
            }
        };

        let root_node = tree.root_node();

        // Check for parse errors
        if root_node.has_error() {
            debug!(
                "Parse errors in {}, falling back to token chunking",
                file_path.display()
            );
            return Ok(None);
        }

        let mut chunks = Vec::new();
        let bpe = &*BPE;

        match language {
            CodeLanguage::Rust => {
                self.extract_rust_items(file_path, content, root_node, &bpe, &mut chunks)?
            }
            CodeLanguage::Python => {
                self.extract_python_items(file_path, content, root_node, &bpe, &mut chunks)?
            }
            CodeLanguage::JavaScript | CodeLanguage::TypeScript | CodeLanguage::Tsx => {
                self.extract_js_items(file_path, content, root_node, &bpe, &mut chunks)?
            }
            CodeLanguage::Go => {
                self.extract_go_items(file_path, content, root_node, &bpe, &mut chunks)?
            }
            CodeLanguage::Unsupported => return Ok(None),
        }

        if chunks.is_empty() {
            debug!(
                "No semantic chunks extracted from {}, falling back",
                file_path.display()
            );
            return Ok(None);
        }

        debug!(
            "Extracted {} semantic chunks from {} using tree-sitter",
            chunks.len(),
            file_path.display()
        );
        Ok(Some(chunks))
    }

    /// Extract Rust items (functions, structs, impls, etc.)
    fn extract_rust_items(
        &self,
        file_path: &PathBuf,
        content: &str,
        root_node: Node,
        bpe: &tiktoken_rs::CoreBPE,
        chunks: &mut Vec<Chunk>,
    ) -> Result<()> {
        let mut chunk_idx = 0;

        fn visit_rust_node(
            node: Node,
            content: &str,
            file_path: &PathBuf,
            bpe: &tiktoken_rs::CoreBPE,
            max_tokens: usize,
            chunks: &mut Vec<Chunk>,
            chunk_idx: &mut usize,
            context_path: &str,
        ) {
            let kind = node.kind();

            // Nodes we want to extract as chunks
            let is_extractable = matches!(
                kind,
                "function_item"
                    | "struct_item"
                    | "enum_item"
                    | "trait_item"
                    | "impl_item"
                    | "mod_item"
                    | "const_item"
                    | "static_item"
            );

            if is_extractable {
                if let Ok(text) = node.utf8_text(content.as_bytes()) {
                    // Prepend any leading doc comments and attributes.
                    let full_text = match extract_rust_leading(node, content) {
                        Some(leading) => format!("{}\n{}", leading, text),
                        None => text.to_string(),
                    };

                    let token_count = bpe.encode_with_special_tokens(&full_text).len();

                    // Skip if too large - fallback chunker will handle it
                    if token_count <= max_tokens {
                        // Build context path
                        let item_name = extract_rust_item_name(node, content);
                        let new_context = if context_path.is_empty() {
                            format!("{}::{}", kind.replace("_item", ""), item_name)
                        } else {
                            format!(
                                "{}::{}::{}",
                                context_path,
                                kind.replace("_item", ""),
                                item_name
                            )
                        };

                        chunks.push(Chunk {
                            id: Uuid::new_v4(),
                            file_path: file_path.clone(),
                            chunk_index: *chunk_idx,
                            content: full_text,
                            token_count,
                            context: Some(new_context.clone()),
                        });
                        *chunk_idx += 1;

                        // For impl blocks and modules, also visit children
                        if kind == "impl_item" || kind == "mod_item" {
                            let mut child_cursor = node.walk();
                            for child in node.children(&mut child_cursor) {
                                visit_rust_node(
                                    child,
                                    content,
                                    file_path,
                                    bpe,
                                    max_tokens,
                                    chunks,
                                    chunk_idx,
                                    &new_context,
                                );
                            }
                        }
                        return; // Don't visit children again
                    }
                }
            }

            // Visit children
            let mut child_cursor = node.walk();
            for child in node.children(&mut child_cursor) {
                visit_rust_node(
                    child,
                    content,
                    file_path,
                    bpe,
                    max_tokens,
                    chunks,
                    chunk_idx,
                    context_path,
                );
            }
        }

        visit_rust_node(
            root_node,
            content,
            file_path,
            bpe,
            self.config.max_chunk_tokens,
            chunks,
            &mut chunk_idx,
            "",
        );
        Ok(())
    }

    /// Extract Python items (functions and classes)
    fn extract_python_items(
        &self,
        file_path: &PathBuf,
        content: &str,
        root_node: Node,
        bpe: &tiktoken_rs::CoreBPE,
        chunks: &mut Vec<Chunk>,
    ) -> Result<()> {
        let mut chunk_idx = 0;

        fn visit_python_node(
            node: Node,
            content: &str,
            file_path: &PathBuf,
            bpe: &tiktoken_rs::CoreBPE,
            max_tokens: usize,
            chunks: &mut Vec<Chunk>,
            chunk_idx: &mut usize,
            context_path: &str,
        ) {
            let kind = node.kind();

            let is_extractable = matches!(kind, "function_definition" | "class_definition");

            if is_extractable {
                // A decorated def parses as `decorated_definition > (decorator..., def)`;
                // anchor on the wrapper so the chunk carries its decorators.
                let anchor = node
                    .parent()
                    .filter(|p| p.kind() == "decorated_definition")
                    .unwrap_or(node);
                if let Ok(text) = anchor.utf8_text(content.as_bytes()) {
                    let full_text = match extract_leading_comments(anchor, content) {
                        Some(comment) => format!("{}\n{}", comment, text),
                        None => text.to_string(),
                    };
                    let token_count = bpe.encode_with_special_tokens(&full_text).len();

                    if token_count <= max_tokens {
                        let item_name = extract_python_item_name(node, content);
                        let new_context = if context_path.is_empty() {
                            format!(
                                "{}::{}",
                                if kind == "function_definition" {
                                    "fn"
                                } else {
                                    "class"
                                },
                                item_name
                            )
                        } else {
                            format!(
                                "{}::{}::{}",
                                context_path,
                                if kind == "function_definition" {
                                    "fn"
                                } else {
                                    "class"
                                },
                                item_name
                            )
                        };

                        chunks.push(Chunk {
                            id: Uuid::new_v4(),
                            file_path: file_path.clone(),
                            chunk_index: *chunk_idx,
                            content: full_text,
                            token_count,
                            context: Some(new_context.clone()),
                        });
                        *chunk_idx += 1;

                        // For classes, also visit methods
                        if kind == "class_definition" {
                            let mut child_cursor = node.walk();
                            for child in node.children(&mut child_cursor) {
                                visit_python_node(
                                    child,
                                    content,
                                    file_path,
                                    bpe,
                                    max_tokens,
                                    chunks,
                                    chunk_idx,
                                    &new_context,
                                );
                            }
                        }
                        return;
                    }
                }
            }

            // Visit children
            let mut child_cursor = node.walk();
            for child in node.children(&mut child_cursor) {
                visit_python_node(
                    child,
                    content,
                    file_path,
                    bpe,
                    max_tokens,
                    chunks,
                    chunk_idx,
                    context_path,
                );
            }
        }

        visit_python_node(
            root_node,
            content,
            file_path,
            bpe,
            self.config.max_chunk_tokens,
            chunks,
            &mut chunk_idx,
            "",
        );
        Ok(())
    }

    /// Extract JavaScript/TypeScript items
    fn extract_js_items(
        &self,
        file_path: &PathBuf,
        content: &str,
        root_node: Node,
        bpe: &tiktoken_rs::CoreBPE,
        chunks: &mut Vec<Chunk>,
    ) -> Result<()> {
        let mut chunk_idx = 0;

        fn visit_js_node(
            node: Node,
            content: &str,
            file_path: &PathBuf,
            bpe: &tiktoken_rs::CoreBPE,
            max_tokens: usize,
            chunks: &mut Vec<Chunk>,
            chunk_idx: &mut usize,
            context_path: &str,
        ) {
            let kind = node.kind();

            let is_extractable = matches!(
                kind,
                "function_declaration"
                    | "method_definition"
                    | "class_declaration"
                    | "arrow_function"
                    | "function_expression"
                    // TypeScript-only top-level constructs.
                    | "interface_declaration"
                    | "enum_declaration"
                    | "type_alias_declaration"
            );

            if is_extractable {
                if let Ok(text) = node.utf8_text(content.as_bytes()) {
                    let full_text = match extract_leading_comments(node, content) {
                        Some(comment) => format!("{}\n{}", comment, text),
                        None => text.to_string(),
                    };
                    let token_count = bpe.encode_with_special_tokens(&full_text).len();

                    if token_count <= max_tokens {
                        let item_name = extract_js_item_name(node, content);
                        let item_type = match kind {
                            "class_declaration" => "class",
                            "method_definition" => "method",
                            "interface_declaration" => "interface",
                            "enum_declaration" => "enum",
                            "type_alias_declaration" => "type",
                            _ => "fn",
                        };
                        let new_context = if context_path.is_empty() {
                            format!("{}::{}", item_type, item_name)
                        } else {
                            format!("{}::{}::{}", context_path, item_type, item_name)
                        };

                        chunks.push(Chunk {
                            id: Uuid::new_v4(),
                            file_path: file_path.clone(),
                            chunk_index: *chunk_idx,
                            content: full_text,
                            token_count,
                            context: Some(new_context.clone()),
                        });
                        *chunk_idx += 1;

                        // For classes, visit methods
                        if kind == "class_declaration" {
                            let mut child_cursor = node.walk();
                            for child in node.children(&mut child_cursor) {
                                visit_js_node(
                                    child,
                                    content,
                                    file_path,
                                    bpe,
                                    max_tokens,
                                    chunks,
                                    chunk_idx,
                                    &new_context,
                                );
                            }
                        }
                        return;
                    }
                }
            }

            // Visit children
            let mut child_cursor = node.walk();
            for child in node.children(&mut child_cursor) {
                visit_js_node(
                    child,
                    content,
                    file_path,
                    bpe,
                    max_tokens,
                    chunks,
                    chunk_idx,
                    context_path,
                );
            }
        }

        visit_js_node(
            root_node,
            content,
            file_path,
            bpe,
            self.config.max_chunk_tokens,
            chunks,
            &mut chunk_idx,
            "",
        );
        Ok(())
    }

    /// Extract Go items (functions, methods, types)
    fn extract_go_items(
        &self,
        file_path: &PathBuf,
        content: &str,
        root_node: Node,
        bpe: &tiktoken_rs::CoreBPE,
        chunks: &mut Vec<Chunk>,
    ) -> Result<()> {
        let mut chunk_idx = 0;

        fn visit_go_node(
            node: Node,
            content: &str,
            file_path: &PathBuf,
            bpe: &tiktoken_rs::CoreBPE,
            max_tokens: usize,
            chunks: &mut Vec<Chunk>,
            chunk_idx: &mut usize,
            context_path: &str,
        ) {
            let kind = node.kind();

            let is_extractable = matches!(
                kind,
                "function_declaration"
                    | "method_declaration"
                    | "type_declaration"
                    | "const_declaration"
                    | "var_declaration"
            );

            if is_extractable {
                if let Ok(text) = node.utf8_text(content.as_bytes()) {
                    let full_text = match extract_leading_comments(node, content) {
                        Some(comment) => format!("{}\n{}", comment, text),
                        None => text.to_string(),
                    };
                    let token_count = bpe.encode_with_special_tokens(&full_text).len();

                    if token_count <= max_tokens {
                        let item_name = extract_go_item_name(node, content);
                        let item_type = match kind {
                            "type_declaration" => "type",
                            "method_declaration" => "method",
                            "const_declaration" => "const",
                            "var_declaration" => "var",
                            _ => "fn",
                        };
                        let new_context = if context_path.is_empty() {
                            format!("{}::{}", item_type, item_name)
                        } else {
                            format!("{}::{}::{}", context_path, item_type, item_name)
                        };

                        chunks.push(Chunk {
                            id: Uuid::new_v4(),
                            file_path: file_path.clone(),
                            chunk_index: *chunk_idx,
                            content: full_text,
                            token_count,
                            context: Some(new_context.clone()),
                        });
                        *chunk_idx += 1;

                        // For type declarations, visit methods
                        if kind == "type_declaration" {
                            let mut child_cursor = node.walk();
                            for child in node.children(&mut child_cursor) {
                                visit_go_node(
                                    child,
                                    content,
                                    file_path,
                                    bpe,
                                    max_tokens,
                                    chunks,
                                    chunk_idx,
                                    &new_context,
                                );
                            }
                        }
                        return;
                    }
                }
            }

            // Visit children
            let mut child_cursor = node.walk();
            for child in node.children(&mut child_cursor) {
                visit_go_node(
                    child,
                    content,
                    file_path,
                    bpe,
                    max_tokens,
                    chunks,
                    chunk_idx,
                    context_path,
                );
            }
        }

        visit_go_node(
            root_node,
            content,
            file_path,
            bpe,
            self.config.max_chunk_tokens,
            chunks,
            &mut chunk_idx,
            "",
        );
        Ok(())
    }

    // Traditional fallback chunking methods

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
                        &chunk_tokens
                            .iter()
                            .flat_map(|&t| bpe.decode(vec![t]).unwrap_or_default().into_bytes())
                            .collect::<Vec<u8>>(),
                    )
                    .to_string()
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
                        context: None,
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
                        context: None,
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
                    context: None,
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
                context: None,
            });
        }

        Ok(chunks)
    }

    pub fn chunk_file(&mut self, file_path: &PathBuf, content: &str) -> Result<Vec<Chunk>> {
        let bpe = &*BPE;

        if content.trim().is_empty() {
            return Ok(vec![]);
        }

        // Try tree-sitter first for code files if enabled
        if self.config.use_treesitter {
            let file_type = Self::determine_file_type(file_path);
            if file_type == FileType::Code {
                let language = Self::detect_code_language(file_path);
                if language != CodeLanguage::Unsupported {
                    if let Ok(Some(chunks)) =
                        self.chunk_with_treesitter(file_path, content, language)
                    {
                        debug!("Using tree-sitter chunks for {:?}", file_path);
                        return Ok(chunks);
                    }
                }
            }
        }

        // Fallback to traditional chunking
        debug!("Using traditional chunking for {:?}", file_path);
        let file_type = Self::determine_file_type(file_path);
        let raw_segments: Vec<&str> = match file_type {
            FileType::Code => self.split_code(content),
            FileType::Markdown => self.split_markdown(content),
            FileType::Prose => self.split_prose(content),
            FileType::Unknown => self.split_default(content),
        };

        let chunks = self.normalize_segments(file_path, raw_segments, &bpe)?;
        Ok(chunks)
    }
}

// Helper functions for extracting names from tree-sitter nodes

/// Read the text of a node's `name` field, if present.
fn field_name_text(node: Node, content: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(content.as_bytes()).ok())
        .map(|s| s.to_string())
}

fn extract_rust_item_name(node: Node, content: &str) -> String {
    // Most items expose a `name` field (function/struct/enum/trait/mod/const/static);
    // `impl` blocks have no name, so fall back to the `type` they implement.
    field_name_text(node, content)
        .or_else(|| {
            node.child_by_field_name("type")
                .and_then(|n| n.utf8_text(content.as_bytes()).ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "anonymous".to_string())
}

fn extract_python_item_name(node: Node, content: &str) -> String {
    // function_definition and class_definition both have a `name` field.
    field_name_text(node, content).unwrap_or_else(|| "anonymous".to_string())
}

fn extract_js_item_name(node: Node, content: &str) -> String {
    // Named declarations/methods have a `name` field; anonymous arrow/function
    // expressions don't (their name lives on the enclosing declarator).
    field_name_text(node, content).unwrap_or_else(|| "anonymous".to_string())
}

fn extract_go_item_name(node: Node, content: &str) -> String {
    // Go wraps named specs: `type Foo struct {...}` parses as
    // type_declaration > type_spec(name: Foo); const/var declarations wrap
    // const_spec/var_spec the same way (using the first name of a group).
    let spec_kinds: &[&str] = match node.kind() {
        "type_declaration" => &["type_spec", "type_alias"],
        "const_declaration" => &["const_spec"],
        "var_declaration" => &["var_spec"],
        _ => &[],
    };
    if !spec_kinds.is_empty() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if spec_kinds.contains(&child.kind()) {
                if let Some(name) = field_name_text(child, content) {
                    return name;
                }
            }
        }
    }

    // function_declaration (identifier) and method_declaration (field_identifier)
    // both expose a `name` field.
    field_name_text(node, content).unwrap_or_else(|| "anonymous".to_string())
}

fn extract_rust_leading(node: Node, content: &str) -> Option<String> {
    // Collect the contiguous run of `///` doc comments and `#[...]` attributes
    // immediately preceding the item, preserving source order. We walk the parent's
    // children in order and reset the accumulator on any node that breaks the run, so
    // an item only inherits the block directly above it — not the docs or attributes
    // of an earlier sibling in the same scope.
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let mut leading = Vec::new();

    for sibling in parent.children(&mut cursor) {
        if sibling.start_byte() >= node.start_byte() {
            break;
        }
        match sibling.kind() {
            "line_comment" => match sibling.utf8_text(content.as_bytes()) {
                Ok(text) if text.starts_with("///") => leading.push(text),
                // A non-doc comment (`//`, `//!`, ...) ends the run.
                _ => leading.clear(),
            },
            // Attributes sit between the doc block and the item; keep them and let
            // the run continue (a `///` above an attribute still belongs to the item).
            "attribute_item" => {
                if let Ok(text) = sibling.utf8_text(content.as_bytes()) {
                    leading.push(text);
                }
            }
            // Any other node (another item, etc.) ends the contiguous run.
            _ => leading.clear(),
        }
    }

    if leading.is_empty() {
        None
    } else {
        Some(leading.join("\n"))
    }
}

/// Collect the contiguous run of `comment` nodes immediately preceding `node`
/// within its parent. Mirrors `extract_rust_doc_comment` but generalizes to the
/// single `comment` node kind that the Python/JS/Go grammars use (JSDoc `/** */`
/// blocks, Go `//` doc lines, Python `#` comments). Any non-comment sibling
/// resets the run, so an item only inherits the comment block directly above it
/// — not the trailing comment of an earlier sibling.
fn extract_leading_comments(node: Node, content: &str) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let mut comment_lines: Vec<&str> = Vec::new();

    for sibling in parent.children(&mut cursor) {
        if sibling.start_byte() >= node.start_byte() {
            break;
        }
        if sibling.kind() == "comment" {
            if let Ok(text) = sibling.utf8_text(content.as_bytes()) {
                comment_lines.push(text);
            }
        } else {
            comment_lines.clear();
        }
    }

    if comment_lines.is_empty() {
        None
    } else {
        Some(comment_lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chunker() -> Chunker {
        Chunker::new(ChunkingConfig {
            max_chunk_tokens: 100,
            overlap_tokens: 10,
            max_file_size_mb: 10,
            use_treesitter: false,
            treesitter_languages: vec![],
            fallback_to_tokens: true,
        })
    }

    fn test_chunker_with_treesitter() -> Chunker {
        Chunker::new(ChunkingConfig {
            max_chunk_tokens: 512,
            overlap_tokens: 50,
            max_file_size_mb: 10,
            use_treesitter: true,
            treesitter_languages: vec!["rust".to_string(), "python".to_string()],
            fallback_to_tokens: true,
        })
    }

    #[test]
    fn test_file_type_detection() {
        assert_eq!(
            Chunker::determine_file_type(&PathBuf::from("foo.rs")),
            FileType::Code
        );
        assert_eq!(
            Chunker::determine_file_type(&PathBuf::from("bar.py")),
            FileType::Code
        );
        assert_eq!(
            Chunker::determine_file_type(&PathBuf::from("README.md")),
            FileType::Markdown
        );
        assert_eq!(
            Chunker::determine_file_type(&PathBuf::from("notes.txt")),
            FileType::Prose
        );
        assert_eq!(
            Chunker::determine_file_type(&PathBuf::from("data.json")),
            FileType::Unknown
        );
        assert_eq!(
            Chunker::determine_file_type(&PathBuf::from("no_extension")),
            FileType::Unknown
        );
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
    fn test_empty_content_returns_empty() {
        let mut chunker = test_chunker();
        let path = PathBuf::from("empty.txt");
        let chunks = chunker.chunk_file(&path, "").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_treesitter_rust_extraction() {
        let mut chunker = test_chunker_with_treesitter();
        let path = PathBuf::from("test.rs");
        let content = r#"
/// This is a doc comment
fn hello() {
    println!("hello");
}

fn world() {
    println!("world");
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        assert!(!chunks.is_empty(), "Should extract chunks");

        // Check that we got semantic chunks with context
        let has_context = chunks.iter().any(|c| c.context.is_some());
        assert!(has_context, "Tree-sitter chunks should have context");
    }

    #[test]
    fn test_treesitter_python_extraction() {
        let mut chunker = test_chunker_with_treesitter();
        let path = PathBuf::from("test.py");
        let content = r#"
def hello():
    print("hello")

class MyClass:
    def method(self):
        pass
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        assert!(!chunks.is_empty(), "Should extract chunks");

        let has_context = chunks.iter().any(|c| c.context.is_some());
        assert!(has_context, "Tree-sitter chunks should have context");
    }

    #[test]
    fn test_treesitter_go_extraction() {
        let mut chunker = Chunker::new(ChunkingConfig {
            max_chunk_tokens: 512,
            overlap_tokens: 50,
            max_file_size_mb: 10,
            use_treesitter: true,
            treesitter_languages: vec!["go".to_string()],
            fallback_to_tokens: true,
        });
        let path = PathBuf::from("test.go");
        let content = r#"
package main

func Hello() {
    println("hello")
}

type Person struct {
    Name string
    Age  int
}

func (p *Person) GetName() string {
    return p.Name
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        assert!(!chunks.is_empty(), "Should extract chunks");

        let has_context = chunks.iter().any(|c| c.context.is_some());
        assert!(has_context, "Tree-sitter chunks should have context");
    }

    #[test]
    fn test_rust_doc_comment_not_over_collected() {
        let mut chunker = test_chunker_with_treesitter();
        let path = PathBuf::from("docs.rs");
        // Two documented functions in the same scope: `beta` must only carry its
        // own doc comment, not accumulate `alpha`'s.
        let content = r#"
/// Doc for alpha.
fn alpha() {
    println!("a");
}

/// Doc for beta.
fn beta() {
    println!("b");
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();

        let beta = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.ends_with("beta"))
            })
            .expect("should have a chunk for beta");

        assert!(
            beta.content.contains("Doc for beta"),
            "beta should keep its own doc comment"
        );
        assert!(
            !beta.content.contains("Doc for alpha"),
            "beta must not inherit alpha's doc comment (over-collection regression)"
        );
    }

    #[test]
    fn test_rust_type_and_impl_names_resolved() {
        let mut chunker = test_chunker_with_treesitter();
        let path = PathBuf::from("named.rs");
        let content = r#"
pub struct Widget {
    size: usize,
}

impl Widget {
    fn new() -> Self {
        Widget { size: 0 }
    }
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();

        assert!(
            contexts.iter().any(|c| c.contains("struct::Widget")),
            "struct name should resolve to Widget, got: {:?}",
            contexts
        );
        assert!(
            contexts.iter().any(|c| c.contains("impl::Widget")),
            "impl block should resolve to its type Widget, got: {:?}",
            contexts
        );
        assert!(
            contexts.iter().all(|c| !c.contains("anonymous")),
            "no item should be anonymous, got: {:?}",
            contexts
        );
    }

    fn treesitter_chunker(langs: &[&str]) -> Chunker {
        Chunker::new(ChunkingConfig {
            max_chunk_tokens: 512,
            overlap_tokens: 50,
            max_file_size_mb: 10,
            use_treesitter: true,
            treesitter_languages: langs.iter().map(|s| s.to_string()).collect(),
            fallback_to_tokens: true,
        })
    }

    #[test]
    fn test_python_chunk_includes_leading_comment() {
        let mut chunker = treesitter_chunker(&["python"]);
        let path = PathBuf::from("commented.py");
        let content = r#"
# Greets the caller warmly.
def greet():
    print("hi")
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let greet = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.ends_with("greet"))
            })
            .expect("should have a chunk for greet");
        assert!(
            greet.content.contains("Greets the caller warmly"),
            "python chunk should carry its leading comment, got: {:?}",
            greet.content
        );
    }

    #[test]
    fn test_python_chunk_includes_decorator() {
        let mut chunker = treesitter_chunker(&["python"]);
        let path = PathBuf::from("decorated.py");
        let content = r#"
@cached
def build():
    return 1
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let build = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.ends_with("build"))
            })
            .expect("should have a chunk for build");
        assert!(
            build.content.contains("@cached"),
            "python chunk should carry its decorator, got: {:?}",
            build.content
        );
    }

    #[test]
    fn test_javascript_chunk_includes_jsdoc() {
        let mut chunker = treesitter_chunker(&["javascript"]);
        let path = PathBuf::from("doc.js");
        let content = r#"
/** Adds two numbers. */
function add(a, b) {
    return a + b;
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let add = chunks
            .iter()
            .find(|c| c.context.as_deref().is_some_and(|ctx| ctx.ends_with("add")))
            .expect("should have a chunk for add");
        assert!(
            add.content.contains("Adds two numbers"),
            "js chunk should carry its jsdoc, got: {:?}",
            add.content
        );
    }

    #[test]
    fn test_go_chunk_includes_doc_comment() {
        let mut chunker = treesitter_chunker(&["go"]);
        let path = PathBuf::from("doc.go");
        let content = r#"
package main

// Hello prints a greeting.
func Hello() {
    println("hello")
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let hello = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.ends_with("Hello"))
            })
            .expect("should have a chunk for Hello");
        assert!(
            hello.content.contains("Hello prints a greeting"),
            "go chunk should carry its doc comment, got: {:?}",
            hello.content
        );
    }

    #[test]
    fn test_go_doc_comment_not_over_collected() {
        let mut chunker = treesitter_chunker(&["go"]);
        let path = PathBuf::from("multi.go");
        let content = r#"
package main

// Alpha does the first thing.
func Alpha() {
    println("a")
}

// Beta does the second thing.
func Beta() {
    println("b")
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let beta = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.ends_with("Beta"))
            })
            .expect("should have a chunk for Beta");
        assert!(
            beta.content.contains("Beta does the second thing"),
            "Beta should keep its own doc comment, got: {:?}",
            beta.content
        );
        assert!(
            !beta.content.contains("Alpha does the first thing"),
            "Beta must not inherit Alpha's doc comment (over-collection), got: {:?}",
            beta.content
        );
    }

    #[test]
    fn test_rust_chunk_includes_attributes() {
        let mut chunker = treesitter_chunker(&["rust"]);
        let path = PathBuf::from("attrs.rs");
        let content = r#"
#[derive(Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    name: String,
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let config = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.ends_with("Config"))
            })
            .expect("should have a chunk for Config");
        assert!(
            config.content.contains("#[derive(Debug, Clone)]"),
            "rust chunk should carry its derive attribute, got: {:?}",
            config.content
        );
        assert!(
            config.content.contains("#[serde"),
            "rust chunk should carry its serde attribute, got: {:?}",
            config.content
        );
    }

    #[test]
    fn test_typescript_chunk_extracts_enum() {
        let mut chunker = treesitter_chunker(&["typescript"]);
        let path = PathBuf::from("color.ts");
        let content = r#"
enum Color {
    Red,
    Green,
    Blue,
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let color = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.contains("enum::Color"))
            })
            .expect("should extract a semantic chunk for enum Color");
        assert!(
            color.content.contains("enum Color"),
            "enum chunk should carry its body, got: {:?}",
            color.content
        );
    }

    #[test]
    fn test_typescript_chunk_extracts_interface() {
        let mut chunker = treesitter_chunker(&["typescript"]);
        let path = PathBuf::from("user.ts");
        let content = r#"
interface User {
    name: string;
    age: number;
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let user = chunks
            .iter()
            .find(|c| {
                c.context
                    .as_deref()
                    .is_some_and(|ctx| ctx.contains("interface::User"))
            })
            .expect("should extract a semantic chunk for interface User");
        assert!(
            user.content.contains("interface User"),
            "interface chunk should carry its body, got: {:?}",
            user.content
        );
    }

    #[test]
    fn test_go_chunk_extracts_const_and_var() {
        let mut chunker = treesitter_chunker(&["go"]);
        let path = PathBuf::from("decls.go");
        let content = r#"
package main

const MaxSize = 100

var GlobalName = "test"
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        assert!(
            contexts.iter().any(|c| c.contains("MaxSize")),
            "should extract a chunk for the const, got: {:?}",
            contexts
        );
        assert!(
            contexts.iter().any(|c| c.contains("GlobalName")),
            "should extract a chunk for the var, got: {:?}",
            contexts
        );
    }

    // ---- Ported characterization coverage (adapted from the feature branch) ----

    #[test]
    fn test_code_no_blank_lines_single_segment() {
        let chunker = test_chunker();
        let segments = chunker.split_code("fn foo() {\n    1\n}");
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn test_markdown_splitting_on_headers() {
        let chunker = test_chunker();
        let content = "# Title\n\nIntro paragraph.\n\n## Section\n\nSection content.";
        let segments = chunker.split_markdown(content);
        assert!(segments.len() >= 2, "got {} segments", segments.len());
    }

    #[test]
    fn test_prose_splitting_on_paragraphs() {
        let chunker = test_chunker();
        let content = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let segments = chunker.split_prose(content);
        assert_eq!(segments.len(), 3);
    }

    #[test]
    fn test_whitespace_only_returns_empty() {
        let mut chunker = test_chunker();
        let path = PathBuf::from("whitespace.txt");
        let chunks = chunker.chunk_file(&path, "   \n\n  \t  ").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_indices_are_sequential() {
        let mut chunker = test_chunker();
        let path = PathBuf::from("seq.rs");
        let content = "fn one() {}\n\nfn two() {}\n\nfn three() {}";
        let chunks = chunker.chunk_file(&path, content).unwrap();
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn test_chunk_ids_are_unique() {
        let mut chunker = test_chunker();
        let path = PathBuf::from("ids.md");
        let content = "# One\n\nParagraph.\n\n# Two\n\nAnother.";
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let ids: std::collections::HashSet<_> = chunks.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), chunks.len(), "all chunk IDs should be unique");
    }

    #[test]
    fn test_large_content_splits_by_tokens() {
        let mut chunker = test_chunker(); // max_chunk_tokens = 100
        let path = PathBuf::from("big.txt");
        let content = "word ".repeat(200);
        let chunks = chunker.chunk_file(&path, &content).unwrap();
        assert!(
            chunks.len() > 1,
            "large content should split into many chunks"
        );
        for chunk in &chunks {
            assert!(
                chunk.token_count <= 100,
                "each chunk should respect max_tokens, got {}",
                chunk.token_count
            );
        }
    }

    #[test]
    fn test_rust_extracts_all_item_types() {
        let mut chunker = treesitter_chunker(&["rust"]);
        let path = PathBuf::from("all.rs");
        let content = r#"
const MAX_SIZE: usize = 100;

struct Point {
    x: i32,
    y: i32,
}

enum Color {
    Red,
    Green,
}

trait Drawable {
    fn draw(&self);
}

impl Drawable for Point {
    fn draw(&self) {}
}

fn main() {}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        for expected in [
            "const::MAX_SIZE",
            "struct::Point",
            "enum::Color",
            "trait::Drawable",
            "impl::Point",
            "function::main",
        ] {
            assert!(
                contexts.iter().any(|c| c.contains(expected)),
                "expected a chunk with context containing {expected:?}, got: {contexts:?}"
            );
        }
    }

    #[test]
    fn test_rust_no_blank_lines_still_splits() {
        let mut chunker = treesitter_chunker(&["rust"]);
        let path = PathBuf::from("dense.rs");
        let content = "fn first() { 1 }\nfn second() { 2 }\nfn third() { 3 }";
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        for name in ["function::first", "function::second", "function::third"] {
            assert!(
                contexts.iter().any(|c| c.contains(name)),
                "AST chunking should split adjacent items; missing {name:?} in {contexts:?}"
            );
        }
    }

    #[test]
    fn test_rust_fallback_on_invalid() {
        let mut chunker = treesitter_chunker(&["rust"]);
        let path = PathBuf::from("invalid.rs");
        let content = "this is not valid rust code at all\n\nmore invalid stuff";
        let chunks = chunker.chunk_file(&path, content).unwrap();
        assert!(
            !chunks.is_empty(),
            "unparseable code should still yield fallback chunks"
        );
    }

    #[test]
    fn test_go_extracts_all_item_types() {
        let mut chunker = treesitter_chunker(&["go"]);
        let path = PathBuf::from("all.go");
        let content = r#"
package main

const MaxSize = 100

var GlobalVar = "test"

type Point struct {
    X int
}

func (p Point) Draw() {}

func main() {}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        for expected in [
            "const::MaxSize",
            "var::GlobalVar",
            "type::Point",
            "method::Draw",
            "fn::main",
        ] {
            assert!(
                contexts.iter().any(|c| c.contains(expected)),
                "expected a chunk with context containing {expected:?}, got: {contexts:?}"
            );
        }
    }

    #[test]
    fn test_python_extracts_class_and_function() {
        let mut chunker = treesitter_chunker(&["python"]);
        let path = PathBuf::from("shapes.py");
        let content = r#"
class Point:
    def draw(self):
        print(self.x)

def main():
    pass
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        assert!(
            contexts.iter().any(|c| c.contains("class::Point")),
            "expected a class chunk, got: {contexts:?}"
        );
        assert!(
            contexts.iter().any(|c| c.contains("fn::main")),
            "expected a function chunk, got: {contexts:?}"
        );
    }

    #[test]
    fn test_javascript_extracts_class_and_function() {
        let mut chunker = treesitter_chunker(&["javascript"]);
        let path = PathBuf::from("shapes.js");
        let content = r#"
class Point {
    constructor(x) {
        this.x = x;
    }
}

function main() {
    return new Point(0);
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        assert!(
            contexts.iter().any(|c| c.contains("class::Point")),
            "expected a class chunk, got: {contexts:?}"
        );
        assert!(
            contexts.iter().any(|c| c.contains("fn::main")),
            "expected a function chunk, got: {contexts:?}"
        );
    }

    #[test]
    fn test_typescript_parsing_interface_type_and_function() {
        let mut chunker = treesitter_chunker(&["typescript"]);
        let path = PathBuf::from("types.ts");
        let content = r#"
interface User {
    name: string;
}

type ID = string | number;

function greet(user: User): void {
    console.log(user.name);
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        for expected in ["interface::User", "type::ID", "fn::greet"] {
            assert!(
                contexts.iter().any(|c| c.contains(expected)),
                "expected a chunk with context containing {expected:?}, got: {contexts:?}"
            );
        }
    }

    #[test]
    fn test_chunk_file_routes_by_extension() {
        let mut chunker = treesitter_chunker(&["go", "python", "javascript", "typescript"]);
        let cases = [
            ("test.go", "package main\n\nfunc foo() {}\n\nfunc bar() {}"),
            ("test.py", "def foo():\n    pass\n\ndef bar():\n    pass"),
            ("test.js", "function foo() {}\n\nfunction bar() {}"),
            (
                "test.ts",
                "function foo(): void {}\n\nfunction bar(): void {}",
            ),
        ];
        for (name, content) in cases {
            let chunks = chunker.chunk_file(&PathBuf::from(name), content).unwrap();
            assert!(!chunks.is_empty(), "{name} should produce chunks");
            assert!(
                chunks.iter().any(|c| c.context.is_some()),
                "{name} should produce at least one semantic chunk"
            );
        }
    }

    #[test]
    fn test_tsx_chunk_extracts_component() {
        // Enabling "typescript" should also parse `.tsx` (JSX-bearing) files.
        let mut chunker = treesitter_chunker(&["typescript"]);
        let path = PathBuf::from("Greeting.tsx");
        let content = r#"
interface Props {
    name: string;
}

function Greeting({ name }: Props) {
    return <div>Hello, {name}!</div>;
}
"#;
        let chunks = chunker.chunk_file(&path, content).unwrap();
        let contexts: Vec<String> = chunks.iter().filter_map(|c| c.context.clone()).collect();
        assert!(
            contexts.iter().any(|c| c.contains("fn::Greeting")),
            "TSX component should be extracted as a semantic chunk, got: {contexts:?}"
        );
        assert!(
            contexts.iter().any(|c| c.contains("interface::Props")),
            "TSX interface should be extracted, got: {contexts:?}"
        );
    }
}
