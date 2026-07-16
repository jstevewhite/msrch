# MCP Server — Design (Roadmap Item 4)

*Approved 2026-07-16. Targets release 0.6.0. No index schema change (stays v5).*

## Purpose

Expose msrch's search over the Model Context Protocol so MCP-capable agents
get the same capability shell agents already have — one binary, two
transports, all logic in `msrch-core`. The CLI remains fully supported; MCP
is an additional thin front-end, per the workspace's founding constraint.

## Deployment models (both in v1)

- **stdio** — an MCP client (Claude Code, Claude Desktop, OpenCode) launches
  `msrch mcp` as a per-project child process with cwd inside the project.
  Index discovery is CLI-identical walk-up from cwd; zero configuration
  beyond the client's `.mcp.json` entry.
- **HTTP (streamable)** — one long-running process fronting one or more
  corpora, e.g. on the lab box over tailnet:
  `msrch mcp --transport http --index reports=/data/reports --index code=/code/msrch`.

## CLI surface

```
msrch mcp [--transport stdio|http]     # default: stdio
          [--index [name=]path ...]    # register roots; repeatable
          [--bind ADDR:PORT]           # http only; default 127.0.0.1:7920
```

- With no `--index`: walk-up discovery from cwd (error if no `.msrch` found,
  same message as query). The single entry's name is the root's directory
  basename.
- `--index path` → name = basename; `--index name=path` → explicit name.
- Startup validation, fail fast: every root must contain `.msrch/` (error
  names the offending path); duplicate names are an error; `--bind` with
  `--transport stdio` is an error.

## Crate layout

New `crates/mcp` (package `msrch-mcp`, lib). It owns: rmcp dependency, the
index registry, tool schemas/handlers, and transport startup
(`pub async fn serve(options: McpOptions) -> Result<()>`). The `msrch` binary
gains a `Mcp` subcommand whose match arm parses flags into `McpOptions` and
makes one call — zero logic in the handler. `crates/cli` depends on
`msrch-mcp`; rmcp appears nowhere else.

**Shared date parsing:** the `dates` module moves from `crates/cli` to
`msrch-core` (`msrch_core::dates`) so both front-ends parse `YYYY-MM-DD` /
`Nd`/`Nw`/`Nm` identically; the CLI's clap value_parser wraps it. Core's
`SearchOptions` continues to take resolved `SystemTime`s — parsing strings
remains a front-end concern, just a shared one.

## Index registry

```rust
struct IndexEntry { name: String, root: PathBuf }
struct IndexRegistry { entries: Vec<IndexEntry> }
```

- Built once at startup from `--index` flags or walk-up discovery.
- `resolve(&self, index: Option<&str>) -> Result<&IndexEntry>`:
  `None` + one entry → that entry; `None` + many → error listing names;
  `Some(name)` → match or error listing valid names.
- No runtime mutation (no dynamic registration in v1).

## Tools

All tools take an optional `index` (string, registry name; required only
when multiple roots are registered — the tool description states the
registered names so agents can discover them without a round-trip).

### `search`

Arguments: `query` (string, required), `index?`, `limit?` (int),
`rerank?` (bool), `min_similarity?` (number 0.0–1.0),
`path_contains?` (string), `after?` / `before?` (string — same forms as the
CLI: `YYYY-MM-DD` or `7d`/`2w`/`3m`; after-inclusive, before-exclusive).

Behavior: resolve the index → load the root's effective config → honor
`query.auto_index` (the existing quiet, non-fatal, schema-guarded
`index_quiet` machinery) → build `SearchOptions` → `Searcher::search`.

