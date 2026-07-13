# Implementation Plan: P0 Index Durability

**Date:** 2026-07-11  
**Status:** Implemented on branch `fix/index-durability` (v1: per-file commit)  
**Scope:** Two P0 items from `grok-review.md`

| ID | Issue | Goal |
|----|--------|------|
| **P0-a** | Soft delete failures (`warn!` then continue) | Deletes for modified/deleted files must be fatal |
| **P0-b** | Partial index run corrupts the store | Index runs must be crash/retry-safe: no orphan vectors, no silent duplicates |

These are one durability story, not two independent patches. Implement them together.

---

## 1. Problem statement

### Current pipeline (`index.rs::Indexer::index`)

```
scan all files
  ├─ unchanged → copy manifest entry into new_manifest_entries
  ├─ modified  → delete_by_ids(old)  [warn on failure] → chunk → push to chunks_to_embed
  └─ new       → chunk → push to chunks_to_embed
delete chunks for removed files   [warn on failure]
if chunks empty → write full manifest → return
embed in cross-file batches of batch_size
  └─ each batch: embed → table.add()  [append, not true upsert]
write full manifest once at end
```

### Failure modes

| Scenario | DB state | Manifest | Next `index` | Result |
|----------|----------|----------|--------------|--------|
| **A.** Delete fails, continue | Old vectors remain | Eventually gets new IDs | New IDs appended; old IDs never removed | **Duplicates** |
| **B.** Delete succeeds; embed batch 1 OK; batch 2 fails | Partial new vectors; old gone | Unchanged (old IDs) | Deletes old IDs (noop); re-embeds all; **appends** again | **Orphans + duplicates** from batch 1 |
| **C.** All embeds OK; process dies before manifest write | Full new vectors; old deleted | Unchanged | Same as B | **Duplicates** |
| **D.** Multi-file batch mid-`add()` crash | Partial rows for some files | Unchanged | Same recovery path | **Orphans / duplicates** |

Root causes:

1. **`table.add()` is append-only** — re-running work without a guaranteed pre-delete of *current* rows for that file creates duplicates.
2. **Manifest is the only source of “which IDs belong to this file”** — written only after the entire run; partial success is invisible to the next run.
3. **Delete errors are non-fatal** — the pipeline proceeds into the duplicate-producing path.

Invariant we need:

> For every path present in the manifest, the set of vectors in LanceDB with that `file_path` equals exactly that entry’s `chunk_ids`.  
> For every path *not* in the manifest, zero vectors with that `file_path` exist.  
> After any failed `index` (or crash), a subsequent successful `index` restores the invariant without manual wipe.

---

## 2. Design decision

### Options considered

| Option | Idea | Pros | Cons |
|--------|------|------|------|
| **A. Full wipe on any failure** | Catch errors → remove `index.db` + reset manifest | Simple | Destroys good work; bad UX on flaky network |
| **B. Strict per-file pipeline** | For each dirty file: delete → embed → add → patch manifest | Easy to reason about; fine-grained progress | Many small embed API calls if files are tiny |
| **C. File-grouped batches + path-based pre-delete** | Batch several dirty files (by chunk count); before each file unit, delete by `file_path`; commit manifest only for fully finished files | Keeps batching; recovery is clean | Slightly more code |
| **D. True LanceDB merge/upsert by id** | Rely on DB upsert semantics | Ideal long-term | Still need ordering + manifest rules; API/capability check |

### Recommendation: **C, with B as the first incremental step if needed**

**Primary mechanism: delete-by-`file_path` before writing any new vectors for a file.**

That cleans:

- prior chunk IDs from the last successful run, and  
- orphan rows left by a failed run (unknown IDs not in the manifest).

**Commit unit: one or more complete files** whose chunks have all been embedded and appended successfully; then patch those files into the in-memory manifest and **atomically rewrite** `manifest.json`.

Cross-file embedding batches remain allowed, but:

