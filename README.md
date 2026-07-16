# msrch

**msrch** is a local-first semantic search CLI tool. It contains optimizations for codebases, but works with any text-based data — including HTML, PDF, and docx documents. It creates per-directory indexes using embeddings and provides fast semantic queries that understand concepts, not just keywords.

## Features

- **Semantic Search**: Find code by meaning, not just text matching
- **Smart Chunking**: Tree-sitter based semantic code extraction for Rust, Python, JavaScript/TypeScript, and Go
- **Documents Too**: Extracts and indexes text from HTML, text-layer PDF, and .docx alongside source and Markdown
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
git clone https://github.com/jstevewhite/msrch.git
cd msrch

# Build the release binary and install it to ~/.cargo/bin
make install
# or, equivalently:
cargo install --path crates/cli
```

`crates/cli` is the binary crate — `cargo install --path .` won't work because the
workspace root is a virtual manifest with no package to install.

### Requirements

- A recent stable Rust toolchain (2024 edition)
- An OpenAI-compatible embedding service (local or cloud)

## Semantic Chunking

msrch uses tree-sitter to extract complete semantic units from code files, ensuring search results contain meaningful, self-contained code snippets:

### Supported Languages

- **Rust** (.rs): Functions, structs, enums, traits, impl blocks, modules (with doc comments)
- **Python** (.py): Functions, classes, methods
- **JavaScript/TypeScript** (.js, .jsx, .ts, .tsx): Functions, classes, methods, arrow functions
- **Go** (.go): Functions, methods, type declarations

### How It Works

Instead of arbitrary token-based splits, msrch:

1. **Parses code structure** using tree-sitter AST parsers
2. **Extracts complete functions/classes** as semantic chunks
3. **Preserves context** with hierarchical paths (e.g., `impl::Person::fn::get_name`)
4. **Falls back gracefully** to token-based chunking for:
   - Unsupported languages
   - Parse errors
   - Oversized functions (>512 tokens)
   - Non-code files (markdown, prose)

### Adding Languages

See [ADDING_LANGUAGES.md](ADDING_LANGUAGES.md) for step-by-step instructions on adding support for additional languages.

## Document Extraction

Beyond source code, msrch extracts plain text from common document formats before
chunking, so you can search prose the same way you search code:

- **HTML** (`.html`, `.htm`, `.xhtml`) — readability-style main-content extraction, with automatic whole-page fallback for non-article pages
- **PDF** (`.pdf`) — text-layer extraction; graphics-only/scanned PDFs are skipped with a warning (no OCR)
- **docx** (`.docx`) — headings map to Markdown headings for structure-aware chunking
- **Markdown / prose** — chunked directly

Extraction of these document formats respects `chunking.max_file_size_mb` (default 10).

## Quick Start

### 1. Configure Embedding Service

Create a global config file at `~/.config/msrch/config.toml` (or `$XDG_CONFIG_HOME/msrch/config.toml` if set):

```toml
[embedding]
endpoint = "http://localhost:7997/embeddings"  # full URL to your embedding endpoint
model = "mixedbread-ai/mxbai-embed-large-v1"
# api_key = "sk-..."  # optional, sent as a bearer token

[query]
default_limit = 10
min_similarity = 0.5
```

Upgrading: if a config exists only in the legacy macOS location (`~/Library/Application Support/rs.msrch/`), it is copied over automatically on first run (the old file is left in place).

Run `msrch config` at any time to print the effective configuration.

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

```text
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

# JSON output for scripting
msrch "database" --format json | jq -r '.results[].file_path'

# Deduplicated file paths only (grep -l style), for piping into other tools
msrch "database" --format filename

# Use reranker for better precision (slower)
msrch "auth logic" --rerank

# Force reranking OFF even where config enables it
msrch "auth logic" --no-rerank

# Skip auto-index for this query (overrides query.auto_index in config)
msrch "quick search" --no-auto-index

# Filter by path substring
msrch "config" --path src/

# Filter by file modification time (after-inclusive, before-exclusive;
# relative units: 7d / 2w / 3m = days / weeks / months ago; month ≈ 30 days)
msrch "changes" --after 7d                 # last 7 days
msrch "recent" --after 2026-07-01          # YYYY-MM-DD form
msrch "old" --before 2w                    # before 2 weeks ago
msrch "planning notes" --after 3m          # last ~3 months
msrch "window" --after 2026-07-01 --before 2026-08-01

