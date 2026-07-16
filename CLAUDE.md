# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**msrch** is a local-first semantic search CLI tool written in Rust 2024. It creates per-directory indexes using embeddings from OpenAI-compatible APIs and provides fast semantic search over codebases using vector similarity.

## Essential Commands

### Build & Run
```bash
cargo build                  # Debug build
cargo build --release        # Release build
make build                   # Same as cargo build --release
cargo run -- <args>          # Run in debug mode with arguments
make install                 # Install to ~/.cargo/bin
```

### Testing
```bash
cargo test                   # Run all tests
cargo test test_name         # Run specific test
cargo test module_name::     # Run tests in a module
cargo test -- --nocapture    # Show test output
```

### Linting & Formatting
```bash
cargo fmt                    # Format code
cargo clippy                 # Run linter
cargo clippy -- -W clippy::pedantic  # Pedantic mode
```

### Development Testing
```bash
# Create index
cargo run -- index .

# Query with debug logging
cargo run -- index . --debug
cargo run -- "search query"
cargo run -- query "search query" --limit 5 --rerank
cargo run -- query "search query" --path docs/ --after 7d
```

## Architecture Overview

### Core Data Flow

**Indexing Pipeline:**
1. **Crawler** (`crawler.rs`) - Walks directory tree, respects `.gitignore`/`.msrchignore`, filters binary files
2. **Extractor** (`extract.rs`) - Converts HTML/PDF/docx to plain text before chunking (markdown-ish for HTML/docx, prose for PDF); skips graphics-only PDFs and oversize files
3. **Chunker** (`chunker.rs`) - Splits files into token-sized chunks using tiktoken, with overlap for context
4. **EmbeddingClient** (`embedding.rs`) - Batches chunks and calls OpenAI-compatible embedding API
5. **VectorDB** (`db.rs`) - Stores embeddings in LanceDB (currently flat/brute-force scan)
6. **Manifest** (`index.rs`) - Tracks file modification times for incremental reindexing

**Query Pipeline:**
1. **Index Discovery** - Walks up directory tree to find `.msrch/` (like git finding `.git/`)
2. **Query Embedding** - Embeds search text using same model as indexing
3. **Vector Search** - LanceDB flat similarity search (cosine distance converted to similarity: `1.0 - distance`)
4. **Optional Reranking** (`reranker.rs`) - Cross-encoder reranking for precision (slower but more accurate)
5. **Result Formatting** - Plain/Context/JSON output modes

### Key Modules

- **`main.rs`** - CLI parsing with clap, command dispatch, implicit query support (`msrch "text"`)
- **`config.rs`** - Configuration types and loading: XDG-aware global config (one-time migration from the legacy confy location) + project-level overlays
- **`index.rs`** - Indexing orchestration, incremental updates, manifest management
- **`search.rs`** - Query execution, index discovery (walk-up pattern), output formatting; `SearchOptions` request struct (path/date filters)
- **`db.rs`** - LanceDB wrapper using Arrow RecordBatch for bulk operations
- **`embedding.rs`** - HTTP client for OpenAI-compatible embedding endpoints
- **`reranker.rs`** - HTTP client for reranking endpoints
- **`chunker.rs`** - Token-based text chunking with tiktoken
- **`crawler.rs`** - File discovery with ignore pattern support
- **`extract.rs`** - Text extraction for document formats (HTML via readability + fallback, text-layer PDF, docx); owns all format knowledge

### Vector Database (LanceDB)

- **Storage:** `.msrch/index.db/` directory with LanceDB files
- **Schema:** `id` (UUID as string), `vector` (FixedSizeList\<Float32\>), `file_path`, `chunk_index`, `content`, `context` (semantic path from tree-sitter, e.g. `impl::Foo::fn::bar`)
- **Index:** Flat scanning (no ANN index built yet)
- **Operations:** Append chunks (delete-by-id then add), search with `min_similarity` threshold filtering, delete by ID list
- **Distance Metric:** Cosine distance (converted to similarity: `score = 1.0 - distance`)
- **Schema versioning:** `manifest.json` carries a `version` (see `SCHEMA_VERSION` in `index.rs`). On `index`/`reindex`, a version mismatch (or a pre-versioning manifest) wipes `index.db` and rebuilds so the table is recreated with the current schema.