- A file’s chunks must not be split across “commit units” in a way that leaves the file half-written without a path-delete on retry (simplest rule: **never commit a file until all of its chunks are in the DB**).
- On any failure after some files in a multi-file batch were written, either:
  - **Preferred:** only `add` after *all* embeds for the batch succeed, then update manifest for all files in the batch; if `add` fails mid-batch (Lance may not be half-row atomic across files), next run still path-deletes those dirty files; or  
  - **Safer / simpler v1:** process **one file at a time** (option B), then add multi-file batching as an optimization once tests pass.

**Suggested ship order:**

1. **v1 (P0 minimum):** per-file commit + fatal deletes + path-based pre-delete + atomic manifest write.  
2. **v1.1 (optional same PR if small):** multi-file embed batches that only include whole files and commit the whole group after a single successful `add`.

Do **not** ship soft-delete-only without path-based recovery — that only fixes scenario A, not B/C/D.

---

## 3. Target end-state algorithm

```text
load manifest; migrate schema if needed
connect DB
crawl files → set F

# Working manifest starts as empty; we rebuild it intentionally.
working = Manifest { version: SCHEMA_VERSION, files: {} }

# 1) Unchanged files: keep as-is, no DB touch
for path in F:
  if manifest has path and mtime unchanged:
    working.files[path] = manifest.files[path]

# 2) Deleted files: remove vectors, then drop from store of record
for path in manifest.files.keys() - F:
  delete_by_file_path(path)?          # FATAL
  # do not add to working
atomic_write_manifest(working ∪ still-present-unchanged)?  
# (see note: may batch writes; at least write after all deletes, before dirty work)

# 3) Dirty / new files
dirty = F - working.files.keys()   # new or mtime-changed
for file in dirty:                 # v1: one file at a time
  content = read; if unreadable skip (policy: leave old entry? see open questions)
  chunks = chunk_file(...)
  delete_by_file_path(file)?       # FATAL — clears old + orphans
  if chunks non-empty:
    ensure collection init on first embed
    for each embed batch of this file's chunks:
      embeddings = embed(...)?     # FATAL
      upsert_chunks(...)?          # FATAL
  working.files[file] = FileMetadata { mtime, chunk_ids }
  atomic_write_manifest(working)?  # after each file

done
```

### Properties

| Property | How |
|----------|-----|
| Fatal deletes | `?` / `return Err` — no `warn!` continue |
| No orphans after retry | Next run always `delete_by_file_path` before re-add for dirty files |
| No “success” without manifest | Manifest entry written only after that file’s vectors are in DB |
| Crash mid-file | That file may have partial vectors; not in working (or still old entry if we only update working after full success). Next run treats as dirty (mtime) or we force dirty if we detect incomplete — see recovery note below |
| Crash after vectors, before manifest | Old manifest entry remains; next run sees dirty (mtime) or same mtime with stale IDs → **must still path-delete**. If mtime unchanged and we only reindex on mtime change, **orphans stick forever**. |

### Critical recovery detail: mtime-unchanged orphans

Scenario C variant: file was dirty, we deleted by path, added new vectors, **crashed before manifest update**. On disk:

- Vectors: new IDs for `file_path`  
- Manifest: still old mtime + old chunk_ids (or no entry)

If the file’s mtime still equals the **old** manifest mtime… wait: we only process dirty when `existing.modified_at != current`. After a failed run:

- Manifest still has **previous successful** mtime T0.  
- File still has mtime T1 (the change that triggered reindex).  
- Next run: T1 != T0 → dirty again → path-delete → re-embed. **OK.**

If the file is **new** (not in manifest):

- Vectors partially written; no manifest entry.  
- Next run: still “new” → path-delete (clears orphans) → re-embed. **OK.**

If we had already written manifest with T1 and then crashed mid-*next* file: prior files are consistent. **OK.**

