# msrch Code Review

**Date:** 2026-07-02
**Reviewer:** Claude (Opus 4.8)
**Branch:** `feature/treesitter-chunking`
**Scope:** Full source review of `src/*.rs` (2,490 lines across 9 modules)

## Overall

Solid, readable foundation. The architecture in `CLAUDE.md` is sound, the module
boundaries are clean, and the tree-sitter chunker is genuinely nice work. The main
theme of this feedback: **several documented/configured features are silently
no-ops**, and there are a few real correctness bugs. The `POC` comments scattered
through the code are honest markers — this reads like a working prototype that
hasn't had its config surface wired up to match its ambitions.

Findings are grouped by severity with `file:line` references.

---

## Correctness bugs (fix these)

### 1. The similarity threshold is never applied
`search.rs:79` reads `config.query.min_similarity` (default `0.5`) and passes it to
`db.search(...)`, but `db.rs:171` names the parameter `_min_score` and ignores it.
No filtering happens anywhere. Results below the configured threshold are returned.
Either filter in `db.search` after computing `score = 1.0 - distance`, or drop the
config field. Right now it's a lie in the config.

### 2. Tree-sitter `context` is computed and then thrown away
The chunker does real work building `context` paths like `impl::Foo::fn::bar`
(`chunker.rs:296`), but `index.rs:179` builds the payload with only `file_path`,
`content`, `chunk_index`. The `context` field never reaches the DB (no column for it
in the `db.rs:35` schema either). That's the whole payoff of semantic chunking
discarded. Prepend it to the embedded text, or store it and show it in results —
otherwise the tree-sitter feature buys you better *boundaries* but none of the
*context* it advertises.

