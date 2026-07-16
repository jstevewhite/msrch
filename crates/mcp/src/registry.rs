//! Named index roots, built once at startup. Clients address indexes by
//! name only — the server never resolves client-supplied filesystem paths.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug)]
pub struct IndexRegistry {
    entries: Vec<IndexEntry>,
}

impl IndexRegistry {
    /// Build from `--index` flags: `name=path` (split on the FIRST '=') or a
    /// bare `path` whose name is the directory basename. Every root must
    /// contain `.msrch/`.
    pub fn from_flags(flags: &[String]) -> Result<Self> {
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(flags.len());
        for flag in flags {
            let (name, raw_path) = match flag.split_once('=') {
                Some((name, path)) => (name.to_string(), path.to_string()),
                None => {
                    let path = PathBuf::from(flag);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .with_context(|| format!("cannot derive an index name from '{flag}'"))?;
                    (name, flag.clone())
                }
            };
            let root = PathBuf::from(&raw_path);
            if !root.is_dir() {
                bail!("index root '{raw_path}' does not exist");
            }
            if !root.join(".msrch").is_dir() {
                bail!(
                    "no .msrch index at '{}' — run 'msrch index .' there first",
                    root.display()
                );
            }
            if entries.iter().any(|e| e.name == name) {
                bail!("duplicate index name '{name}'");
            }
            entries.push(IndexEntry { name, root });
        }
        if entries.is_empty() {
            bail!("no indexes registered");
        }
        Ok(Self { entries })
    }

    /// CLI-identical walk-up discovery from `cwd`; single unnamed root whose
    /// name is the root directory's basename.
    pub fn discover(cwd: &Path) -> Result<Self> {
        let root = msrch_core::index::find_index_root(cwd)
            .context("No .msrch index found in directory tree")?;
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string());
        Ok(Self {
            entries: vec![IndexEntry { name, root }],
        })
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// `None` with one entry → that entry; `None` with several → error
    /// listing names; `Some(name)` → match or error listing valid names.
    pub fn resolve(&self, index: Option<&str>) -> Result<&IndexEntry> {
        match index {
            Some(name) => self.entries.iter().find(|e| e.name == name).with_context(|| {
                format!(
                    "unknown index '{name}'; registered indexes: {}",
                    self.names().join(", ")
                )
            }),
            None if self.entries.len() == 1 => Ok(&self.entries[0]),
            None => bail!(
                "multiple indexes registered — pass 'index' with one of: {}",
                self.names().join(", ")
            ),
        }
    }

    fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_index_root(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join(".msrch")).unwrap();
        root
    }

    #[test]
    fn from_flags_parses_named_and_bare_forms() {
        let dir = tempfile::tempdir().unwrap();
        let a = make_index_root(&dir, "alpha");
        let b = make_index_root(&dir, "reports-2026");
        let flags = vec![
            format!("work={}", a.display()),
            b.display().to_string(),
        ];
        let reg = IndexRegistry::from_flags(&flags).unwrap();
        assert_eq!(reg.entries().len(), 2);
        assert_eq!(reg.entries()[0].name, "work");
        assert_eq!(reg.entries()[0].root, a);
        assert_eq!(reg.entries()[1].name, "reports-2026", "bare path → basename");
    }

    #[test]
    fn from_flags_rejects_missing_index_and_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();
        let no_index = dir.path().join("plain");
        std::fs::create_dir_all(&no_index).unwrap();
        let err = IndexRegistry::from_flags(&[no_index.display().to_string()]).unwrap_err();
        assert!(format!("{err:#}").contains("no .msrch index"), "{err:#}");

        let a = make_index_root(&dir, "one");
        let b = make_index_root(&dir, "two");
        let err = IndexRegistry::from_flags(&[
            format!("same={}", a.display()),
            format!("same={}", b.display()),
        ])
        .unwrap_err();
        assert!(format!("{err:#}").contains("duplicate index name"), "{err:#}");
    }

    #[test]
    fn discover_walks_up_like_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let root = make_index_root(&dir, "proj");
        let nested = root.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let reg = IndexRegistry::discover(&nested).unwrap();
        assert_eq!(reg.entries().len(), 1);
        assert_eq!(reg.entries()[0].root, root);
        assert_eq!(reg.entries()[0].name, "proj");

        let bare = tempfile::tempdir().unwrap();
        let err = IndexRegistry::discover(bare.path()).unwrap_err();
        assert!(format!("{err:#}").contains("No .msrch index"), "{err:#}");
    }

    #[test]
    fn resolve_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let a = make_index_root(&dir, "alpha");
        let b = make_index_root(&dir, "beta");
        let one = IndexRegistry::from_flags(&[a.display().to_string()]).unwrap();
        assert_eq!(one.resolve(None).unwrap().name, "alpha");
        assert_eq!(one.resolve(Some("alpha")).unwrap().name, "alpha");

        let two = IndexRegistry::from_flags(&[
            a.display().to_string(),
            b.display().to_string(),
        ])
        .unwrap();
        let err = two.resolve(None).unwrap_err();
        assert!(
            format!("{err:#}").contains("alpha") && format!("{err:#}").contains("beta"),
            "ambiguous resolve lists names: {err:#}"
        );
        let err = two.resolve(Some("nope")).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown index 'nope'")
                && format!("{err:#}").contains("alpha"),
            "unknown name lists valid ones: {err:#}"
        );
    }
}
