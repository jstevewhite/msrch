# Config Relocation + --min-similarity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Global config at `~/.config/msrch/config.toml` on all platforms (XDG-aware, copy-once migration, confy removed), a `--min-similarity`/`-m` per-query flag, plus the README-review commit and MIT LICENSE riders (spec: `docs/superpowers/specs/2026-07-16-config-path-min-similarity-design.md`). Release 0.5.0.

**Architecture:** config.rs gains a pure path resolver (injectable `xdg`/`home`, same pattern as `dates::resolve_with_now`) and a copy-once migration function over explicit paths; `load_global_config` switches from confy to the same read+toml pattern the project overlay uses. The flag threads through `SearchOptions.min_similarity` exactly like `limit`.

**Tech Stack:** Rust 2024 workspace. Removes `confy`; adds nothing. Unix-only path logic (`$HOME`; Windows out of scope per spec).

## Global Constraints

- Path rule, verbatim: `$XDG_CONFIG_HOME/msrch/config.toml` when `XDG_CONFIG_HOME` is set and non-empty; else `$HOME/.config/msrch/config.toml`. Legacy path: `$HOME/Library/Application Support/rs.msrch/config.toml` (exists only on macOS; gated by `exists()`, no `cfg!` needed).
- Migration: copy, never move/modify/delete the legacy file; one stderr notice on successful copy; copy failure → warn + read the legacy file this run; never fatal. New-path-exists or legacy-absent → no migration action.
- Missing config file → defaults silently, and (behavior change vs confy) we NO LONGER auto-create a default config file on first run — document in CHANGELOG.
- `--min-similarity` accepts `0.0..=1.0` inclusive; rejection message states the range; `-m` short; both CLI forms; `None` → `config.query.min_similarity`.
- `cargo test --workspace` green at every commit (baseline 84); clippy no new warnings (~24-26 baseline); no production `unwrap()`; `anyhow` + `.context()`.
- Version 0.4.0 → 0.5.0 in Task 4; tag v0.5.0 on main after merge (controller/human). No schema change.
- The working tree starts with UNCOMMITTED README.md accuracy edits — Task 1 commits them FIRST, unmodified. Do not touch README content in Task 1.

## File Structure (end state)

```
LICENSE                      # NEW — MIT, Copyright (c) 2026 Steve White
Cargo.toml                   # license under [workspace.package]; confy removed; 0.5.0 (Task 4)
crates/core/Cargo.toml       # license.workspace = true; confy removed
crates/cli/Cargo.toml        # license.workspace = true
crates/core/src/config.rs    # resolver + migration + confy-free load_global_config
crates/core/src/search.rs    # SearchOptions.min_similarity; min_score fallback
crates/cli/src/main.rs       # --min-similarity/-m in both forms
README.md / CLAUDE.md / docs/AGENTS-SNIPPET.md / CHANGELOG.md   # Task 4
```

---

### Task 1: Riders — commit the README review, add LICENSE + manifest fields

**Files:**
- Commit (pre-existing edits, unmodified): `README.md`
- Create: `LICENSE`
- Modify: root `Cargo.toml`, `crates/core/Cargo.toml`, `crates/cli/Cargo.toml`

**Interfaces:** none.

- [ ] **Step 1: Commit the pending README edits exactly as they are**

Verify `git status --short` shows exactly ` M README.md` (nothing else). Then:

```bash
git add README.md
git commit -m "docs: README accuracy pass — remove ghost flags, fix paths and claims

External Claude Code review verified the README against the actual CLI and
config code: removed never-implemented --threshold/--endpoint/--index flags,
corrected the global config path and install command, fixed the HNSW/ANN
claim (search is an exact cosine scan), full-URL endpoint examples, repo
links, and added Document Extraction docs."
```

