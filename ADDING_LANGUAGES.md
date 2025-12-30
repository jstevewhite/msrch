# Adding New Languages to Tree-Sitter Chunker

This guide shows how to add support for new programming languages to the semantic chunker.

## Currently Supported Languages

- **Rust** (.rs) - functions, structs, enums, traits, impl blocks, modules
- **Python** (.py) - functions, classes, methods
- **JavaScript** (.js, .jsx) - functions, classes, methods, arrow functions
- **TypeScript** (.ts, .tsx) - same as JavaScript
- **Go** (.go) - functions, methods, type declarations

## How to Add a New Language

### Step 1: Add the Tree-Sitter Dependency

```bash
cargo add tree-sitter-<language>
```

Example for Go:
```bash
cargo add tree-sitter-go
```

### Step 2: Update `src/chunker.rs`

#### 2.1 Add to CodeLanguage enum:

```rust
#[derive(Debug, Clone, PartialEq)]
enum CodeLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,          // <-- Add here
    Unsupported,
}
```

#### 2.2 Add parser field to Chunker struct:

```rust
pub struct Chunker {
    config: ChunkingConfig,
    rust_parser: Option<Parser>,
    python_parser: Option<Parser>,
    javascript_parser: Option<Parser>,
    typescript_parser: Option<Parser>,
    go_parser: Option<Parser>,  // <-- Add here
}
```

#### 2.3 Initialize parser field in `new()`:

```rust
pub fn new(config: ChunkingConfig) -> Self {
    let mut chunker = Self {
        config,
        rust_parser: None,
        python_parser: None,
        javascript_parser: None,
        typescript_parser: None,
        go_parser: None,  // <-- Add here
    };
    // ...
}
```

#### 2.4 Add initialization in `init_parsers()`:

```rust
fn init_parsers(&mut self) {
    for lang in &self.config.treesitter_languages {
        match lang.as_str() {
            // ... other languages ...
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
            // ...
        }
    }
}
```

#### 2.5 Add file extension detection in `detect_code_language()`:

```rust
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
        "ts" | "tsx" => CodeLanguage::TypeScript,
        "go" => CodeLanguage::Go,  // <-- Add here
        _ => CodeLanguage::Unsupported,
    }
}
```

#### 2.6 Add parser selection in `chunk_with_treesitter()`:

```rust
let parser = match language {
    CodeLanguage::Rust => self.rust_parser.as_mut(),
    CodeLanguage::Python => self.python_parser.as_mut(),
    CodeLanguage::JavaScript => self.javascript_parser.as_mut(),
    CodeLanguage::TypeScript => self.typescript_parser.as_mut(),
    CodeLanguage::Go => self.go_parser.as_mut(),  // <-- Add here
    CodeLanguage::Unsupported => return Ok(None),
};
```

#### 2.7 Add extraction dispatch in `chunk_with_treesitter()`:

```rust
match language {
    CodeLanguage::Rust => {
        self.extract_rust_items(file_path, content, root_node, &bpe, &mut chunks)?
    }
    CodeLanguage::Python => {
        self.extract_python_items(file_path, content, root_node, &bpe, &mut chunks)?
    }
    CodeLanguage::JavaScript | CodeLanguage::TypeScript => {
        self.extract_js_items(file_path, content, root_node, &bpe, &mut chunks)?
    }
    CodeLanguage::Go => {  // <-- Add here
        self.extract_go_items(file_path, content, root_node, &bpe, &mut chunks)?
    }
    CodeLanguage::Unsupported => return Ok(None),
}
```

#### 2.8 Implement extraction function:

```rust
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

        // Define what node types to extract
        let is_extractable = matches!(
            kind,
            "function_declaration" | "method_declaration" | "type_declaration"
        );

        if is_extractable {
            if let Ok(text) = node.utf8_text(content.as_bytes()) {
                let token_count = bpe.encode_with_special_tokens(text).len();

                if token_count <= max_tokens {
                    let item_name = extract_go_item_name(node, content);
                    let item_type = match kind {
                        "type_declaration" => "type",
                        "method_declaration" => "method",
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
                        content: text.to_string(),
                        token_count,
                        context: Some(new_context.clone()),
                    });
                    *chunk_idx += 1;

                    // Visit children for nested items
                    if kind == "type_declaration" {
                        let mut child_cursor = node.walk();
                        for child in node.children(&mut child_cursor) {
                            visit_go_node(child, content, file_path, bpe, max_tokens, chunks, chunk_idx, &new_context);
                        }
                    }
                    return;
                }
            }
        }

        // Visit children
        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            visit_go_node(child, content, file_path, bpe, max_tokens, chunks, chunk_idx, context_path);
        }
    }

    visit_go_node(root_node, content, file_path, bpe, self.config.max_chunk_tokens, chunks, &mut chunk_idx, "");
    Ok(())
}
```

#### 2.9 Add name extraction helper:

```rust
fn extract_go_item_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "field_identifier" || child.kind() == "type_identifier" {
            if let Ok(name) = child.utf8_text(content.as_bytes()) {
                return name.to_string();
            }
        }
    }
    "anonymous".to_string()
}
```

#### 2.10 Add test:

```rust
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
```

### Step 3: Update `src/config.rs`

Add the language to the default config:

```rust
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
                "go".to_string(),  // <-- Add here
            ],
            fallback_to_tokens: true,
        }
    }
}
```

### Step 4: Test

```bash
cargo test
```

## Finding the Right Node Types

To find which node types to extract for a language, you can:

1. Check the tree-sitter grammar documentation
2. Use tree-sitter CLI to inspect AST:
   ```bash
   tree-sitter parse <file>
   ```

Common node types:
- **Functions**: `function_declaration`, `function_definition`, `method_declaration`
- **Classes**: `class_declaration`, `class_definition`, `type_declaration`
- **Interfaces**: `interface_declaration`
- **Structs**: `struct_item`, `struct_declaration`
- **Enums**: `enum_item`, `enum_declaration`

## Tips

1. **Start simple**: Just extract functions first, then add classes/types
2. **Test incrementally**: Add a test after each extraction type
3. **Check node names**: Identifier fields vary by language (identifier, type_identifier, field_identifier, etc.)
4. **Consider nesting**: Some languages allow nested functions/classes - handle with context paths
5. **Skip oversized items**: The token count check ensures fallback chunker handles huge functions

## Example Languages to Add

Easy additions (tree-sitter crates available):
- C/C++ (`tree-sitter-c`, `tree-sitter-cpp`)
- Java (`tree-sitter-java`)
- Ruby (`tree-sitter-ruby`)
- Swift (`tree-sitter-swift`)
- Kotlin (`tree-sitter-kotlin`)
- C# (`tree-sitter-c-sharp`)
