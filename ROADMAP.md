# msrch — Roadmap

*Updated 2026-07-12. Supersedes the Google Doc capture ("msrchroadmap.md") of the
same date, revised after reviewing codedoc, Graphify, and actual usage patterns.*

## Vision

One msrch binary. Every capability available two ways — CLI subcommand and MCP
tool — both thin wrappers over the **same core library**.

msrch's proven identity: **the semantic hop in a two-hop search workflow**, for
humans and agents, over code *and* documents.

```
msrch query "where did we set up chunking?"   # hop 1: concept → files
grep -rn "chunk_with_treesitter" src/          # hop 2: identifier → lines
```

## What's already validated (build from here)

- **Agent on documents:** an MCP-less OpenCode/Opus agent at work uses msrch +
  grep to treat a document repo like code — tracks daily/weekly/monthly reports,
  called the tool "incredibly helpful." Ran entirely over the CLI.
- **Human on code:** daily two-hop usage (semantic query → grep identifiers).
- **The handoff is already served:** `-f filename` emits pipeable paths (grep
  `-l` style) and Context output prints tree-sitter semantic paths per hit, so
  hop-two identifiers are handed over automatically.
- Markdown/prose chunking already exists (`FileType::Markdown` / `Prose` in
  `chunker.rs`) — documents work today; only richer formats are missing.

## Work items (in order)

### 1. Workspace refactor — core lib + thin front-ends

```
msrch/
├── crates/
│   ├── core/     # index, query, extraction, (graph) — ALL logic lives here
│   ├── cli/      # clap subcommands, thin wrappers over core
│   └── mcp/      # rmcp server, thin wrappers over core
```

Discipline: **zero logic in command handlers.** Do this first — cheap at ~4k
LOC, expensive after new features land. Config layering and implicit-query
behavior go in core (they're the classic duplicated-between-front-ends traps).

### 2. Document extraction pipeline — HTML, PDF, docx

Directly widens the validated document use case. New extraction stage in core:
crawler → **extract** → chunk → embed.

- **HTML:** extract text before chunking. Decide: full page text vs
  readability-style main content (`scraper` for simple, `dom_smoothie` for
  readability). Main risk: nav/footer boilerplate polluting embeddings.
- **PDF:** text-layer only (`pdf-extract` / `lopdf`). Skip graphics-only PDFs
  via heuristic: extracted text near-empty relative to page count → skip with
  warning. No OCR, no vision models.
- **docx:** zip of XML; extraction nearly as easy as HTML. Work document repos
  are full of these.

### 3. Report-workflow query ergonomics

Small features that fall straight out of "track periodic reports":

- **Path/date filters:** `--path 2026/07/`, `--after` / `--before` (file
  mtime). "What did I do in March" = semantic + temporal; LanceDB can filter,
  so this is cheap.
- **Staleness-guarded auto-reindex:** config option so `query` runs the
  incremental mtime pass first when the manifest is stale. A report repo
  changes daily; neither humans nor agents should have to remember
  `msrch index`. Incremental machinery makes the check nearly free.
- **Zero-code:** drop an AGENTS.md snippet into consuming repos telling agents
  when to reach for msrch vs grep.

### 4. MCP server

`msrch mcp --transport stdio|http` via **rmcp** (official SDK; native stdio +
streamable-HTTP confirmed). Exposes query (and graph, if/when built) as tools.

- Test-driven by home use (Max subscription headroom); the work environment
  can't use MCP at all, so this is expansion, not the proven path.
- Treat MCP tool descriptions as UX — they determine agent uptake.
- **Open decision — server-mode index lifecycle:** a long-running MCP process
  holds the LanceDB table while `msrch index` may run concurrently. Re-open
  table per query, watch the manifest for staleness, or hold a handle? Decide
  before shipping HTTP mode; annoying to retrofit.

### 5. Benchmarks — numbers, not vibes

Define metrics before building anything speculative:

- **Arms:** plain grep · msrch semantic-only · Graphify structural-only ·
  msrch + Graphify side-by-side. If side-by-side shows no lift over
  semantic-only, the graph feature (item 6) dies cheaply.
- **Human metric:** fixed set of natural-language questions ("which file has
  the chunker?"), scored on right-file-in-top-3.
- **Agent metric:** tool calls / tokens until the agent locates the correct
  code or document.

### 6. Structural graph — gated, last

`msrch graph <file-or-symbol>` — neighbors / callers / hubs. Build **only if**
item 5 (or daily use) shows grep failing as hop two: too-common identifiers,
wanting callers ranked by importance, import-chain connections grep can't see.

**Validate before writing Rust:** `pip install graphifyy` — Graphify
(tree-sitter + NetworkX + Leiden, MIT) already ships a queryable code graph
with hub ranking. Run it as the benchmark arm; prototype personalized ranking
as a script over its `graph.json`. An hour reading `Graphify-Labs/graphify`
before trusting it on private code is warranted (marketing site is SEO-heavy;
claims unaudited).

Design decisions if it proceeds:

- **v1 edges: import-level, file-to-file.** Cheap, sufficient for hub ranking
  (Aider's repo-map operates at this level). Import *resolution* is the hidden
  80% of the work — relative imports, tsconfig path aliases, Rust mod/use vs
  crate layout, Go module paths. v1: best-effort path heuristics within the
  repo; unresolved edges tagged external. Existing grammars (Rust, Python,
  JS/TS, Go) are the right v1 language set.
- **"Callers" without a resolver:** tree-sitter parses, it doesn't bind names.
  Approximate callers Aider-style — files that mention the symbol's name and
  import its file. Cheap, wrong at the margins, surprisingly effective.
- **Freshness for free:** recompute outgoing edges for changed files inside the
  existing incremental mtime reindex; graph extraction needs no embedding
  calls, so it can never drift independently. Storage: serialized adjacency
  sidecar in `.msrch/` (fine at this scale).
- **Docs bonus:** markdown links are parseable edges — `msrch graph README.md`
  showing which code files the docs reference is nearly free.
- **The differentiator — semantic-personalized ranking:** use the query's
  top-k semantic hits as the PageRank personalization vector. This is the one
  capability Graphify's no-embeddings architecture can never match, and the
  point where the two axes become one capability. Global PageRank v1;
  personalized ranking is the headline v2 feature.

## Explicitly dropped

- **Porting codedoc.** Examined: ~1,400 lines of regex-driven Python; graph
  edges Python-only; import resolution by basename string-match; hardcoded
  per-project heuristics; "hub ranking" is a one-pass degree sum. Nothing to
  port — the graph item above is a greenfield feature informed by the concept.
  "End state: no Python" is achieved by archiving codedoc.

## Resolved from the original capture's open questions

| Question | Resolution |
| --- | --- |
| rmcp transport API | Confirmed: native stdio + streamable-HTTP |
| Import-level vs call-level edges | Import-level v1; mention-matching approximates callers |
| Global vs personalized hub ranking | Global v1; semantic-personalized v2 (the differentiator) |
| Keeping the structural layer fresh | Tie edge recompute into incremental mtime reindex |