(No Claude Code footer — the content is the user's external review pass, recorded as-is.)

- [ ] **Step 2: Create `LICENSE`** (standard MIT text, verbatim):

```text
MIT License

Copyright (c) 2026 Steve White

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 3: Manifest fields**

Root `Cargo.toml` `[workspace.package]` gains:

```toml
license = "MIT"
```

Both `crates/core/Cargo.toml` and `crates/cli/Cargo.toml` `[package]` sections gain:

```toml
license.workspace = true
```

- [ ] **Step 4: Verify + commit**

Run: `cargo build -q` (metadata validates) and `cargo test --workspace 2>&1 | grep "test result"` — 84 green.

```bash
git add LICENSE Cargo.toml crates/core/Cargo.toml crates/cli/Cargo.toml
git commit -m "chore: add MIT LICENSE and license manifest field

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: Config relocation — XDG path, copy-once migration, drop confy

**Files:**
- Modify: `crates/core/src/config.rs` (resolver, migration, `load_global_config` rework)
- Modify: root `Cargo.toml` + `crates/core/Cargo.toml` (remove confy)

**Interfaces:**
- Produces: `Config::load_global_config() -> anyhow::Result<Self>` (signature changes from `Result<Self, confy::ConfyError>`; the only caller is `load_global_config_or_default`, whose match arms already fit).
- Produces (crate-internal, testable): `resolve_global_config_path(xdg: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> Option<PathBuf>`; `migrate_legacy_config(new_path: &Path, legacy_path: &Path) -> PathBuf` (returns the path to read this run); `Config::load_global_config_from(path: &Path) -> anyhow::Result<Self>`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/config.rs` tests module:

```rust
#[test]
fn resolve_global_config_path_prefers_nonempty_xdg() {
    use std::ffi::OsStr;
    assert_eq!(
        resolve_global_config_path(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/u"))),
        Some(PathBuf::from("/xdg/msrch/config.toml"))
    );
    // Empty XDG_CONFIG_HOME is treated as unset (XDG spec):
    assert_eq!(
        resolve_global_config_path(Some(OsStr::new("")), Some(OsStr::new("/home/u"))),
        Some(PathBuf::from("/home/u/.config/msrch/config.toml"))
    );
    assert_eq!(
        resolve_global_config_path(None, Some(OsStr::new("/home/u"))),
        Some(PathBuf::from("/home/u/.config/msrch/config.toml"))
    );
    assert_eq!(resolve_global_config_path(None, None), None);
}

#[test]
fn migrate_legacy_config_copies_once_and_prefers_new() {
    let dir = tempfile::tempdir().unwrap();
    let new_path = dir.path().join("new/config.toml");
    let legacy = dir.path().join("legacy/config.toml");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "[query]\ndefault_limit = 3\n").unwrap();

    // Legacy exists, new absent → copy happens, new path returned, legacy intact.
    let read = migrate_legacy_config(&new_path, &legacy);
    assert_eq!(read, new_path);
    assert_eq!(
        std::fs::read_to_string(&new_path).unwrap(),
        "[query]\ndefault_limit = 3\n"
    );
    assert!(legacy.exists(), "legacy file must never be removed");

    // New exists → no re-copy, even when contents differ.
    std::fs::write(&new_path, "[query]\ndefault_limit = 9\n").unwrap();
    let read = migrate_legacy_config(&new_path, &legacy);
    assert_eq!(read, new_path);
    assert_eq!(
        std::fs::read_to_string(&new_path).unwrap(),
        "[query]\ndefault_limit = 9\n",
        "existing new-path config must not be overwritten"
    );

    // Legacy absent → new path returned untouched.
    let lonely = dir.path().join("lonely/config.toml");
    assert_eq!(migrate_legacy_config(&lonely, &dir.path().join("nope.toml")), lonely);
}

#[test]
fn migrate_legacy_config_copy_failure_falls_back_to_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy.toml");
    std::fs::write(&legacy, "[query]\ndefault_limit = 3\n").unwrap();
    // Make the new path's parent an ordinary FILE so create_dir_all fails.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"file, not dir").unwrap();
    let new_path = blocker.join("config.toml");

    let read = migrate_legacy_config(&new_path, &legacy);
    assert_eq!(read, legacy, "copy failure must fall back to reading legacy");
    assert!(legacy.exists());
}