### Incremental Reindexing Strategy

The indexer maintains a manifest at `.msrch/manifest.json` that tracks:
- File paths mapped to modification times and chunk IDs
- On reindex: compares current mtime vs manifest mtime
- Only re-embeds files with changed mtime
- Deletes stale chunks from vector DB before re-embedding modified files
- Removes chunks for deleted files

**Critical detail:** Must delete old chunks before upserting new ones to avoid orphaned vectors.

### Index Discovery Pattern

msrch implements git-like behavior for finding indexes:
1. Start at `cwd`
2. Check for `.msrch/` subdirectory
3. If not found, move to parent directory
4. Repeat until found or reach filesystem root
5. Works from any subdirectory within an indexed project tree

Implementation is in `search.rs::find_index_root()`.

## Configuration System

### Config Hierarchy (high to low precedence)
1. CLI flags: `--limit`, `--min-similarity`, `--rerank`, `--path`, `--after`/`--before`, etc.
2. Project config: `.msrch/config.toml` in the index root (field-by-field overlay)
3. Global User config: `~/.config/msrch/config.toml` ($XDG_CONFIG_HOME-aware)
4. Hardcoded defaults in `config.rs::Default` implementations

### Default Embedding Endpoint
- Default: `http://r7.home.lab:7997/embeddings`
- Default model: `mixedbread-ai/mxbai-embed-large-v1`
- Set in `config.rs::EmbeddingConfig::default()`

### Config Loading
- Global: `~/.config/msrch/config.toml` (XDG-aware; one-time copy migration from the legacy macOS confy path; missing file → defaults, no auto-creation)
- Project: merged via Config::load_for_index(index_root) — global config overlaid with .msrch/config.toml (project wins field-by-field; malformed project file warns and is ignored)
- Note: retries are pending implementation; `max_file_size_mb` is enforced for extractable document types only (see extract.rs)

## Versioning & Releases

- SemVer 0.x, single source of truth: `workspace.package.version` in the root
  `Cargo.toml` (both crates inherit it).
- Bump **minor** for features, roadmap items, or anything affecting index
  compatibility (`SCHEMA_VERSION` bumps); **patch** for fixes. Bump per
  meaningful merge to main, not per commit.
- On every bump: update `CHANGELOG.md`, commit, then `git tag vX.Y.Z` on that
  commit (`git push --tags` to publish).
- `msrch --version` prints `<semver> (index schema vN, commit <hash>)`. The
  hash is embedded by `crates/cli/build.rs` at compile time (`unknown` outside
  a git checkout, `-dirty` suffix when the tree has uncommitted changes) —
  build release binaries from a clean, tagged checkout so the hash is
  authoritative.

## Important Implementation Details

### Error Handling
- Use `anyhow::Result<T>` for all fallible functions
- Add context with `.context("description")?`
- Use `anyhow::bail!("msg")` for explicit errors
- Never use `unwrap()` in production paths - prefer `?` or `unwrap_or_default()`

### Async/Await
- Main function: `#[tokio::main]`
- All I/O operations are async (embedding API, LanceDB, file metadata)
- Use `.await?` for error propagation in async contexts
- Batch embedding requests to amortize network overhead

### LanceDB Arrow RecordBatch Pattern
When upserting chunks to LanceDB:
1. Pre-allocate vectors for columns
2. Build Arrow arrays: `StringArray`, `Float32Array`, `UInt64Array`
3. Create `RecordBatch` from arrays
4. Upsert via `RecordBatchIterator`

**Critical:** Vector dimension must match schema. Initialize collection on first embedding to detect dimension.

### Embedding Batching
- Default batch size: 32 chunks per API call
- Sort API responses by index to preserve chunk order
- Show progress with `indicatif` crate during indexing

### Reranking
- Optional cross-encoder reranking for better precision
- When enabled: fetch `top_n` candidates (default 50), rerank, return top `limit` results
- Reranker endpoint separate from embedding endpoint
- Enable via `--rerank` flag or config `reranker.enabled = true`