So mtime-based dirty detection + path-delete on dirty is sufficient **without** a separate journal, as long as we never write the new mtime into the manifest until vectors for that version are fully committed.

**Do not** write `new_manifest_entries` with new mtime/IDs into memory and then bulk-write at the end without intermediate commits — that’s the current bug. Intermediate atomic writes after each successful file are required for progress and for bounding rework.

---

## 4. Concrete code changes

### 4.1 `src/db.rs`

#### Add `delete_by_file_path`

```rust
pub async fn delete_by_file_path(&self, file_path: &str) -> Result<()> {
    // no-op if table missing
    // Escape single quotes in path for SQL filter: ' → ''
    // filter: file_path = '<escaped>'
    table.delete(&filter).await?;
}
```

Notes:

- Paths may contain `'` (rare) and Windows backslashes — escape for Lance filter syntax.
- Prefer storing a **canonical string form** for `file_path` consistently (same as upsert payload); document that comparison is exact string match on the column.
- Optional later: `delete_by_file_paths(&[String])` with `file_path IN (...)` for deleted-file batching — not required for v1.

#### Keep `delete_by_ids`?

- Still useful for tests or bulk cleanup by ID.  
- **Indexer v1 should prefer `delete_by_file_path`** for durability.  
- Can leave `delete_by_ids` as-is for now; call sites in `index.rs` switch away.

#### `upsert_chunks` naming

- Optional rename comment: “append chunks (caller must delete prior rows for this file)”.  
- No need for true merge in P0 if path-delete is always called first.

#### Delete failure behavior

- Propagate errors with `.context("Failed to delete vectors for …")?`.  
- Empty ID list / missing table: `Ok(())` (already true for `delete_by_ids`).

### 4.2 `src/index.rs` — restructure `Indexer::index`

#### Helpers (private)

| Helper | Responsibility |
|--------|----------------|
| `load_manifest(path) -> Manifest` | Open + parse; default empty; ensure `version` stamped when writing |
| `atomic_write_manifest(path, &Manifest) -> Result<()>` | Write `manifest.json.tmp` then `rename` over `manifest.json` (portable: on Unix rename is atomic; on Windows may need remove+rename — use a small helper) |
| `process_dirty_file(...)` | path-delete → chunk → embed batches → upsert → return `FileMetadata` |
| `embed_and_store_chunks(...)` | Shared batching loop for one file’s chunks; initializes collection once |

#### Control flow rewrite (high level)

Replace the two-phase “collect all chunks / embed all / write once” with the algorithm in §3.

Remove:

```rust
if let Err(e) = db.delete_by_ids(...).await {
    warn!(...);
}
```

everywhere in this path.

#### Progress UX

- Progress bar over **dirty + deleted** work units (or all files with a “skip” fast path for unchanged).  
- Message: `Indexing path/to/file.rs` / `Removing deleted: ...`.  
- Print summary at end: `N unchanged, M updated, D removed, E errors` (errors = hard fail).

#### Manifest version

- Every write sets `manifest.version = SCHEMA_VERSION`.  
- After migrate wipe, write empty/versioned manifest early so a crash mid-rebuild doesn’t leave a zero-version manifest pointing at a wiped DB inconsistently (optional polish: write stamped empty manifest immediately after migrate).

#### Unreadable / non-UTF8 files

Current: skip with `continue`, **and today that still left them out of `new_manifest_entries`**, so they looked **deleted** and old chunks were removed. That is a separate footgun.

**P0 policy (document and implement explicitly):**

- If path was in the old manifest and read fails: **keep previous `FileMetadata`** (do not delete vectors; do not treat as deleted). Log a warning.  
- If path is new and read fails: skip; no vectors.  
- Ensures flaky permissions don’t wipe indexed content.

### 4.3 Atomic manifest write

```rust
fn atomic_write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let file = fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&file, manifest)?;
        file.sync_all()?; // optional but good for durability
    }
    fs::rename(&tmp, path).context("Failed to replace manifest.json")?;
    Ok(())
}
```