#[test]
fn load_global_config_from_reads_missing_as_default_and_file_as_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("nope.toml");
    let config = Config::load_global_config_from(&absent).unwrap();
    assert_eq!(config.query.default_limit, Config::default().query.default_limit);

    let present = dir.path().join("config.toml");
    std::fs::write(&present, "[query]\ndefault_limit = 4\n").unwrap();
    let config = Config::load_global_config_from(&present).unwrap();
    assert_eq!(config.query.default_limit, 4);

    let malformed = dir.path().join("bad.toml");
    std::fs::write(&malformed, "not [valid").unwrap();
    assert!(Config::load_global_config_from(&malformed).is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core 'resolve_global|migrate_legacy|load_global_config_from' 2>&1 | tail -3`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement**

In `crates/core/src/config.rs`, add `use std::ffi::OsStr;` and `use std::path::PathBuf;` as needed, then replace the `load_global_config` function with:

```rust
/// Global config path: `$XDG_CONFIG_HOME/msrch/config.toml` when set and
/// non-empty, else `$HOME/.config/msrch/config.toml`. Pure for testability.
fn resolve_global_config_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(xdg) = xdg
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("msrch").join("config.toml"));
    }
    home.map(|h| PathBuf::from(h).join(".config").join("msrch").join("config.toml"))
}

/// Where confy (pre-0.5.0) kept the global config on macOS. On Linux the
/// confy path coincides with the new path, so this only ever exists on macOS.
fn legacy_global_config_path(home: Option<&OsStr>) -> Option<PathBuf> {
    home.map(|h| {
        PathBuf::from(h)
            .join("Library/Application Support/rs.msrch")
            .join("config.toml")
    })
}

/// Copy-once migration from the legacy confy location. Returns the path this
/// run should read. Never modifies or removes the legacy file; every failure
/// degrades to reading the legacy path directly.
fn migrate_legacy_config(new_path: &Path, legacy_path: &Path) -> PathBuf {
    if new_path.exists() || !legacy_path.exists() {
        return new_path.to_path_buf();
    }
    if let Some(dir) = new_path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!(
            "warning: could not create {} ({e}); reading legacy config at {}",
            dir.display(),
            legacy_path.display()
        );
        return legacy_path.to_path_buf();
    }
    match std::fs::copy(legacy_path, new_path) {
        Ok(_) => {
            eprintln!(
                "migrated global config to {} (old file left in place at {})",
                new_path.display(),
                legacy_path.display()
            );
            new_path.to_path_buf()
        }
        Err(e) => {
            eprintln!(
                "warning: could not migrate global config to {} ({e}); reading {}",
                new_path.display(),
                legacy_path.display()
            );
            legacy_path.to_path_buf()
        }
    }
}
```

and inside `impl Config`:

```rust
    /// Load the global config from `~/.config/msrch/config.toml` (XDG-aware),
    /// after a one-time copy migration from the legacy confy location.
    /// A missing file yields defaults; unlike confy, no file is auto-created.
    pub fn load_global_config() -> anyhow::Result<Self> {
        let xdg = std::env::var_os("XDG_CONFIG_HOME");
        let home = std::env::var_os("HOME");
        let new_path = resolve_global_config_path(xdg.as_deref(), home.as_deref())
            .context("cannot determine home directory for global config")?;
        let read_path = match legacy_global_config_path(home.as_deref()) {
            Some(legacy) => migrate_legacy_config(&new_path, &legacy),
            None => new_path,
        };
        Self::load_global_config_from(&read_path)
    }

    /// Read a global config file: absent → defaults; malformed → Err (the
    /// caller `load_global_config_or_default` turns that into warn+defaults).
    fn load_global_config_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read global config at {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse global config at {}", path.display()))
    }
```

(`load_global_config_or_default` needs no changes — its `Err(e)` arm already prints the warning; anyhow's `{e}` display keeps it one line. Note `resolve_global_config_path`/`migrate_legacy_config` are free functions, `load_global_config_from` is an associated fn — matching the tests. Rust 2024 let-chains are fine, the codebase already uses them.)

Remove confy: delete `confy = "2.0.0"` from root `Cargo.toml` `[workspace.dependencies]` and `confy.workspace = true` from `crates/core/Cargo.toml`. Run `cargo build -q` to refresh Cargo.lock (commit the lock change).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 88 total green (84 + 4 new). Also: `cargo tree -p msrch-core 2>/dev/null | grep -c confy` → 0.

- [ ] **Step 5: Manual smoke (THIS machine has a real legacy config)**

The live legacy file is `~/Library/Application Support/rs.msrch/config.toml` and `~/.config/msrch/` does not exist yet. Run: `cargo run -q -- config 2>&1 | head -5`.
Expected: the migration notice line on stderr, then the effective config with the user's real endpoint (a Tailscale IP, not the default). Then run it AGAIN — no notice the second time. Verify `cat ~/.config/msrch/config.toml` matches the legacy file, and the legacy file is untouched. Put actual outputs in your report. DO NOT delete or edit either config file.

- [ ] **Step 6: Clippy + commit**

Run: `cargo clippy 2>&1 | tail -3` — no new warnings.

```bash
git add -A
git commit -m "feat: global config at ~/.config/msrch (XDG-aware); drop confy