## Code Style Conventions

### Module Organization
- Declare modules at top of `main.rs`: `mod config;`
- One module per file in `src/`
- Flat structure (no deep nesting)

### Imports
Group by: stdlib → external crates → internal modules
```rust
use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use crate::config::Config;
```

### Naming
- Files/Modules: `snake_case.rs`
- Structs/Enums: `PascalCase`
- Functions/Variables: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`

### Type Patterns
- Derive: `Debug`, `Clone`, `Serialize`, `Deserialize` where appropriate
- Implement `Default` for configs and structs with sensible defaults
- Use `PathBuf` for all file paths (not `String` or `&str`)

### Output
- User-facing output: `println!()` to stdout
- Errors: `eprintln!()` to stderr
- Colors: Use `colored` crate for terminal output
- Progress: Use `indicatif::ProgressBar` for long operations
- Debug logging: `log::debug!()` with `env_logger` (enable with `--debug` flag)

## Testing Notes

- Test files should be in `src/` with `#[cfg(test)]` or separate `tests/` directory
- Use `cargo test -- --nocapture` to see debug output during tests
- Integration tests can create temporary indexes to test full pipeline

## Common Development Tasks

### Adding a New Config Option
1. Add field to appropriate `Config` struct in `config.rs`
2. Update `Default` implementation
3. Add to example TOML in comments or README
4. Use in relevant module (indexer, searcher, etc.)

### Modifying Vector Schema
1. Update `db.rs::init_collection()` schema definition
2. Update `db.rs::upsert_chunks()` to match new columns
3. Requires full reindex to apply schema changes
4. Consider versioning manifest to handle migrations

### Adding a New CLI Command
1. Add variant to `Commands` enum in `main.rs`
2. Add match arm in `main()` to handle command
3. Implement command logic (may extract to separate module)
4. Update CLI help text via doc comments

### Debugging Embedding Issues
1. Use `--debug` flag to enable detailed logging
2. Check `log::debug!()` output for batch processing
3. Verify endpoint is reachable: `curl http://r7.home.lab:7997/embeddings`
4. Confirm API response format matches OpenAI schema

## File Structure

```
msrch/
├── crates/
│   ├── core/
│   │   ├── src/
│   │   │   ├── lib.rs               # Core library entry point
│   │   │   ├── config.rs            # Configuration types and loading
│   │   │   ├── db.rs                # LanceDB vector database wrapper
│   │   │   ├── embedding.rs         # Embedding API client
│   │   │   ├── extract.rs           # Document extraction (HTML/PDF/docx)
│   │   │   ├── index.rs             # Indexing orchestration
│   │   │   ├── reranker.rs          # Reranking API client
│   │   │   ├── search.rs            # Query execution and formatting
│   │   │   ├── chunker.rs           # Text chunking with tiktoken
│   │   │   └── crawler.rs           # File discovery and filtering
│   │   └── Cargo.toml               # Core crate manifest
│   └── cli/
│       ├── src/
│       │   ├── main.rs              # CLI entry point, command dispatch
│       │   └── output.rs            # Output formatting
│       ├── build.rs                 # Build script for git hash embedding
│       └── Cargo.toml               # CLI crate manifest
├── Cargo.toml                       # Workspace manifest (version source of truth)
├── Cargo.lock                       # Locked dependency versions
├── Makefile                         # Build shortcuts
├── README.md                        # User documentation
├── CLAUDE.md                        # This file: developer guidance
├── AGENTS.md                        # Coding guidelines for AI agents
├── CHANGELOG.md                     # Release history
├── msrch_HLD.md                     # Detailed architecture design doc
└── .msrch/                          # Example index (created by `msrch index .`)
    ├── index.db/                    # LanceDB storage
    ├── manifest.json                # File tracking for incremental updates
    └── config.toml                  # Optional project-specific config
```

## References

- **HLD:** `msrch_HLD.md` - Comprehensive architecture and design decisions
- **AGENTS.md:** Detailed coding guidelines and build instructions
- **README.md:** User-facing documentation and usage examples