### 3. `reindex` is broken from subdirectories and doesn't force a rebuild
`main.rs:131-148` is self-aware about being a mess ("Quick hack", "Wait, searcher
doesn't expose root"). It creates a `_searcher` that connects to the DB and is then
discarded (wasted work), then indexes `current_dir()` directly instead of walking up
to find the existing `.msrch/`. Run `msrch reindex` from a subfolder and you'll
create a *new, second* index there. Also the help says "Force full rebuild" but it
just calls incremental `index()` — it never deletes the existing index. Both the
behavior and the doc are wrong.

### 4. Commands swallow errors and exit 0 on failure
`Index` (`main.rs:113-116`), `Query`, `Similar`, etc. print an error to stderr then
`return Ok(())`. The process exits with code 0 even when indexing/search failed. That
breaks scripting (`msrch index . && ...` proceeds after a failure). Propagate the
error (`?`) or `std::process::exit(1)`.

### 5. Embedding response deserialization is brittle
`embedding.rs:15-30` requires `usage` and `EmbeddingData.index` to be present and
non-optional. Many OpenAI-*compatible* servers (the stated target) omit `usage` or
`index`. A missing field fails the whole `response.json()` parse. Make
`usage: Option<Usage>` and consider tolerating a missing `index` (fall back to
response order). This is exactly the kind of thing the `--debug` commit (f2b2ee9) was
chasing.

### 6. `config.query.output_format` (String) is dead and inconsistent
It defaults to `"context"` but nothing reads it — format comes only from the CLI
flag, via the `OutputFormat` enum used everywhere else. Either wire it up or delete it.

---

## Documented features that don't exist

Gaps between `CLAUDE.md` and the code. Either implement or correct the docs — right
now the docs oversell.

- **HNSW indexing** — `CLAUDE.md` says "LanceDB with HNSW indexing" and "HNSW
  similarity search." `create_index` is *never called* (confirmed by grep). Every
  search is a brute-force flat scan. Fine for a few thousand chunks, but it's not
  what's advertised, and it'll degrade on large repos.
- **Config hierarchy** — `CLAUDE.md` documents a 4-level precedence (CLI → project
  `.msrch/config.toml` → user → defaults). In reality only `load_global_config()` is
  ever used; `load_from_path` (`config.rs:164`) is defined but never called. Project
  config is completely ignored. There's no merge logic. `msrch config` even shows
  only the global config.
- **Retry logic** — `EmbeddingConfig.max_retries` (default 3) is never used;
  `embedding.rs:52` says "Basic retry logic could be added here, simplified for POC."
  Given the batch-upload failures the debug flag was added to chase, real
  retry-with-backoff would be high-value.
- **`max_file_size_mb`** — configured (default 10) but never enforced. `crawler.rs:41`
  even comments "Can also check file size here if needed." Large files are read fully
  into memory.
- **"Upsert chunks by ID (idempotent)"** — `db.rs:64` `upsert_chunks` actually calls
  `table.add()`, which is an **append**, not an upsert. It only works because
  `index.rs` deletes old chunk IDs first. But that delete is a soft `warn!` on failure
  (`index.rs:88`), so a failed delete leaves duplicate vectors with no error. And if
  the process dies between delete and add, you lose data. Consider LanceDB's real
  merge/upsert, or at minimum make the delete failure fatal.

---

## Design issues worth a decision

### 7. `ignore_patterns` config is effectively broken
`crawler.rs:53-68` does a naive `path_str.contains(pattern.trim_end_matches('/'))`.
Two problems: (a) glob patterns like `*.pyc` can never match (no path contains a
literal `*`), and (b) substring matching over-matches — `venv` matches `solvent/`,
`myvenv_notes.py`, etc. You're really relying entirely on the `ignore` crate's
`.gitignore` handling. Use `ignore`'s `OverrideBuilder` / `globset` and delete the
hand-rolled check.

### 8. Double-indexing of nested items
In Rust an `impl_item` is emitted as a chunk *and* its child `function_item`s are
emitted again (`chunker.rs:303-308`); same for Python classes+methods, JS classes, Go
types. The method bodies get embedded twice, inflating index size and letting one file
dominate results. Maybe intentional (multi-granularity retrieval) — but it should be a
conscious, documented choice, not a side effect. At minimum, dedup by file in results
(already done in `Similar` but not in `search`).

### 9. `chunk_index` looks like a line number but isn't
Output is `file_path:chunk_index` (`search.rs:160,175`). Users will read `foo.rs:3` as
line 3. Since you have byte offsets from tree-sitter (`node.start_byte()`) and could
compute line numbers, storing a real start line would make results far more useful
(and clickable in editors).

### 10. Duplicated `find_index_root` walk-up logic in three places
`search.rs:48`, plus inlined copies in `main.rs:167` (Stats) and `main.rs:302`
(Similar). The `Manifest`/`FileMetadata` structs are also re-declared inside the Stats
arm (`main.rs:155`) instead of reused from `index.rs`. Extract a small
`index_discovery` module and share the types.

### 11. `main.rs` is doing too much
Stats and Similar are ~100-line inline command bodies with embedding calls, index
discovery, and formatting. Move each command into its module (the pattern already
exists with `index`/`search`). `main` should dispatch, not implement.

---

## Performance

### 12. `cl100k_base()` is rebuilt per file, sometimes twice
Called in `chunk_file` (`chunker.rs:769`) and again in `chunk_with_treesitter`
(`chunker.rs:206`) — for every file. Building the BPE tokenizer is not cheap. Cache it
once (`LazyLock` or a field on `Chunker`). Easy, meaningful indexing speedup.

### 13. Batch embedding is fully sequential
`index.rs:154`, one `await` per batch. With network-bound embedding calls,
`futures::stream::buffer_unordered` with a small concurrency limit would cut indexing
time substantially. `futures` is already a dependency.

### 14. Whole-corpus buffering
`chunks_to_embed` holds every chunk (including full content strings) in memory before
embedding starts (`index.rs:110`). Combined with #13, streaming
file→chunk→embed→upsert would bound memory and start network I/O earlier.

**Note on tokenizer choice:** `cl100k_base` is the GPT tokenizer, not mxbai-embed's
(BERT-style) tokenizer, so token counts are approximate. Fine for chunk sizing, just
be aware the 512 boundary isn't exact for the actual model.

---

## Minor / housekeeping

- `db.rs:86` `payload.as_object().unwrap()` and `main.rs:336`
  `embeddings...next().unwrap()` — panic on malformed/empty input. Low risk given
  controlled callers, but `CLAUDE.md` says "Never use `unwrap()` in production paths."
- `main.rs:203` `_total_chunks_in_manifest` is computed and never used.
- `main.rs:247` "Est. tokens: chunk_count * 256" — magic number, and `max_chunk_tokens`
  defaults to 512, so the estimate is off by ~2x. You have `token_count` on each chunk;
  sum real counts.
- `--debug` only exists on the `Index` subcommand; query/similar/stats can't enable
  logging.
- Untracked `node_modules/`, `package.json`, `package-lock.json` in a Rust repo (from
  git status) — looks accidental; gitignore or remove them.
- Test coverage is chunker-only. The search/index/db pipeline — including the
  incremental delete-before-add invariant that `CLAUDE.md` calls "critical" — has no
  tests. That invariant is exactly where a regression test belongs.

---

## What I'd fix first

1. **min_similarity threshold (#1)** and **error exit codes (#4)** — small, and
   actively misleading right now.
2. **Store/use tree-sitter `context` (#2)** — the hard part is already done; the
   result is just being dropped.
3. **Fix or delete `reindex` (#3)** and the **broken `ignore_patterns` (#7)**.
4. **Reconcile docs with reality** (HNSW, config hierarchy, retries) — either
   implement or update `CLAUDE.md` so it stops describing features that aren't there.
5. **Cache the tokenizer (#12)** — one-line-ish win.

The threshold fix, error-code fix, and tokenizer caching are quick and
self-contained. The `context` storage change touches the DB schema and needs a
reindex, so scope that one separately.