Call after:

- all deleted-file cleanups (one write), and  
- each successful dirty file (or each multi-file commit group in v1.1).

### 4.4 No schema version bump required

- No new Lance columns.  
- Behavior change only; existing indexes remain valid.  
- Optional: document in changelog that interrupted index runs should re-run `msrch index` (they already should).

---

## 5. Failure matrix (acceptance criteria)

| # | Injected failure | Expected outcome | Next successful `index` |
|---|------------------|------------------|-------------------------|
| 1 | `delete_by_file_path` errors | Run aborts non-zero; manifest unchanged for that file | Retries delete; completes |
| 2 | Embed API fails on file N | Files 1..N-1 committed in manifest+DB; file N no new manifest entry; possible partial vectors only if add already ran — v1 does embed-all-then-add per file to minimize this | Path-delete file N; completes |
| 3 | `upsert_chunks` / `add` fails | Same as 2 | Path-delete; completes |
| 4 | Kill process after add, before manifest write | DB has new vectors; manifest old | Dirty by mtime; path-delete clears; re-embed; single copy |
| 5 | Kill after manifest write for file N | File N consistent | Unchanged skip |
| 6 | File deleted on disk | Vectors removed; not in manifest | Stable |
| 7 | Delete of removed file fails | Abort; removed file still in manifest | Retry |

**Hard requirement:** after any of (2–4) followed by a full successful run, `count(vectors where file_path = P) == len(manifest.files[P].chunk_ids)` for every P, and no extra paths in DB (spot-check via search or a debug count-by-path helper in tests).

---

## 6. Testing plan

### 6.1 Unit tests (no network)

**`db` (integration-style with temp Lance dir):**

- `delete_by_file_path` removes only rows for that path.  
- Re-add after delete → row count equals new chunk count (no growth beyond).  
- Missing table / empty path → Ok.

**`index` helpers:**

- `atomic_write_manifest` produces valid JSON; crash-safe rename (write tmp, rename, read back).  
- Unreadable file keeps prior manifest entry (temp dir fixture).

### 6.2 Durability / orchestration tests

Prefer tests that **mock the embedder** so CI doesn’t need a live model.

Introduce a thin trait or test hook:

```rust
// e.g. in embedding.rs or index.rs
#[async_trait]
trait Embedder {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
}
```

Production: `EmbeddingClient` implements it.  
Tests: `FakeEmbedder` returns deterministic unit vectors (fixed dim, e.g. 8).

Then:

| Test | Setup | Assert |
|------|--------|--------|
| `reindex_file_replaces_vectors` | Index file A; change content + mtime; reindex | Chunk count for A stable; content/search reflects new text; IDs changed |
| `failed_embed_keeps_prior_files` | Two dirty files; fake embedder fails on 2nd | Manifest has file1 only (or file1 updated); file2 old or absent; re-run succeeds both |
| `orphan_cleanup_after_partial_add` | Manually `add` orphan rows for path P not matching manifest IDs; run index on dirty P | Orphans gone; only new IDs remain |
| `delete_failure_is_fatal` | Inject failing DB wrapper / invalid path filter if feasible | `index()` returns Err; no success message |
| `deleted_file_removes_vectors` | Index; delete file from disk; reindex | No rows for path; not in manifest |

If mocking DB is hard, temp-dir LanceDB + fake embedder is enough for almost all of the above.

### 6.3 Manual QA checklist

```bash
msrch index .
# kill -9 mid "Embedding..."
msrch index .   # should complete; stats chunk count not inflated 2x
msrch "some query"
# modify one file, index again — score/content updates, stats roughly stable
```

Compare `msrch stats` chunk counts before/after interrupted run + recovery: must not roughly double.

---

## 7. Implementation steps (ordered PR tasks)

### Task 1 — DB path delete + tests  
**Files:** `src/db.rs`  
**Do:**