# Per-query minimum similarity (0.0-1.0; default from config)
msrch "config parsing" --min-similarity 0.7    # or: -m 0.7
```

JSON output includes `score_kind` (`"vector"` cosine similarity, or
`"reranker"` cross-encoder relevance — a different scale) and a `warnings`
array when a degradation occurred (e.g. reranker unreachable).

Index discovery is automatic (walk-up), so there is no `--index` flag —
run from anywhere inside the indexed tree.

### Advanced Commands

```bash
# Find semantically similar files
msrch similar <file>

# Show effective configuration
msrch config
```

## Configuration

Configuration is loaded in this priority order:

1. CLI flags (`--limit`, `--format`, `--min-similarity`, `--rerank`, `--no-rerank`, `--path`, `--after`/`--before`, `--no-auto-index`)
2. Project config: `.msrch/config.toml` in the index root (overlaid field-by-field)
3. Global user config: `~/.config/msrch/config.toml` (`$XDG_CONFIG_HOME/msrch/config.toml` when set)
4. Built-in defaults

### Example Configuration

```toml
[embedding]
endpoint = "http://localhost:7997/embeddings"  # full URL; built-in default is http://r7.home.lab:7997/embeddings
model = "mixedbread-ai/mxbai-embed-large-v1"
batch_size = 32
timeout_seconds = 30
max_retries = 3

[chunking]
max_chunk_tokens = 512
overlap_tokens = 50
max_file_size_mb = 10
use_treesitter = true
treesitter_languages = ["rust", "python", "javascript", "typescript", "go"]
fallback_to_tokens = true

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
auto_index = false  # (default) set true to refresh the index before every query — quiet (one line only when files changed); failures fall back to the stale index
# Note: the refresh is not atomic — if the embedding endpoint goes down mid-refresh, recently modified files may be temporarily unsearchable until the next successful index run.

[reranker]
enabled = false
endpoint = "http://localhost:7995/rerank"
model = "BAAI/bge-reranker-large"
top_n = 50

[display]
show_similarity_scores = true
color_output = true
```

## Index Discovery

msrch automatically walks up the directory tree to find `.msrch/`, just like `git` finds `.git/`:

```text
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

## Using msrch from coding agents

msrch is designed to be driven by shell-capable agents (no MCP required):
semantic hop with `msrch`, identifier hop with `grep`. See
[docs/AGENTS-SNIPPET.md](docs/AGENTS-SNIPPET.md) for a copy-paste block to add
to a repo's AGENTS.md / CLAUDE.md.

## MCP server

`msrch mcp` exposes search over the Model Context Protocol — same core, same
results as the CLI.

**Per-project (stdio):** add to the repo's `.mcp.json` (Claude Code) or MCP
client config; the server discovers the index by walking up from its working
directory, exactly like the CLI:

```json
{
  "mcpServers": {
    "msrch": { "command": "msrch", "args": ["mcp"] }
  }
}
```

**Shared server (HTTP):** one long-running process can front several indexes
by name:

```bash
msrch mcp --transport http \
  --index reports=/data/reports \
  --index code=/code/msrch \
  --bind 127.0.0.1:7920      # default; use a tailnet address to share
```

Tools: `search` (full filter set: `path_contains`, `after`/`before`,
`min_similarity`, `rerank`), `stats`, `list_indexes`. When several indexes
are registered, pass `index` by name. Clients never supply filesystem paths.
There is no authentication in v1 — bind to localhost or a trusted network
(e.g. tailnet) only.

## Project Structure

Indexed directories contain:

```text
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

- **tree-sitter**: AST parsing for semantic code extraction
- **LanceDB**: Vector storage (local mode)
- **reqwest**: HTTP client for embedding API
- **tokio**: Async runtime
- **clap**: CLI argument parsing
- **tiktoken-rs**: Token counting
- **anyhow**: Error handling

For detailed architecture, see [docs/msrch_HLD.md](docs/msrch_HLD.md) and [CLAUDE.md](CLAUDE.md).

## Performance

- **Query time**: 20-100ms for typical projects (exact cosine scan)
- **Index size**: 4 bytes per vector dimension (Float32), plus the stored chunk text — roughly a few KB per chunk with a 1024-dim model
- **Memory**: ~10-200MB depending on index size

## FAQ

**What embedding service do I need?**

Any OpenAI-compatible API. This includes OpenAI, local inference servers, or other providers.

**Is my data private?**

Yes! All indexes and file contents are stored locally. Only embeddings are sent to your configured service.

**Can I use this with large codebases?**

Yes, within reason. Vector search is currently an exact (brute-force) cosine scan over all chunks — there is no approximate (ANN) index yet — so query time grows with the number of chunks. It stays fast (tens of milliseconds) for typical projects; very large indexes will be slower.

**How does incremental indexing work?**

msrch tracks file modification times and only re-embeds changed or new files.

## License

MIT License.
