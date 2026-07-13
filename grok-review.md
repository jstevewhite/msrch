# Codebase review: msrch

**Date:** 2026-07-11  
**Reviewer:** Grok  
**Scope:** Full review of `src/` (~3.9k LOC, 9 modules), docs, and tests. Working tree was clean; this is not a diff review.  
**Tests:** 42 passed. Clippy pedantic reports ~139 warnings (mostly style).  
**Prior art:** Compared against `docs/opus_review-2026-07-02.md` — several earlier bugs are fixed.

---

## Overall

msrch is a solid local-first semantic search CLI with a clear pipeline (crawl → chunk → embed → LanceDB → search/rerank) and a thoughtfully designed tree-sitter chunker. Module boundaries are clean, error handling mostly uses `anyhow` with context, and recent work (schema versioning, context in embeddings/results, reindex/root discovery, config load warnings, strong chunker tests) shows good product sense.

The main themes of this review:

1. **Config/docs still oversell the product** (HNSW, project config, several knobs).
2. **Indexing durability is the highest real risk** (partial failures, soft deletes).
3. **Chunker edge cases lose content** for oversized semantic units.
4. **Architecture is ready to harden** more than to rewrite.

---

## What’s working well

| Area | Notes |
|------|--------|
| **Tree-sitter chunking** | Real semantic units + hierarchical `context` paths; docs, attributes, decorators handled carefully; multi-language support with documented extension path |
| **Schema versioning** | `SCHEMA_VERSION` + wipe-and-rebuild is the right migration story for embedding/schema changes |
| **Context in search** | Context is stored, shown in UI/JSON, and **prepended into embedding text** so symbol names actually affect vectors |
| **CLI UX** | Implicit query, formats (`plain`/`context`/`json`/`filename`), walk-up index discovery, `.gitignore` + `.msrchignore` |
| **Incremental index** | mtime + chunk-id delete before re-embed; deleted-file cleanup |
| **Chunker tests** | ~33 tests covering languages, doc-comment regressions, TSX, fallbacks — best-covered part of the system |
| **Config resilience** | `#[serde(default)]` + `load_global_config_or_default()` avoid silent all-defaults on partial configs |

---

## Progress since the July 2026 review

Several earlier “fix these” items look addressed:

- **`min_similarity` is applied** in `db::search` (`score >= min_score`).
- **Tree-sitter `context` is stored and embedded**, not discarded.
- **`reindex` finds the index root and force-deletes `.msrch/`** before rebuild.
- **Index/query errors propagate** via `?` (exit non-zero on failure) — except parts of `Similar`.
- **`find_index_root` is shared** via `index.rs`.
- **Tokenizer is cached** (`LazyLock` on `BPE`).
- **Ignore patterns use `OverrideBuilder`** instead of naive substring matching.

That is real forward progress; residual issues below are the current bar.

---

## Correctness / reliability

### 1. Partial index failure leaves a corrupt index (bug)

Embedding is sequential by batch. On failure mid-run, already-upserted batches stay in LanceDB, but the manifest is only written after **all** batches succeed.

Next `index` then:

- Sees files as “new/changed” (manifest still old or empty for those paths),
- Does not reliably delete the orphan vectors already added,
- **Appends** again via `table.add()`.

You can accumulate duplicate vectors with no user-visible error until search quality drops. The delete-before-add path only helps when the **previous** manifest still has those chunk IDs.

**Suggestion:** Write progress more carefully (e.g. per-file commit: delete → embed → upsert → update that file’s manifest entry), or treat any failed run as requiring wipe/reindex and make delete failures fatal. A transactional “batch of files” unit is enough.

### 2. Stale-chunk delete failures are soft (`warn!` only)

In `index.rs`, failed `delete_by_ids` only logs a warning, then re-embeds and appends. That guarantees duplicates on partial DB errors.

**Suggestion:** Make delete failure fatal for modified/deleted files (`return Err(...)`).

### 3. Oversized tree-sitter items can vanish (bug)

If a function/class exceeds `max_chunk_tokens`, it is skipped. Fallback to token chunking only runs when **no** semantic chunks were extracted for the whole file. A file with one huge function and several small ones indexes only the small ones; the large body never appears.

**Suggestion:** For oversized extractable nodes, call `split_by_tokens` on that node’s text (with the same `context` path) instead of dropping it.

### 4. Nested multi-granularity double-embeds (design debt)

`impl`/`class`/`type` chunks **and** their methods are both embedded. Method bodies are effectively indexed twice. Fine if intentional (parent for “find the type”, child for “find the method”), but it inflates cost and can dominate results. Document it, or make it configurable (`embed_parents: bool`).

### 5. `chunk_index` reads like a line number

Output is `path:chunk_index`. Users and editors will treat `foo.rs:3` as line 3. Tree-sitter has byte/line ranges; storing `start_line` would make results editor-friendly and more trustworthy.

### 6. `Similar` is fragile and can exit 0 on embed failure

- Embed errors print and `return Ok(())` → exit code 0.
- Path equality uses string display of canonical path vs stored `file_path` from the walker (often absolute, but not always the same normalization) — self-matches may leak through.
- Truncates by **bytes** (`content[..8000]`), which can panic on a non-UTF-8 boundary (Rust string slicing panics mid-char).

