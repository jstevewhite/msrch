# AGENTS.md - Agent Guidelines for msrch

This file provides coding guidelines and build instructions for AI agents working on msrch,
a semantic code search CLI tool written in Rust.

## Project Overview

msrch is a local-first semantic search tool that indexes code directories using embeddings
and provides fast semantic queries. Built with Rust 2024 edition.

**Key Dependencies:** LanceDB (vector storage), reqwest (HTTP), tokio (async), clap (CLI)

## Build & Test Commands

### Building
```bash
# Debug build (fast compile, slower runtime)
cargo build

# Release build (optimized for production)
cargo build --release
make build

# Check code without building
cargo check

# Install to ~/.cargo/bin
make install
```

### Testing
```bash
# Run all tests
cargo test
make test

# Run a single test by name
cargo test test_name

# Run tests in a specific module
cargo test module_name::

# Run tests with output visible
cargo test -- --nocapture

# Run tests in parallel (default) or single-threaded
cargo test -- --test-threads=1
```

### Linting & Formatting
```bash
# Format code (automatically applies rustfmt)
cargo fmt

# Check formatting without modifying files
cargo fmt -- --check

# Run clippy linter
cargo clippy

# Run clippy with pedantic warnings
cargo clippy -- -W clippy::pedantic
```

### Running
```bash
# Run in debug mode
cargo run -- <args>
make run

# Run specific commands
cargo run -- index .
cargo run -- query "search text" --limit 5
```

### Cleaning
```bash
cargo clean
make clean
```

## Code Style Guidelines

### Module Organization
- Modules declared at top of main.rs: `mod config;`, `mod crawler;`, etc.
- Each module in separate file: `src/module_name.rs`
- Use flat module structure (avoid deep nesting)

### Imports
- Group imports by category: stdlib, external crates, internal modules
- Use `use crate::` for internal module imports
- Example order:
  ```rust
  use std::path::PathBuf;
  use std::env;
  
  use anyhow::{Context, Result};
  use serde::{Deserialize, Serialize};
  
  use crate::config::Config;
  use crate::db::VectorDB;
  ```

### Error Handling
- Use `anyhow::Result<T>` for public function return types
- Use `.context("description")` to add context to errors
- Use `anyhow::bail!("message")` for explicit errors
- Propagate errors with `?` operator
- Example:
  ```rust
  pub async fn load_data() -> Result<Data> {
      let content = std::fs::read_to_string(path)
          .context("Failed to read file")?;
      Ok(parse(content)?)
  }
  ```

### Type Definitions
- Use struct for grouping related data
- Derive traits in consistent order: `Debug`, `Clone`, `Serialize`, `Deserialize`
- Implement `Default` trait where appropriate
- Use type aliases for complex types when they improve readability
- Example:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct Config {
      pub field: String,
  }
  ```

### Naming Conventions
- **Files/Modules:** snake_case (e.g., `embedding.rs`, `vector_db.rs`)
- **Structs/Enums:** PascalCase (e.g., `EmbeddingClient`, `Commands`)
- **Functions/Variables:** snake_case (e.g., `load_config`, `chunk_index`)
- **Constants:** SCREAMING_SNAKE_CASE (e.g., `MAX_CHUNK_SIZE`)
- **Lifetimes:** short lowercase (e.g., `'a`, `'b`)

### Async/Await
- Use `async fn` for functions that perform I/O or call async dependencies
- Mark main with `#[tokio::main]`
- Prefer `async/await` syntax over manual Future combinators
- Use `.await?` for error propagation in async contexts

### Comments & Documentation
- Use `///` for public API documentation
- Use `//` for implementation comments
- Document public structs, enums, and functions
- Keep comments concise and explain "why" not "what"
- Example:
  ```rust
  /// Searches the vector database for similar content.
  /// 
  /// Returns scored results sorted by similarity.
  pub async fn search(&self, query: &str) -> Result<Vec<ScoredPoint>> {
      // Load config from index or use global defaults
      let config = Config::load_global_config().unwrap_or_default();
      // ...
  }
  ```

### Configuration
- Use `serde` for serialization/deserialization
- Provide sensible `Default` implementations
- Use `confy` for loading global config from OS-specific paths
- Support both global config and per-project overrides
- Configuration files use TOML format

### CLI Design
- Use `clap` with derive macros for argument parsing
- Use subcommands for distinct operations (`index`, `query`, `config`)
- Provide helpful descriptions with `#[command(about = "...")]`
- Use `PathBuf` for file/directory arguments

### Output & Display
- Use `colored` crate for terminal colors
- Format scores with 2 decimal places: `format!("{:.2}", score)`
- Use `.bold()`, `.cyan()`, `.yellow()` for emphasis
- Print errors to stderr with `eprintln!`
- Regular output to stdout with `println!`

### Testing
- Place unit tests in the same file as the code: `#[cfg(test)] mod tests { ... }`
- Use integration tests in `tests/` directory for end-to-end scenarios
- Name tests descriptively: `test_embedding_client_basic_request`
- Use `assert!`, `assert_eq!`, and `assert_ne!` for assertions

## Project-Specific Patterns

### Vector Database (LanceDB)
- Initialize collections with proper schema and dimensions
- Use Arrow RecordBatch for bulk inserts
- Convert distance scores to similarity: `1.0 - distance`
- Handle async operations with proper error context

### Embeddings
- Batch API requests using configured batch_size
- Sort results by index to preserve input order
- Include retry logic and timeout handling
- Support optional bearer token authentication

### Chunking
- Use tiktoken for token counting
- Apply overlap between chunks for context preservation
- Respect max_chunk_tokens from configuration

### Indexing
- Honor .gitignore patterns automatically
- Track file modification times for incremental updates
- Skip binary files unless explicitly configured
- Store metadata in JSON manifest for inspection

## Common Pitfalls to Avoid

- Don't use `unwrap()` in production code; prefer `?` or `unwrap_or_default()`
- Don't block async runtime with synchronous I/O operations
- Don't hardcode paths; use configuration or detect at runtime
- Don't forget to add `.context()` when propagating errors
- Don't mix tabs and spaces (use rustfmt)

## Additional Resources

- High-Level Design: `msrch_HLD.md`
- Dependencies: `Cargo.toml`
- Build automation: `Makefile`