Result (mirrors the CLI's JSON contract):

```json
{
  "index": "reports",
  "query": "...",
  "auto_index_refreshed": 3,        // present only when > 0
  "results": [
    { "file_path": "...", "chunk_index": 2, "similarity": 0.71,
      "context": "impl::Foo::fn::bar", "content": "..." }
  ]
}
```

### `stats`

Arguments: `index?`. Result: the existing `IndexStats` fields as JSON
(index_path, root_path, file_count, chunk_count, estimated_tokens,
last_indexed as ISO-8601 or null, size_on_disk, model, endpoint).

### `list_indexes`

No arguments. Result: `[{ name, root, files }]` — `files` read from the
manifest (no DB open, stays fast; 0 when the manifest is missing/unreadable).

## Index lifecycle

Per-request open: every `search`/`stats` call constructs its `Searcher`/reads
its stats fresh, exactly like a CLI invocation. Cost is milliseconds against
embedding-call latency; the payoff is inherent safety against concurrent
`msrch index`/`reindex` runs — the server never holds a table handle that can
go stale. Revisit only with profiling evidence.

## Transports & security posture

- **stdio:** rmcp's stdio transport. stdout is protocol-only — all msrch
  status/warning output already goes to stderr (guaranteed by the 0.4.0
  audit), so no output-contamination work is needed.
- **HTTP:** rmcp's streamable-HTTP transport. Default bind `127.0.0.1:7920`;
  exposing beyond localhost is an explicit `--bind` decision. **No
  authentication in v1** — documented localhost/tailnet-trust posture,
  matching the embedding/reranker endpoints. Bearer-token support is parked
  (YAGNI ledger).
- Path safety: clients never supply filesystem paths — only registry names.
  The only paths the server touches are the startup-registered roots.

## Error handling

- Tool-level failures → MCP tool errors with actionable text: unknown index
  name (lists valid names), missing manifest for date filters, embedding
  endpoint unreachable, and pre-v5 index (message includes "run 'msrch
  index .' to migrate").
- Auto-index failures inside `search` stay non-fatal exactly as in the CLI:
  warn to the server's stderr, search the existing index.
- Extraction/indexing warnings never appear in tool results.

## Testing

- **Unit:** registry resolution (single/multi/unknown/duplicate), `--index`
  flag parsing (`name=path` and bare-path forms), startup validation
  failures, date-string round-trip through the relocated `msrch_core::dates`
  (existing tests move with the module).
- **In-process:** rmcp client↔server round-trip over an in-memory/stdio-pair
  transport where rmcp supports it: `list_indexes` and `stats` against a
  fixture index (no network needed); `search` argument-validation errors
  (bad date string, unknown index) without reaching the embedder.
- **Manual acceptance (needs the live embedding endpoint):** (1) stdio via a
  real `.mcp.json` in this repo driven by Claude Code — search/stats/
  list_indexes end to end; (2) HTTP with two registered roots — per-name
  search, default-index error when the name is omitted.
- Existing suite stays green; CLI behavior byte-identical (the dates-module
  move must not change any CLI output or error text).

## Docs & release

- README: "MCP server" section — both transports, a copy-paste `.mcp.json`
  example, the named-roots HTTP example, security posture note.
- `docs/AGENTS-SNIPPET.md`: one line — agents with MCP support can use
  `msrch mcp` instead of shell calls; same capabilities.
- CLAUDE.md: architecture (crates/mcp in the tree + Key Modules), the
  `msrch mcp` command in Essential Commands.
- CHANGELOG 0.6.0; version bump; `git tag v0.6.0` on main after merge.

## YAGNI ledger (explicitly not in v1)

- Authentication (bearer token parked), TLS
- `similar` and `reindex`/`index` tools (auto_index covers freshness;
  rebuilds are an operator action)
- Runtime root registration / deregistration
- Per-call filesystem paths of any kind
- Watch mode / file-system event integration
- Held DB handles or caching (per-request open until profiling says otherwise)
- MCP resources/prompts (tools only)

## Success criteria

- In this repo, a `.mcp.json` entry (`"command": "msrch", "args": ["mcp"]`)
  lets Claude Code call `search` with filters and get the same hits the CLI
  returns, including auto-index freshness when the repo config enables it.
- On one process, `msrch mcp --transport http --index a=... --index b=...`
  serves both corpora by name; omitting `index` errors helpfully; localhost
  bind by default.
- `msrch --version` reports 0.6.0 / schema v5; CLI untouched in behavior;
  suite green with the new registry/date tests.
