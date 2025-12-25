# msrch

**msrch** is a local-first semantic search CLI tool for codebases. It creates per-directory indexes using embeddings and provides fast semantic queries that understand concepts, not just keywords.

## Features

- **Semantic Search**: Find code by meaning, not just text matching
- **Local-First**: All indexes stored locally, no cloud dependencies
- **Git-Like UX**: Automatic index discovery (walks up tree like `git`), honors `.gitignore`
- **Incremental**: Smart reindexing based on file modification times
- **Fast**: Sub-100ms query times for typical codebases
- **Flexible**: Works with any OpenAI-compatible embedding service
- **Scriptable**: JSON output for automation and AI agent integration

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/yourusername/msrch.git
cd msrch

# Build and install
cargo build --release
cargo install --path .
```

### Requirements

- Rust 2024 edition
- An OpenAI-compatible embedding service (local or cloud)

## Quick Start

### 1. Configure Embedding Service

Create a config file at `~/.config/msrch/msrch.conf`:

```toml
[embedding]
endpoint = "http://localhost:8765/v1"  # Your embedding service
model = "mixedbread-ai/mxbai-embed-large-v1"
# api_key = "sk-..."  # Optional, for OpenAI

[query]
default_limit = 10
min_similarity = 0.5
```

### 2. Index Your Project

```bash
cd /path/to/your/project
msrch index .
```

### 3. Search

```bash
# Search from anywhere in the project tree
msrch "jwt authentication"

# Or use explicit command
msrch query "token validation" --limit 5
```

## Usage

### Basic Search

```bash
# Semantic search - works from any subdirectory
cd src/auth
msrch "token verification"
```

Output:
```
Found 2 results:
0.89 src/auth/jwt.rs:23
│ pub fn verify_token(token: &str) -> Result<Claims> {
│     let key = DecodingKey::from_secret(SECRET.as_ref());
│     decode::<Claims>(token, &key, &Validation::default())

0.84 src/auth/middleware.rs:15
│ fn authenticate_request(req: &Request) -> Result<User> {
│     let token = extract_bearer_token(req)?;
│     verify_jwt(&token)
```

### Index Commands

```bash
# Create or update index
msrch index <path>

# Force full rebuild
msrch reindex

# Show index statistics
msrch stats
```

### Query Options

```bash
# Limit results
msrch "error handling" --limit 5

# Set minimum similarity threshold
msrch "config parsing" --threshold 0.7

# JSON output for scripting
msrch "database" --format json | jq '.results[].file_path'

# Specify index explicitly
msrch "query" --index /path/to/.msrch
```

### Advanced Commands

```bash
# Find semantically similar files
msrch similar <file>

# Show effective configuration
msrch config
```

## Configuration

Configuration is loaded in this priority order:

1. CLI flags (`--threshold`, `--endpoint`, etc.)
2. Project config: `.msrch/config.toml` (in indexed directory)
3. User config: `~/.config/msrch/msrch.conf`
4. Built-in defaults

### Example Configuration

```toml
[embedding]
endpoint = "http://localhost:8765/v1"
model = "mixedbread-ai/mxbai-embed-large-v1"
batch_size = 32
timeout_seconds = 30
max_retries = 3

[chunking]
max_chunk_tokens = 512
overlap_tokens = 50
max_file_size_mb = 10

[indexing]
skip_binary = true
follow_symlinks = false
ignore_patterns = [
    ".git/", ".msrch/", "node_modules/",
    "target/", "__pycache__/", "*.pyc"
]

[query]
default_limit = 10
min_similarity = 0.5
output_format = "context"  # plain|context|json

[display]
show_similarity_scores = true
color_output = true
```

## Index Discovery

msrch automatically walks up the directory tree to find `.msrch/`, just like `git` finds `.git/`:

```
/home/user/projects/nebula/.msrch/    # Index is here
  └── src/
      └── auth/                       # You are here

$ msrch "query"
# Automatically uses /home/user/projects/nebula/.msrch/
```

## File Filtering

msrch respects `.gitignore` patterns and supports `.msrchignore` for project-specific exclusions:

```gitignore
# .msrchignore
# Exclude build artifacts
target/
dist/
*.pyc

# Exclude sensitive files
secrets.toml
.env
*.key
```

## Project Structure

Indexed directories contain:

```
<project>/
└── .msrch/
    ├── index.db/              # Vector database (LanceDB)
    ├── manifest.json          # Index metadata
    └── config.toml          # Project-specific config (optional)
```

## Development

### Build

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Check without building
cargo check
```

### Test

```bash
# Run all tests
cargo test

# Run single test
cargo test test_name

# Run with output
cargo test -- --nocapture
```

### Lint & Format

```bash
# Format code
cargo fmt

# Run clippy
cargo clippy

# Pedantic clippy
cargo clippy -- -W clippy::pedantic
```

See [AGENTS.md](AGENTS.md) for detailed coding guidelines.

## Architecture

msrch is built with Rust and uses:

- **LanceDB**: Vector storage (local mode)
- **reqwest**: HTTP client for embedding API
- **tokio**: Async runtime
- **clap**: CLI argument parsing
- **tiktoken-rs**: Token counting
- **anyhow**: Error handling

For detailed architecture, see [msrch_HLD.md](msrch_HLD.md).

## Performance

- **Query time**: 20-100ms for typical projects
- **Index size**: ~6-8 bytes per vector dimension (~8KB per chunk with 1024-dim model)
- **Memory**: ~10-200MB depending on index size

## FAQ

**What embedding service do I need?**

Any OpenAI-compatible API. This includes OpenAI, local inference servers, or other providers.

**Is my data private?**

Yes! All indexes and file contents are stored locally. Only embeddings are sent to your configured service.

**Can I use this with large codebases?**

Yes. msrch scales efficiently with HNSW indexing. Typical 10k-file projects query in 40-80ms.

**How does incremental indexing work?**

msrch tracks file modification times and only re-embeds changed or new files.

## License

MIT License - see LICENSE file for details.