1. Add `delete_by_file_path` with quoting/escaping.  
2. Unit/integration test with temp DB: insert two paths, delete one, count.  
3. No behavior change to indexer yet.

### Task 2 — Atomic manifest write + unreadable-file policy  
**Files:** `src/index.rs`  
**Do:**

1. `atomic_write_manifest`.  
2. Fix deleted-vs-unreadable: unreadable existing files keep prior metadata.  
3. Small unit tests for write + policy.

### Task 3 — Fatal deletes + per-file commit loop (core P0)  
**Files:** `src/index.rs` (main rewrite)  
**Do:**

1. Restructure `index()` per §3 / §4.2.  
2. Remove all `warn!`+continue on delete.  
3. Per dirty file: `delete_by_file_path` → embed → upsert → update working manifest → atomic write.  
4. Deleted files: path-delete → omit from working → write.  
5. Preserve progress bar + endpoint print + schema migration order (migrate before connect).  
6. Ensure `collection_initialized` still happens on first successful embed.

### Task 4 — Fake embedder + durability tests  
**Files:** `src/embedding.rs` and/or `src/index.rs`, tests  
**Do:**

1. Extract embed call behind a testable seam (trait or `Indexer` generic — keep it minimal).  
2. Add tests from §6.2.  
3. `cargo test` green.

### Task 5 — Docs touch-up (same PR or follow-up)  
**Files:** `CLAUDE.md` / `docs/msrch_HLD.md` incremental section, maybe README  
**Do:**

1. Document invariant: delete-by-path before replace; manifest committed per file.  
2. Note that `upsert_chunks` is append and relies on pre-delete.  
3. Do **not** claim multi-file atomic transactions beyond “retry is safe.”

### Task 6 (optional v1.1) — Multi-file embed batching  
**Do only after Task 4 is green.**

1. Accumulate dirty files until `sum(chunks) >= batch_size` or end.  
2. Path-delete all files in the group first.  
3. Embed/upsert all chunks (may be multiple API calls).  
4. On full success, update all group entries + one manifest write.  
5. On failure, leave manifest without those updates; path-delete on retry repairs DB.

---

## 8. Out of scope (explicit non-goals for this P0)

- Embedding retries / backoff (`max_retries`) — P2 from review; complementary but separate.  
- Concurrent embed streams — can amplify partial-write races; add only after durability is solid.  
- Lance native merge-insert / true upsert by primary key — nice follow-up, not required if path-delete works.  
- ANN/HNSW indexes.  
- Oversized tree-sitter drop bug (P1).  
- Changing chunk ID strategy (UUIDs remain fine).  
- Journal/WAL file — unnecessary if path-delete + mtime dirty rules hold.

---

## 9. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Path string mismatch (relative vs absolute) between crawler and delete filter | Always use the same `PathBuf`/`display` form as stored in payload today; add test that round-trips path from crawl → upsert → delete |
| Filter injection / special chars in paths | Escape `'`; test path with quote and spaces |
| Manifest write spam (large repos, many dirty files) | Accept for v1; v1.1 groups commits; or write every N files with documented tradeoff |
| Slower indexing (per-file API) | Measure; batch whole files in v1.1; still correct first |
| Windows rename atomicity | Use `std::fs::rename`; if needed, `remove` target then rename; document |
| Double progress / UX change | Mention in PR description |

---

## 10. Definition of done

- [ ] No `warn!` + continue on delete paths in `index.rs`.  
- [ ] Dirty/new file path: always `delete_by_file_path` before `add`.  
- [ ] Manifest entry for a file updated only after its vectors are fully written.  
- [ ] Manifest writes are atomic (temp + rename).  
- [ ] Unreadable previously-indexed files do not wipe their index entries.  
- [ ] Tests in §6.2 cover replace, partial failure, orphan cleanup, deleted files.  
- [ ] Manual kill-mid-index + re-run does not roughly double chunk count.  
- [ ] `cargo test` / `cargo clippy` clean for touched code (no new pedantic debt required beyond existing baseline).

