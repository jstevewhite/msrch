# Config Relocation + --min-similarity — Design (Housekeeping)

*Approved 2026-07-16. Targets release 0.5.0. No index schema change (stays v5).*

## Purpose

Two user-requested changes plus pending housekeeping: (1) the global config
moves to `~/.config/msrch/config.toml` on every platform instead of confy's
macOS Application Support path; (2) a per-query `--min-similarity` / `-m`
flag over the config-only `query.min_similarity`; (3) riders — commit the
pending README accuracy review, add the missing MIT LICENSE, release 0.5.0.

## 1. Global config relocation (core: `config.rs`)

**Path resolution (all platforms):**
`$XDG_CONFIG_HOME/msrch/config.toml` when `XDG_CONFIG_HOME` is set and
non-empty; otherwise `~/.config/msrch/config.toml` (home from
`std::env::home_dir()`; if the home directory can't be determined, warn and
use defaults — never panic).

**Loading semantics unchanged:** missing file → defaults silently; malformed
file → the existing `load_global_config_or_default` warning + defaults.
`#[serde(default)]` tolerance is untouched. All callers keep going through
`Config::load_global_config_or_default()` / `load_for_index()` — the change
is internal to config.rs.

**One-time migration (copy, never move):** at global-config load, if the new
path does not exist AND the legacy confy path
(`~/Library/Application Support/rs.msrch/config.toml`) exists: create
`~/.config/msrch/`, copy the file, print one stderr notice
(`migrated global config to <new path> (old file left in place)`).
The old file is never modified or deleted. If the copy fails (permissions,
read-only volume): warn to stderr and read the legacy file directly for this
run — migration failure is never fatal and never loses the working config.
On Linux the legacy confy path is `~/.config/msrch/config.toml` already —
old == new, migration is a structural no-op.

**Dependency:** `confy` is removed from the workspace entirely (its only use
is the one `confy::load` call). Global-config parsing reuses the same
`fs::read_to_string` + `toml::from_str` pattern the project config uses.

**Testability:** path resolution is a pure function taking injectable
`xdg: Option<&str>` and `home: Option<&Path>` (same injectable pattern as
`dates::resolve_with_now`); migration is a function over explicit
`(new_path, old_path)` arguments exercised against tempdirs (copy happens,
notice-worthy states, copy-failure fallback, new-exists no-op).

**Out of scope:** Windows-specific legacy path migration (not a deployment
target); a `--config` CLI flag; any change to project-config discovery.

## 2. `--min-similarity` / `-m` (CLI + core)

- `SearchOptions.min_similarity: Option<f32>` — `None` → config's
  `query.min_similarity` (identical fallback pattern to `limit`).
- `Searcher::search` uses `opts.min_similarity.unwrap_or(config.query.min_similarity)`
  where `min_score` is computed today. No other search behavior changes.
- CLI: `--min-similarity <F>` with `-m` short, in both the global (implicit
  query) and `query` subcommand forms, mirroring the existing dual-definition
  pattern. Value parser rejects values outside `0.0..=1.0` with a message
  stating the accepted range. Help text: "Minimum similarity score (0.0-1.0);
  overrides query.min_similarity".
- Docs: README's Query Options gains the flag (replacing the "config-only"
  note added by the accuracy review); AGENTS-SNIPPET gains one example line.

## 3. Riders

- **First commit on the branch:** the pending uncommitted README accuracy
  edits (external Claude Code review), committed as-is with attribution in
  the message body.
- **LICENSE:** standard MIT text, `Copyright (c) 2026 Steve White`, at repo
  root; `license = "MIT"` added to `[workspace.package]` with
  `license.workspace = true` in both crate manifests.
- **Docs:** README quick-start + configuration sections and CLAUDE.md's
  config lines updated to the new canonical path (drop "via confy");
  CHANGELOG 0.5.0 entry covering relocation (with the auto-copy migration
  note), the new flag, and the license. Version 0.4.0 → 0.5.0; tag v0.5.0 on
  main after merge per policy.

## Error handling

- Unparseable / out-of-range `--min-similarity` → clap parse error, non-zero
  exit, message states the 0.0–1.0 range.
- Config migration and loading are never fatal: every failure path degrades
  to a warning plus defaults (or the legacy file), and queries proceed.

## Testing

- Path resolver: XDG set/unset/empty; home fallback; injectable inputs so no
  test mutates process env.
- Migration: legacy→new copy happens once; new-exists → no-op; legacy absent
  → no-op; copy-failure → legacy read fallback (simulate with an unwritable
  target dir).
- Round-trip: a config written at the new path loads with correct overrides
  (reuses existing tolerant-parse tests' style).
- `--min-similarity`: parse accept/reject/boundary (0.0, 1.0, 1.5, -0.1,
  garbage); implicit-form parse test; SearchOptions default None.
- Full workspace suite green throughout; clippy no new warnings.

## Success criteria

- On this Mac: first post-upgrade run prints the migration notice once;
  `msrch config` shows the same effective values as before; the file at
  `~/.config/msrch/config.toml` is authoritative thereafter.
- `msrch "q" -m 0.2` visibly loosens results vs `-m 0.9` on a live index.
- `cargo tree | grep confy` is empty; LICENSE exists; `msrch --version`
  reports 0.5.0 / schema v5.