### 7. Embedding response shape is still brittle

`usage` and `index` are required fields. Many OpenAI-compatible servers omit them. Prefer `usage: Option<...>` and fall back to response order if `index` is missing. `max_retries` is still unused despite config defaults.

---

## Config surface vs reality

These are configured or documented but not fully real:

| Knob / doc claim | Reality |
|------------------|---------|
| Project `.msrch/config.toml` | `load_from_path` exists, never called (dead_code warning) |
| `query.output_format` | CLI only; string field unused |
| `embedding.max_retries` | Not implemented (POC comment remains) |
| `chunking.max_file_size_mb` | Not enforced; large files fully read |
| `indexing.follow_symlinks` | Not passed into `WalkBuilder` |
| HNSW / “scales with HNSW” (README FAQ) | Flat scan only; no `create_index` |
| Default embedding endpoint | Hardcoded `http://r7.home.lab:7997/...` — bad default for anyone else |
| CLI flags in HLD (`--threshold`, `--index`, `--type`, `--quiet`) | Partially missing from real CLI |

Either wire these up or demote docs/config to match the code. Silent no-ops erode trust faster than missing features.

---

## Architecture / structure

**Strengths:** Flat modules, clear data flow, schema versioning, shared root discovery.

**Improvements:**

1. **`main.rs` still owns `Similar` end-to-end** (~100 lines of embed/DB/format). Move next to `search` or a small `similar` helper so `main` only dispatches (Stats is already better).
2. **`db.rs` is unwrap-heavy** on Arrow column downcasts. Controlled schema makes panics unlikely, but guidelines say avoid unwraps in production paths — use `context` + `?` for clearer failures after schema drift.
3. **Payload is free-form JSON** then re-parsed into columns. A typed `ChunkPayload` struct would remove the `as_object().unwrap()` and keep schema/upsert in sync.
4. **No ANN index strategy.** Fine for small/medium repos; for large monorepos, plan Lance IVF/HNSW (or document flat-scan limits honestly).
5. **Searcher always reloads global config**, never project config — blocks per-repo model/endpoint overrides.

---

## Performance

| Issue | Impact |
|-------|--------|
| Sequential embedding batches | Index time dominated by RTT × batch count; `futures` is already a dep — bounded concurrency would help |
| Full `chunks_to_embed` buffer | Peak memory = all changed content before any network I/O |
| Flat vector scan | Query latency grows with corpus size |
| `cl100k_base` vs embedding model tokenizer | Chunk boundaries are approximate (acceptable if documented) |

Streaming “file → chunk → embed → upsert → manifest patch” would fix both durability (#1) and memory.

---

## Testing

**Good:** Chunker regression suite is excellent (doc over-collection, names, TSX, decorators, fallbacks). Schema migration unit tests. CLI parse tests for implicit query. Config missing-fields test.

**Gaps (highest value next):**

1. Incremental reindex: modify file → delete old IDs → no duplicates (the invariant CLAUDE.md calls critical).
2. Partial embed failure behavior (or the fix for it).
3. `db::search` min_score filtering (table-level unit test with a tiny fixture).
4. Crawler ignore patterns / `.msrchignore` / binary skip.
5. Embedding client deserialization with optional `usage`/`index` (mock HTTP).

Right now coverage is strong where the product is differentiated (chunking) and thin where data loss can happen (index/db).

---

## Smaller notes

- **`--debug` only on `Index`** — query/similar would benefit from the same global flag.
- **Token estimate in stats** (`chunk_count * 256`) is arbitrary vs 512 default; store real token totals if you care.
- **JSON pretty-print uses `.unwrap()`** in `display_json` — extremely unlikely to fail, but inconsistent with error style.
- **Pedantic clippy** is noisy (139); worth fixing a focused set or documenting an allow-list rather than ignoring forever.
- **HLD still describes Qdrant/HNSW-era design** in places — drift from LanceDB implementation.

---

## What I’d prioritize

| Priority | Item | Why |
|----------|------|-----|
| P0 | Durable indexing (per-file commit or fatal mid-run recovery) | Prevents silent index corruption |
| P0 | Fatal delete-on-reindex failures | Same class of bug |
| P1 | Fallback-split oversized tree-sitter nodes | Lost code in large functions |
| P1 | Wire or remove dead config; fix README HNSW claim | Trust / onboarding |
| P1 | Safer defaults for embedding endpoint (localhost or required config) | Out-of-box experience |
| P2 | Retries with backoff on embed/rerank | Production reliability |
| P2 | `start_line` in results | UX for editors/agents |
| P2 | Extract `Similar`; concurrency + streaming index | Maintainability / speed |
| P3 | ANN index when count > N | Scale story |
| P3 | Project config merge | Documented feature gap |

---

## Bottom line

This is a credible, usable POC-plus: the semantic chunker is the standout piece, the search UX is thoughtful, and several hard correctness issues from the earlier review are already fixed. The next quality jump is not more languages — it is **making the index transactionally trustworthy**, **closing the config/docs honesty gap**, and **covering the index/DB path with tests** the way the chunker already is.