---

## 11. Suggested PR shape

**Single PR preferred** (Tasks 1–4): “fix: durable per-file indexing and fatal vector deletes”

Rationale: Task 3 without Task 1 is incomplete; Task 1 without Task 3 doesn’t fix production. Split only if the rewrite is too large for review — then:

1. PR1: `delete_by_file_path` + tests  
2. PR2: indexer restructure + fake embedder + durability tests  

---

## 12. Pseudocode sketch (v1 core loop)

```rust
// Inside Indexer::index after crawl + migrate + db connect:

let mut working = Manifest {
    version: SCHEMA_VERSION,
    files: HashMap::new(),
};

// Unchanged
for path in &files {
    if let Some(meta) = manifest.files.get(path) {
        if meta.modified_at == fs::metadata(path)?.modified()? {
            working.files.insert(path.clone(), meta.clone());
        }
    }
}

// Deleted
for (path, _meta) in &manifest.files {
    if !files.iter().any(|f| f == path) {
        db.delete_by_file_path(&path_string(path)).await
            .with_context(|| format!("Failed to delete vectors for removed file {}", path.display()))?;
    }
}
atomic_write_manifest(&manifest_path, &working)?;

// Dirty / new
for path in files.iter().filter(|p| !working.files.contains_key(*p)) {
    let modified = fs::metadata(path)?.modified()?;
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            if let Some(prev) = manifest.files.get(path) {
                warn!("Skipping unreadable {}: {} (keeping previous index entry)", path.display(), e);
                working.files.insert(path.clone(), prev.clone());
                atomic_write_manifest(&manifest_path, &working)?;
            }
            continue;
        }
    };

    let chunks = chunker.chunk_file(path, &content)?;
    db.delete_by_file_path(&path_string(path)).await
        .with_context(|| format!("Failed to delete stale vectors for {}", path.display()))?;

    if !chunks.is_empty() {
        embed_and_store(&db, &embedder, &chunks, &mut collection_initialized).await?;
    }

    working.files.insert(
        path.clone(),
        FileMetadata {
            modified_at: modified,
            chunk_ids: chunks.iter().map(|c| c.id).collect(),
        },
    );
    atomic_write_manifest(&manifest_path, &working)?;
}
```

---

## 13. Open questions (resolve during implementation if needed)

1. **Empty files:** chunk to zero chunks → path-delete + manifest entry with empty `chunk_ids`. Prefer yes (marks file as seen).  
2. **Embed dimension change mid-project:** already handled by schema/model ops outside this plan; path-delete doesn’t fix wrong dim.  
3. **Should `reindex` stay wipe-all?** Yes — full rebuild remains valid; durability is about incremental `index`.  
4. **Trait vs callback for FakeEmbedder:** prefer smallest change that allows tests; avoid large refactor of `EmbeddingClient` public API if a `#[cfg(test)]` inject point on `Indexer` is enough.

---

## 14. Effort estimate

| Task | Rough effort |
|------|----------------|
| Task 1 DB path delete + tests | S |
| Task 2 Atomic manifest + unreadable policy | S |
| Task 3 Indexer rewrite | M |
| Task 4 Fake embedder + durability tests | M |
| Task 5 Docs | S |
| Task 6 Multi-file batching (optional) | S–M |

**Overall P0 (Tasks 1–4):** roughly a focused day for someone familiar with the module; two if including thorough tests and review polish.

---

## 15. Relation to review P0 wording

| Review item | Plan coverage |
|-------------|----------------|
| P0 durable indexing (per-file commit or fatal mid-run recovery) | §2–§3 path-delete + per-file manifest commit |
| P0 fatal delete-on-reindex failures | §4.2 remove soft-delete; all deletes `?` |

Both land in the same PR series; fatal deletes alone are insufficient without path-based recovery and deferred manifest updates.