Copy-once migration from the legacy macOS confy path (old file left in
place; failures degrade to reading it). Missing config now yields defaults
without auto-creating a file."
```

(Append the standard Claude Code footer to the commit message.)

---

### Task 3: `--min-similarity` / `-m`

**Files:**
- Modify: `crates/core/src/search.rs` (`SearchOptions.min_similarity`, `min_score` fallback, extend the default test)
- Modify: `crates/cli/src/main.rs` (flag in both forms, parser, parse tests)

**Interfaces:**
- Produces: `SearchOptions.min_similarity: Option<f32>` (pub field).
- Produces (cli-internal): `fn parse_min_similarity(s: &str) -> Result<f32, String>`.

- [ ] **Step 1: Write the failing tests**

`crates/core/src/search.rs` — EXTEND the existing `search_options_default_is_all_off` test with one assertion:

```rust
    assert!(opts.min_similarity.is_none());
```

`crates/cli/src/main.rs` tests module — add:

```rust
#[test]
fn min_similarity_flag_parses_in_both_forms_and_validates_range() {
    let cli = Cli::try_parse_from(["msrch", "q", "-m", "0.7"]).expect("short form parses");
    assert_eq!(cli.min_similarity, Some(0.7));
    let cli = Cli::try_parse_from(["msrch", "query", "q", "--min-similarity", "0.0"])
        .expect("subcommand form parses at lower bound");
    match cli.command {
        Some(Commands::Query { min_similarity, .. }) => assert_eq!(min_similarity, Some(0.0)),
        other => panic!("expected Query, got {other:?}"),
    }
    for bad in ["1.5", "-0.1", "abc"] {
        let err = Cli::try_parse_from(["msrch", "q", "-m", bad]).unwrap_err();
        assert!(
            err.to_string().contains("between 0.0 and 1.0"),
            "range in message for {bad}: {err}"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch 'min_similarity_flag' 2>&1 | tail -3`
Expected: compile error — field/parser not defined.

- [ ] **Step 3: Implement**

`crates/core/src/search.rs` — add to `SearchOptions` (after `use_rerank`):

```rust
    /// Minimum similarity score (0.0–1.0); `None` uses the config's
    /// `query.min_similarity`.
    pub min_similarity: Option<f32>,
```

and change the `min_score` line in `search()`:

```rust
        let min_score = opts
            .min_similarity
            .unwrap_or(self.config.query.min_similarity);
```

`crates/cli/src/main.rs` — parser (near `version_string`):

```rust
/// clap value parser for `--min-similarity`: a float in 0.0..=1.0.
fn parse_min_similarity(s: &str) -> Result<f32, String> {
    let v: f32 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a number between 0.0 and 1.0"))?;
    if (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err(format!("'{s}' is out of range; must be between 0.0 and 1.0"))
    }
}
```

Cli struct (after `rerank`, matching the global pattern):

```rust
    /// Minimum similarity score (0.0-1.0); overrides query.min_similarity
    #[arg(long, short = 'm', global = true, value_parser = parse_min_similarity)]
    min_similarity: Option<f32>,
```

Query variant (after its `rerank` field):

```rust
        /// Minimum similarity score (0.0-1.0); overrides query.min_similarity
        #[arg(long, short = 'm', value_parser = parse_min_similarity)]
        min_similarity: Option<f32>,
```

Implicit-query construction adds `min_similarity: cli.min_similarity,`; the Query match arm binds `min_similarity` and the `SearchOptions` literal gains `min_similarity: *min_similarity,`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: 89 total green.

- [ ] **Step 5: Manual smoke + clippy + commit**

Run in the repo root: `cargo run -q -- "chunking" -m 0.9 -f filename 2>/dev/null | wc -l` vs `-m 0.1` — the 0.9 run returns fewer (possibly zero) paths. Report actual counts.

Run: `cargo clippy 2>&1 | tail -3` — no new warnings.

```bash
git add -A
git commit -m "feat: --min-similarity/-m per-query threshold flag"
```

(Append the standard Claude Code footer.)

---

### Task 4: Docs + release 0.5.0

**Files:**
- Modify: `README.md`, `CLAUDE.md`, `docs/AGENTS-SNIPPET.md`, `CHANGELOG.md`, root `Cargo.toml`

**Interfaces:** none.

- [ ] **Step 1: Version bump**

Root `Cargo.toml`: `version = "0.4.0"` → `version = "0.5.0"`; `cargo build -q` refreshes Cargo.lock (commit it).

- [ ] **Step 2: README**

- Quick Start + Configuration sections: the global config path is now
  `~/.config/msrch/config.toml` on ALL platforms (`$XDG_CONFIG_HOME/msrch/config.toml` when set). Remove the macOS Application Support bullets the accuracy review added; add one migration sentence: "Upgrading from ≤0.4: an existing config in the old macOS location is copied over automatically on first run (the old file is left in place)."
- Query Options: replace the "minimum-similarity threshold is set via config; there is no CLI flag" paragraph with a flag example:
  ```bash
  # Per-query minimum similarity (0.0-1.0; default from config)
  msrch "config parsing" --min-similarity 0.7    # or: -m 0.7
  ```
  and add `--min-similarity`/`-m` to the CLI-flags precedence line in Configuration.

- [ ] **Step 3: CLAUDE.md**

- "Config Hierarchy" line 3 → `3. Global User config: ~/.config/msrch/config.toml ($XDG_CONFIG_HOME-aware)`.
- "Config Loading" global bullet → `- Global: ~/.config/msrch/config.toml (XDG-aware; one-time copy migration from the legacy macOS confy path; missing file → defaults, no auto-creation)`.
- CLI-flags example line in Config Hierarchy gains `--min-similarity`.

- [ ] **Step 4: AGENTS-SNIPPET.md**

Add one line to the filter examples block:

```
    msrch "config parsing" -m 0.7                     # per-query similarity floor
```

- [ ] **Step 5: CHANGELOG entry** (top, above [0.4.0])

```markdown
## [0.5.0] - 2026-07-16

### Added
- `--min-similarity` / `-m`: per-query minimum similarity (0.0–1.0),
  overriding config's `query.min_similarity`.
- `LICENSE` (MIT) and the `license` manifest field.

### Changed
- **Global config now lives at `~/.config/msrch/config.toml` on every
  platform** (`$XDG_CONFIG_HOME/msrch/config.toml` when set). On macOS an
  existing config in the legacy confy location
  (`~/Library/Application Support/rs.msrch/`) is copied over automatically on
  first run — the old file is left in place. A missing config now yields
  defaults without auto-creating a file. The `confy` dependency is gone.

No index schema change — existing indexes work as-is.
```

- [ ] **Step 6: Full suite + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` — 89 green.

```bash
git add -A
git commit -m "chore: release 0.5.0 — XDG config path + --min-similarity (see CHANGELOG)"
```

(Append the standard Claude Code footer.)

**Post-merge (controller/human):** on main, `git tag v0.5.0 && git push --tags`, `make install`, `msrch --version` → `0.5.0 (index schema v5, …)`; first `msrch` run on this Mac prints the migration notice once; `msrch config` shows the same endpoint as before.

---

## Self-review notes

- Spec coverage: path rule + XDG-empty handling (Task 2 resolver + tests), copy-once migration with all four states + failure fallback (Task 2), confy removal (Task 2), no-auto-create behavior change documented (Task 2 impl + Task 4 CHANGELOG), `--min-similarity` both forms + range validation + fallback (Task 3), riders (Task 1), docs + 0.5.0 (Task 4). ✓
- Type consistency: `resolve_global_config_path(Option<&OsStr>, Option<&OsStr>) -> Option<PathBuf>`, `migrate_legacy_config(&Path, &Path) -> PathBuf`, `load_global_config_from(&Path) -> anyhow::Result<Self>`, `parse_min_similarity(&str) -> Result<f32, String>` — used identically in tests and impls. ✓
- The Task 2 smoke uses the REAL user config — instructions explicitly forbid deleting/editing either file, and migration never mutates the legacy file by construction. ✓
- Float equality in the Task 3 parse test (`Some(0.7)`) is exact-representable and comes straight from `parse::<f32>` — no arithmetic, safe to compare. ✓
