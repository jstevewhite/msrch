# AGENTS.md - Agent Guidelines for msrch

This file provides coding guidelines and build instructions for AI agents working on msrch,
a local-first semantic search CLI tool in Rust 2024.

## Build & Test Commands

```bash
# Building
cargo build                 # Debug build
cargo build --release       # Release build (make build)
cargo check                 # Check without building

# Testing
cargo test                  # Run all tests (make test)
cargo test test_name         # Run single test by name
cargo test module_name::     # Run tests in module
cargo test -- --nocapture   # Show test output

# Linting & Formatting
cargo fmt                   # Format code
cargo fmt -- --check        # Check formatting
cargo clippy                # Run linter
cargo clippy -- -W clippy::pedantic  # Pedantic warnings

# Running
cargo run -- <args>         # Debug mode
make run                    # Same as above
```

## Code Style Guidelines

### Module Organization
- Declare modules at top of main.rs: `mod config; mod crawler;`
- One module per file: `src/module_name.rs`
- Use flat structure, avoid deep nesting

### Imports
Group by category: stdlib → external crates → internal modules
```rust
use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use crate::config::Config;
```

### Error Handling
- Use `anyhow::Result<T>` for public functions
- Add context: `.context("description")?`
- Use `anyhow::bail!("msg")` for explicit errors
```rust
pub async fn load_data() -> Result<Data> {
    let content = std::fs::read_to_string(path)
        .context("Failed to read file")?;
    Ok(parse(content)?)
}
```

### Type Definitions
- Derive traits: `Debug`, `Clone`, `Serialize`, `Deserialize`
- Implement `Default` where appropriate
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config { pub field: String }
```

### Naming Conventions
- Files/Modules: `snake_case.rs` (e.g., `embedding.rs`)
- Structs/Enums: `PascalCase` (e.g., `EmbeddingClient`)
- Functions/Variables: `snake_case` (e.g., `load_config`)
- Constants: `SCREAMING_SNAKE_CASE` (e.g., `MAX_CHUNK_SIZE`)

### Async/Await
- Use `async fn` for I/O operations
- Mark main with `#[tokio::main]`
- Use `.await?` for error propagation

### CLI Design
- Use `clap` derive macros for argument parsing
- Subcommands for distinct operations (`index`, `query`, `config`)
- Use `PathBuf` for file/directory arguments

### Output
- Use `colored` crate for terminal output
- Errors to stderr: `eprintln!`
- Regular output to stdout: `println!`

## Project-Specific Patterns

### Configuration
- Use `serde` + TOML for config files
- Load global config with `confy::load("msrch", "config")`
- Support per-project overrides

### Vector Database (LanceDB)
- Initialize collections with proper schema
- Use Arrow RecordBatch for bulk inserts
- Convert distance to similarity: `1.0 - distance`

### Embeddings
- Batch requests using configured `batch_size`
- Sort results by index to preserve order
- Handle timeouts and retries

### Chunking
- Use `tiktoken-rs::cl100k_base()` for token counting
- Apply overlap between chunks for context
- Respect `max_chunk_tokens` from config

### Indexing
- Honor .gitignore patterns via `ignore` crate
- Track file modification times for incremental updates
- Skip binary files by default

## Common Pitfalls

- Don't use `unwrap()` in production; prefer `?` or `unwrap_or_default()`
- Don't block async runtime with sync I/O
- Don't hardcode paths; use config or runtime detection
- Don't forget `.context()` when propagating errors
- Don't mix tabs/spaces (use `cargo fmt`)

## Resources

- HLD: `msrch_HLD.md` | Dependencies: `Cargo.toml` | Build: `Makefile`
