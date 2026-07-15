# Document Extraction Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** msrch indexes HTML, text-layer PDF, and .docx documents by extracting text before chunking (spec: `docs/superpowers/specs/2026-07-15-document-extraction-design.md`).

**Architecture:** One new module `crates/core/src/extract.rs` owns all format knowledge, emitting markdown-ish text (HTML/docx) or prose (PDF). Two hooks: the crawler's binary filter whitelists extractable extensions, and the indexer's read site calls `extract()` instead of `read_to_string` for them. The chunker only learns an extension→FileType mapping; the CLI is untouched.

**Tech Stack:** Rust 2024 workspace. New core deps (exact versions, cargo-add-verified): `dom_smoothie = "0.18.0"`, `scraper = "0.27.0"`, `pdf-extract = "0.12.0"`, `zip = "8.6.0"`, `quick-xml = "0.41.0"`.

## Global Constraints

- `cargo test --workspace` green at every commit; `cargo clippy` introduces no new warnings (baseline ~26 pre-existing).
- Skip-file warnings are `eprintln!` to stderr (visible without `--debug`), phrased `warning: skipping <path>: <reason>`; unexpected errors propagate as `anyhow::Result` with `.context()`. No `unwrap()` in production paths (tests may unwrap).
- Spec thresholds, verbatim: HTML degenerate fallback when main-content text `< 200` chars OR `< 5%` of whole-page text; PDF graphics-only skip when trimmed text `< 200` chars; size gate = `max_file_size_mb` (existing `ChunkingConfig` field, default 10) × 1024 × 1024 bytes, checked via `fs::metadata` before parsing.
- Extraction failures are per-file (warn + skip), never abort the index run.
- `SCHEMA_VERSION` becomes 5; release version becomes 0.3.0 (workspace.package), with CHANGELOG entry and `git tag v0.3.0` **applied on main after merge**, per CLAUDE.md's Versioning & Releases policy.
- No new config keys; no CLI changes; scope is exactly html/htm/xhtml, pdf, docx.
- Crate-API adaptation clause: the code blocks below are written against dom_smoothie 0.18 / scraper 0.27 / zip 8.6 / quick-xml 0.41 APIs from documentation knowledge. If a method name or signature differs at compile time, adapt **mechanically** (same contract, same behavior) and note the adaptation in your report. If the crate cannot fulfill the stated contract at all (e.g. dom_smoothie exposes no cleaned-article HTML), STOP and report BLOCKED.

## File Structure (end state)

```
crates/core/
├── src/
│   ├── extract.rs        # NEW — is_extractable, extract, per-format extractors, html_to_markdown
│   ├── crawler.rs        # +3 lines: whitelist extractable extensions past null-byte check
│   ├── chunker.rs        # determine_file_type: html/docx → Markdown, pdf → Prose
│   ├── index.rs          # read-site swap; SCHEMA_VERSION 5
│   └── lib.rs            # + pub mod extract;
└── tests/
    ├── fixtures/
    │   ├── saved-page.html      # nav + article, committed text
    │   ├── nav-only.html        # degenerate page, committed text
    │   ├── text-layer.pdf       # generated once via cupsfilter, committed binary
    │   └── graphics-only.pdf    # generated once via sips, committed binary
    └── extraction_pipeline.rs   # NEW — end-to-end crawl→extract→chunk test
```

---

### Task 1: `extract.rs` scaffold — dispatcher, size gate, and the HTML→markdown walker

**Files:**
- Create: `crates/core/src/extract.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod extract;` in alphabetical position)
- Modify: root `Cargo.toml` `[workspace.dependencies]` and `crates/core/Cargo.toml` `[dependencies]`

**Interfaces:**
- Produces: `extract::is_extractable(path: &Path) -> bool`
- Produces: `extract::extract(path: &Path, max_bytes: u64) -> anyhow::Result<Option<String>>` (dispatcher; per-format fns are stubs completed in Tasks 2–4)
- Produces (crate-internal): `extract::html_to_markdown(html: &str) -> String` — used by Task 2 for both readability output and whole-page fallback.

- [ ] **Step 1: Add dependencies**

Root `Cargo.toml`, `[workspace.dependencies]` (alphabetical):

```toml
dom_smoothie = "0.18.0"
pdf-extract = "0.12.0"
quick-xml = "0.41.0"
scraper = "0.27.0"
zip = "8.6.0"
```

`crates/core/Cargo.toml` `[dependencies]` (alphabetical):

```toml
dom_smoothie.workspace = true
pdf-extract.workspace = true
quick-xml.workspace = true
scraper.workspace = true
zip.workspace = true
```

Run: `cargo build -q` — expect clean (new deps compile; first build is minutes).

- [ ] **Step 2: Write the failing tests**

Create `crates/core/src/extract.rs` containing ONLY the tests module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn is_extractable_matches_exactly_the_supported_extensions() {
        for good in ["a.html", "b.HTM", "c.xhtml", "d.pdf", "e.DOCX"] {
            assert!(is_extractable(&PathBuf::from(good)), "{good}");
        }
        for bad in ["a.md", "b.rs", "c.txt", "d.doc", "e.pptx", "noext"] {
            assert!(!is_extractable(&PathBuf::from(bad)), "{bad}");
        }
    }

    #[test]
    fn extract_skips_oversize_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.html");
        std::fs::write(&p, "x".repeat(2048)).unwrap();
        // max_bytes below the file size → size gate trips, Ok(None)
        assert!(extract(&p, 1024).unwrap().is_none());
    }

    #[test]
    fn html_to_markdown_maps_headings_blocks_and_drops_script() {
        let html = r#"<html><head><title>t</title><script>var x=1;</script>
            <style>.a{}</style></head><body>
            <h1>Top Title</h1><p>First paragraph.</p>
            <h2>Section</h2><p>Second  paragraph
            with   wrapped whitespace.</p>
            <ul><li>alpha</li><li>beta</li></ul>
            <table><tr><td>c1</td><td>c2</td></tr></table>
            </body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("# Top Title"), "h1 → #: {md}");
        assert!(md.contains("## Section"), "h2 → ##: {md}");
        assert!(md.contains("First paragraph."));
        assert!(md.contains("Second paragraph with wrapped whitespace."), "whitespace normalized: {md}");
        assert!(md.contains("- alpha"), "list items bulleted: {md}");
        assert!(md.contains("c1 | c2"), "table cells joined: {md}");
        assert!(!md.contains("var x"), "script dropped: {md}");
        assert!(!md.contains(".a{{}}") && !md.contains(".a{}"), "style dropped: {md}");
        assert!(!md.contains('<'), "no tags leak: {md}");
        assert!(!md.contains("\n\n\n"), "blank lines collapsed: {md}");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p msrch-core extract:: 2>&1 | tail -5`
Expected: compile error — `is_extractable`, `extract`, `html_to_markdown` not defined.

- [ ] **Step 4: Implement the scaffold**

Add above the tests module in `crates/core/src/extract.rs`:

```rust
//! Text extraction for document formats (HTML, text-layer PDF, docx).
//!
//! One module owns all format knowledge. The indexer calls [`extract`] for
//! paths where [`is_extractable`] is true, instead of reading the file as
//! UTF-8. `Ok(None)` means "skip this file; the reason was already warned to
//! stderr" — no text layer, over the size cap, or unparseable.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const EXTRACTABLE_EXTS: &[&str] = &["html", "htm", "xhtml", "pdf", "docx"];

/// True for extensions this module handles (case-insensitive).
pub fn is_extractable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTRACTABLE_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Extract indexable text from a supported document.
///
/// `Ok(Some(text))` → index it. `Ok(None)` → skipped (reason already printed).
/// `Err` → unexpected I/O failure; the caller warns and skips the file.
pub fn extract(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let meta = fs::metadata(path).context("stat file for extraction")?;
    if meta.len() > max_bytes {
        eprintln!(
            "warning: skipping {}: {} bytes exceeds max_file_size_mb",
            path.display(),
            meta.len()
        );
        return Ok(None);
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" | "xhtml" => extract_html(path),
        "pdf" => extract_pdf(path),
        "docx" => extract_docx(path),
        other => anyhow::bail!("extract() called for non-extractable extension: {other}"),
    }
}

// Per-format extractors are completed in later tasks.
fn extract_html(_path: &Path) -> Result<Option<String>> {
    anyhow::bail!("extract_html: implemented in Task 2")
}
fn extract_pdf(_path: &Path) -> Result<Option<String>> {
    anyhow::bail!("extract_pdf: implemented in Task 4")
}
fn extract_docx(_path: &Path) -> Result<Option<String>> {
    anyhow::bail!("extract_docx: implemented in Task 3")
}

/// Convert an HTML string to markdown-ish plain text: h1–h6 → `#` lines,
/// block elements separated by blank lines, list items bulleted, table cells
/// joined with ` | `, script/style/head dropped, whitespace normalized.
pub(crate) fn html_to_markdown(html: &str) -> String {
    use scraper::{Html, Node};

    fn heading_level(tag: &str) -> Option<usize> {
        match tag {
            "h1" => Some(1),
            "h2" => Some(2),
            "h3" => Some(3),
            "h4" => Some(4),
            "h5" => Some(5),
            "h6" => Some(6),
            _ => None,
        }
    }

    fn is_block(tag: &str) -> bool {
        matches!(
            tag,
            "p" | "div" | "section" | "article" | "ul" | "ol" | "li" | "table" | "tr"
                | "blockquote" | "pre" | "header" | "footer" | "main" | "aside" | "figure"
                | "figcaption" | "nav"
        )
    }

    fn ensure_block_break(out: &mut String) {
        while out.ends_with(' ') {
            out.pop();
        }
        if !out.is_empty() && !out.ends_with("\n\n") {
            while out.ends_with('\n') {
                out.pop();
            }
            out.push_str("\n\n");
        }
    }

    fn emit(node: ego_tree::NodeRef<Node>, out: &mut String) {
        match node.value() {
            Node::Text(t) => {
                let cleaned = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !cleaned.is_empty() {
                    if !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) && !out.ends_with("# ") {
                        out.push(' ');
                    }
                    out.push_str(&cleaned);
                }
            }
            Node::Element(el) => {
                let tag = el.name();
                if matches!(tag, "script" | "style" | "noscript" | "template" | "head") {
                    return;
                }
                if let Some(level) = heading_level(tag) {
                    ensure_block_break(out);
                    out.push_str(&"#".repeat(level));
                    out.push(' ');
                } else if is_block(tag) {
                    ensure_block_break(out);
                    if tag == "li" {
                        out.push_str("- ");
                    }
                }
                for child in node.children() {
                    emit(child, out);
                }
                if tag == "br" {
                    out.push('\n');
                } else if matches!(tag, "td" | "th") {
                    out.push_str(" | ");
                } else if heading_level(tag).is_some() || is_block(tag) {
                    ensure_block_break(out);
                }
            }
            _ => {
                for child in node.children() {
                    emit(child, out);
                }
            }
        }
    }

    let doc = Html::parse_document(html);
    let mut out = String::new();
    emit(doc.tree.root(), &mut out);

    // Trim trailing " | " artifacts at line ends and collapse 3+ newlines.
    let cleaned = out
        .lines()
        .map(|l| l.trim_end().trim_end_matches('|').trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    let mut collapsed = String::with_capacity(cleaned.len());
    let mut newlines = 0;
    for ch in cleaned.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                collapsed.push(ch);
            }
        } else {
            newlines = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}
```

Note: `ego_tree` is scraper's re-exported tree crate; if `ego_tree::NodeRef` isn't nameable directly, add `ego_tree` via scraper's re-export (`scraper::ego_tree::NodeRef`) — mechanical adaptation.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p msrch-core extract::`
Expected: 3 passed.

- [ ] **Step 6: Full suite + clippy + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` — all green (52 existing + 3 new).
Run: `cargo clippy 2>&1 | tail -3` — no new warnings.

```bash
git add -A
git commit -m "feat: extraction scaffold — dispatcher, size gate, HTML→markdown walker"
```

---

### Task 2: HTML extractor — readability + degenerate fallback

**Files:**
- Modify: `crates/core/src/extract.rs` (replace the `extract_html` stub; add tests)
- Create: `crates/core/tests/fixtures/saved-page.html`
- Create: `crates/core/tests/fixtures/nav-only.html`

**Interfaces:**
- Consumes: `html_to_markdown` from Task 1.
- Produces: working `extract_html(path) -> Result<Option<String>>` behind the Task 1 dispatcher.

- [ ] **Step 1: Create the fixtures**

`crates/core/tests/fixtures/saved-page.html`:

```html
<!DOCTYPE html>
<html><head><title>Quarterly Report — Q2 2026</title></head><body>
<nav><ul><li><a href="/">Home</a></li><li><a href="/reports">Reports</a></li>
<li><a href="/about">About</a></li><li><a href="/contact">Contact</a></li></ul></nav>
<aside>Related links: <a href="/q1">Q1 report</a> <a href="/q3">Q3 preview</a></aside>
<article>
<h1>Quarterly Report</h1>
<p>This quarter the team shipped the workspace refactor and the document
extraction pipeline. Search quality on the internal document repository
improved measurably, and the agent workflow now covers PDF and Word sources
in addition to markdown. This paragraph exists to give the readability
algorithm enough body text to identify the article as main content.</p>
<h2>Highlights</h2>
<p>Semantic search over mixed corpora is now the default workflow for weekly
reporting, with reranking enabled on the home lab endpoint. Incremental
indexing keeps the corpus fresh without manual intervention, and the new
extraction stage removes markup noise from saved web pages entirely.</p>
<h2>Numbers</h2>
<table><tr><th>Metric</th><th>Value</th></tr>
<tr><td>Indexed files</td><td>24</td></tr>
<tr><td>Chunks</td><td>381</td></tr></table>
</article>
<footer>Copyright 2026 — internal use only — unsubscribe — privacy policy</footer>
</body></html>
```

`crates/core/tests/fixtures/nav-only.html`:

```html
<!DOCTYPE html>
<html><head><title>Reports index</title></head><body>
<nav><ul>
<li><a href="/r/2026-01">January</a></li><li><a href="/r/2026-02">February</a></li>
<li><a href="/r/2026-03">March</a></li><li><a href="/r/2026-04">April</a></li>
<li><a href="/r/2026-05">May</a></li><li><a href="/r/2026-06">June</a></li>
</ul></nav>
</body></html>
```

- [ ] **Step 2: Write the failing tests**

Add to the tests module in `crates/core/src/extract.rs`:

```rust
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn html_readability_keeps_article_drops_nav() {
    let text = extract(&fixture("saved-page.html"), u64::MAX)
        .unwrap()
        .expect("article page must extract");
    assert!(text.contains("# Quarterly Report"), "article heading kept: {text}");
    assert!(text.contains("workspace refactor"), "body kept");
    assert!(text.contains("Indexed files | 24"), "table kept: {text}");
    assert!(!text.contains("unsubscribe"), "footer boilerplate dropped: {text}");
    assert!(!text.contains('<'), "no tags leak");
}

#[test]
fn html_degenerate_page_falls_back_to_whole_page_text() {
    let text = extract(&fixture("nav-only.html"), u64::MAX)
        .unwrap()
        .expect("nav-only page must still extract via fallback");
    // Readability finds no real article here; whole-page fallback keeps the links' text.
    assert!(text.contains("January"), "fallback preserved nav text: {text}");
    assert!(text.contains("June"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p msrch-core extract:: 2>&1 | tail -5`
Expected: the two new tests FAIL with "extract_html: implemented in Task 2".

- [ ] **Step 4: Implement `extract_html`**

Replace the stub in `crates/core/src/extract.rs`:

```rust
/// Minimum main-content size (chars) below which readability output is
/// considered degenerate and the whole-page text is used instead.
const HTML_MIN_MAIN_CHARS: usize = 200;
/// Main content must be at least this fraction of the whole-page text (5%).
const HTML_MIN_MAIN_FRACTION_DENOM: usize = 20;

fn extract_html(path: &Path) -> Result<Option<String>> {
    let raw = fs::read_to_string(path).context("read html file")?;
    let whole_page = html_to_markdown(&raw);

    let main = readability_markdown(&raw);
    let text = match main {
        Some(m)
            if m.trim().len() >= HTML_MIN_MAIN_CHARS
                && m.trim().len() * HTML_MIN_MAIN_FRACTION_DENOM >= whole_page.trim().len() =>
        {
            m
        }
        _ => whole_page,
    };

    if text.trim().is_empty() {
        eprintln!("warning: skipping {}: no extractable text", path.display());
        return Ok(None);
    }
    Ok(Some(text))
}

/// Readability-style main-content extraction; None when parsing fails or the
/// crate finds no article (callers fall back to whole-page text).
fn readability_markdown(raw: &str) -> Option<String> {
    let mut readability = dom_smoothie::Readability::new(raw, None, None).ok()?;
    let article = readability.parse().ok()?;
    Some(html_to_markdown(article.content.as_ref()))
}
```

(dom_smoothie 0.18: `Readability::new(html, url: Option<&str>, cfg: Option<Config>)`, `parse() -> Result<Article>`, `Article.content` = cleaned article HTML. Adapt mechanically if names differ — the contract is raw HTML in, cleaned article HTML out.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p msrch-core extract::`
Expected: 5 passed. If `html_readability_keeps_article_drops_nav` fails because dom_smoothie *kept* the footer, loosen nothing — check whether the footer text appears inside `article.content` (print it), and if dom_smoothie genuinely includes footers on this fixture, move the footer line inside `<footer>` outside `<article>` is already the case — report DONE_WITH_CONCERNS with the actual extracted text in your report instead of weakening the assertion silently.

- [ ] **Step 6: Full suite + clippy + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` and `cargo clippy 2>&1 | tail -3`.

```bash
git add -A
git commit -m "feat: HTML extraction — readability main content with whole-page fallback"
```

---

### Task 3: docx extractor — zip + quick-xml

**Files:**
- Modify: `crates/core/src/extract.rs` (replace `extract_docx` stub; add helper + tests)

**Interfaces:**
- Produces: working `extract_docx(path) -> Result<Option<String>>`; crate-internal `docx_xml_to_markdown(xml: &str) -> String`.

- [ ] **Step 1: Write the failing tests (with in-test docx builder)**

Add to the tests module:

```rust
/// Build a minimal .docx (zip with word/document.xml) in memory.
fn build_docx(document_xml: &str) -> Vec<u8> {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut z = zip::ZipWriter::new(cursor);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", opts).unwrap();
        z.write_all(document_xml.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    buf
}

fn write_docx(dir: &tempfile::TempDir, name: &str, document_xml: &str) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, build_docx(document_xml)).unwrap();
    p
}

const DOCX_BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
 <w:body>
  <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Report Title</w:t></w:r></w:p>
  <w:p><w:r><w:t>First part,</w:t></w:r><w:r><w:t xml:space="preserve"> second part.</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Details</w:t></w:r></w:p>
  <w:p><w:r><w:t>Before tab</w:t><w:tab/><w:t>after tab.</w:t></w:r></w:p>
  <w:tbl>
   <w:tr><w:tc><w:p><w:r><w:t>Metric</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr>
   <w:tr><w:tc><w:p><w:r><w:t>Files</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>24</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
 </w:body>
</w:document>"#;

#[test]
fn docx_headings_runs_tabs_and_tables_extract() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_docx(&dir, "report.docx", DOCX_BODY);
    let text = extract(&p, u64::MAX).unwrap().expect("docx must extract");
    assert!(text.contains("# Report Title"), "Heading1 → #: {text}");
    assert!(text.contains("## Details"), "Heading2 → ##: {text}");
    assert!(text.contains("First part, second part."), "runs concatenated: {text}");
    assert!(text.contains("Before tab after tab."), "tab → space: {text}");
    assert!(text.contains("Metric | Value"), "table row flattened: {text}");
    assert!(text.contains("Files | 24"), "second row: {text}");
}

#[test]
fn docx_with_no_text_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let empty = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
 <w:body><w:p></w:p></w:body></w:document>"#;
    let p = write_docx(&dir, "empty.docx", empty);
    assert!(extract(&p, u64::MAX).unwrap().is_none(), "empty docx → skip");
}

#[test]
fn docx_that_is_not_a_zip_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("corrupt.docx");
    std::fs::write(&p, b"this is not a zip archive").unwrap();
    assert!(extract(&p, u64::MAX).unwrap().is_none(), "corrupt docx → skip, not Err");
}
```

Also move `zip` to core's `[dev-dependencies]`? No — it stays a regular dependency (production code reads docx archives with it); tests reuse it.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core extract::docx 2>&1 | tail -5`
Expected: FAIL with "extract_docx: implemented in Task 3".

- [ ] **Step 3: Implement**

Replace the stub:

```rust
fn extract_docx(path: &Path) -> Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Err(e).context("open docx"),
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("warning: skipping {}: not a readable docx archive: {e}", path.display());
            return Ok(None);
        }
    };
    let mut xml = String::new();
    match archive.by_name("word/document.xml") {
        Ok(mut entry) => {
            use std::io::Read;
            if let Err(e) = entry.read_to_string(&mut xml) {
                eprintln!("warning: skipping {}: unreadable document.xml: {e}", path.display());
                return Ok(None);
            }
        }
        Err(_) => {
            eprintln!("warning: skipping {}: no word/document.xml", path.display());
            return Ok(None);
        }
    }

    let text = docx_xml_to_markdown(&xml);
    if text.trim().is_empty() {
        eprintln!("warning: skipping {}: no extractable text", path.display());
        return Ok(None);
    }
    Ok(Some(text))
}

/// Map a paragraph style name to a markdown heading level (0 = not a heading).
fn heading_from_style(style: &str) -> usize {
    let s = style.trim();
    if s.eq_ignore_ascii_case("title") {
        return 1;
    }
    let rest = s
        .strip_prefix("Heading")
        .or_else(|| s.strip_prefix("heading"))
        .map(str::trim)
        .unwrap_or("");
    rest.parse::<usize>().ok().filter(|n| (1..=6).contains(n)).unwrap_or(0)
}

/// Stream word/document.xml into markdown-ish text: Heading styles → `#`,
/// runs concatenated, tabs → space, table rows → `cell | cell` lines.
fn docx_xml_to_markdown(xml: &str) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut para = String::new();
    let mut heading = 0usize;
    let mut in_text = false;
    let mut in_row = false;
    let mut row = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"p" if !in_row => {
                    para.clear();
                    heading = 0;
                }
                b"t" => in_text = true,
                b"tr" => {
                    in_row = true;
                    row.clear();
                    para.clear();
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"pStyle" => {
                    if let Ok(Some(attr)) = e.try_get_attribute("w:val") {
                        heading = heading_from_style(&String::from_utf8_lossy(&attr.value));
                    }
                }
                b"tab" => para.push(' '),
                b"br" => para.push('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_text => {
                para.push_str(&t.unescape().unwrap_or_default());
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" if !in_row => {
                    let trimmed = para.trim();
                    if !trimmed.is_empty() {
                        if heading > 0 {
                            out.push_str(&"#".repeat(heading));
                            out.push(' ');
                        }
                        out.push_str(trimmed);
                        out.push_str("\n\n");
                    }
                }
                b"tc" => {
                    let cell = para.trim();
                    if !row.is_empty() {
                        row.push_str(" | ");
                    }
                    row.push_str(cell);
                    para.clear();
                }
                b"tr" => {
                    in_row = false;
                    if !row.trim().is_empty() {
                        out.push_str(row.trim());
                        out.push('\n');
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => {
                log::debug!("docx xml parse stopped early: {e}");
                break;
            }
            _ => {}
        }
    }
    out.trim().to_string()
}
```

(quick-xml 0.41: `Reader::from_str`, `read_event()`, `e.local_name().as_ref() -> &[u8]`, `e.try_get_attribute("w:val")`, `BytesText::unescape()`. Adapt mechanically if signatures differ.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p msrch-core extract::`
Expected: 8 passed.

- [ ] **Step 5: Full suite + clippy + commit**

```bash
git add -A
git commit -m "feat: docx extraction — zip + streaming XML to markdown-ish text"
```

---

### Task 4: PDF extractor — text layer + graphics-only skip

**Files:**
- Modify: `crates/core/src/extract.rs` (replace `extract_pdf` stub; add tests)
- Create: `crates/core/tests/fixtures/text-layer.pdf` (generated, committed)
- Create: `crates/core/tests/fixtures/graphics-only.pdf` (generated, committed)

**Interfaces:**
- Produces: working `extract_pdf(path) -> Result<Option<String>>`.

- [ ] **Step 1: Generate the two PDF fixtures (one-time, committed)**

```bash
cd crates/core/tests/fixtures
# Text-layer PDF via macOS cupsfilter (must yield ≥200 chars of text):
cat > /tmp/fixture.txt <<'EOF'
msrch extraction fixture: quarterly report alpha bravo charlie delta echo.
This document exists to test PDF text-layer extraction in msrch. It contains
several full sentences so that the extracted text comfortably exceeds the two
hundred character graphics-only threshold used by the extractor heuristic.
The quick brown fox jumps over the lazy dog near the riverbank at dawn.
EOF
cupsfilter /tmp/fixture.txt > text-layer.pdf 2>/dev/null
# Graphics-only PDF: 1x1 PNG → PDF via macOS sips:
printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==' | base64 -d > /tmp/pixel.png
sips -s format pdf /tmp/pixel.png --out graphics-only.pdf >/dev/null
ls -la text-layer.pdf graphics-only.pdf
```

Expected: both files exist, each under ~30KB. If `cupsfilter` is unavailable on this macOS version, STOP and report BLOCKED naming the tool (an alternative generator needs a human decision, not improvisation).

- [ ] **Step 2: Write the failing tests**

Add to the tests module:

```rust
#[test]
fn pdf_text_layer_extracts_as_prose() {
    let text = extract(&fixture("text-layer.pdf"), u64::MAX)
        .unwrap()
        .expect("text-layer pdf must extract");
    assert!(text.contains("quarterly report"), "text layer content: {text}");
    assert!(text.trim().len() >= 200, "fixture must exceed threshold: {}", text.len());
}

#[test]
fn pdf_without_text_layer_is_skipped() {
    assert!(
        extract(&fixture("graphics-only.pdf"), u64::MAX).unwrap().is_none(),
        "graphics-only pdf → skip"
    );
}

#[test]
fn pdf_that_is_corrupt_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("corrupt.pdf");
    std::fs::write(&p, b"%PDF-1.4 truncated garbage").unwrap();
    assert!(extract(&p, u64::MAX).unwrap().is_none(), "corrupt pdf → skip, not Err");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p msrch-core extract::pdf 2>&1 | tail -5`
Expected: FAIL with "extract_pdf: implemented in Task 4".

- [ ] **Step 4: Implement**

Replace the stub:

```rust
/// Minimum trimmed text length for a PDF to count as having a text layer.
const PDF_MIN_TEXT_CHARS: usize = 200;

fn extract_pdf(path: &Path) -> Result<Option<String>> {
    let text = match pdf_extract::extract_text(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: skipping {}: failed to parse PDF: {e}", path.display());
            return Ok(None);
        }
    };
    if text.trim().len() < PDF_MIN_TEXT_CHARS {
        eprintln!(
            "warning: skipping {}: no text layer (graphics-only?)",
            path.display()
        );
        return Ok(None);
    }
    Ok(Some(text))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p msrch-core extract::`
Expected: 11 passed. Note: pdf-extract may print its own stderr noise for the corrupt-PDF case; that's the library, not a test failure — but capture it in your report.

- [ ] **Step 6: Full suite + clippy + commit**

```bash
git add -A
git commit -m "feat: PDF text-layer extraction with graphics-only skip"
```

---

### Task 5: Pipeline hooks — crawler, chunker, indexer, SCHEMA_VERSION 5

**Files:**
- Modify: `crates/core/src/crawler.rs` (`is_binary`, ~line 62)
- Modify: `crates/core/src/chunker.rs` (`determine_file_type`, ~line 162; tests ~line 1169)
- Modify: `crates/core/src/index.rs` (read site ~line 260; `SCHEMA_VERSION` ~line 29)
- Create: `crates/core/tests/extraction_pipeline.rs`

**Interfaces:**
- Consumes: `extract::is_extractable`, `extract::extract` (Tasks 1–4), `Crawler::crawl`, `Chunker::chunk_file`, `ChunkingConfig`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/chunker.rs`'s existing `determine_file_type` tests (follow the surrounding assert style):

```rust
#[test]
fn extractable_extensions_route_to_markdown_or_prose() {
    assert_eq!(
        Chunker::determine_file_type(&PathBuf::from("page.html")),
        FileType::Markdown
    );
    assert_eq!(
        Chunker::determine_file_type(&PathBuf::from("page.xhtml")),
        FileType::Markdown
    );
    assert_eq!(
        Chunker::determine_file_type(&PathBuf::from("report.docx")),
        FileType::Markdown
    );
    assert_eq!(
        Chunker::determine_file_type(&PathBuf::from("paper.pdf")),
        FileType::Prose
    );
}
```

Create `crates/core/tests/extraction_pipeline.rs` (integration test — crawl → extract → chunk, no embedding, no network):

```rust
use msrch_core::chunker::Chunker;
use msrch_core::config::{ChunkingConfig, IndexingConfig};
use msrch_core::crawler::Crawler;
use msrch_core::extract;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn crawl_extract_chunk_covers_fixture_corpus() {
    // Stage fixtures in a tempdir so crawler state (.gitignore etc.) is controlled.
    let dir = tempfile::tempdir().unwrap();
    for name in ["saved-page.html", "nav-only.html", "text-layer.pdf", "graphics-only.pdf"] {
        std::fs::copy(fixture_dir().join(name), dir.path().join(name)).unwrap();
    }
    std::fs::write(dir.path().join("notes.md"), "# Notes\n\nplain markdown still works\n").unwrap();

    let crawler = Crawler::new(IndexingConfig::default());
    let files = crawler.crawl(dir.path()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    // The crawler must now surface the binary-format documents:
    assert!(names.contains(&"text-layer.pdf".to_string()), "crawler passes pdf: {names:?}");
    assert!(names.contains(&"graphics-only.pdf".to_string()));
    assert!(names.contains(&"saved-page.html".to_string()));

    let mut chunker = Chunker::new(ChunkingConfig::default());
    let mut all_chunks = Vec::new();
    let mut skipped = Vec::new();
    for file in &files {
        let content = if extract::is_extractable(file) {
            match extract::extract(file, u64::MAX).unwrap() {
                Some(text) => text,
                None => {
                    skipped.push(file.file_name().unwrap().to_string_lossy().to_string());
                    continue;
                }
            }
        } else {
            std::fs::read_to_string(file).unwrap()
        };
        all_chunks.extend(chunker.chunk_file(file, &content).unwrap());
    }

    // Graphics-only PDF was skipped; everything else produced chunks.
    assert!(skipped.contains(&"graphics-only.pdf".to_string()), "skipped: {skipped:?}");
    let html_chunks: Vec<_> = all_chunks
        .iter()
        .filter(|c| c.file_path.ends_with("saved-page.html"))
        .collect();
    assert!(!html_chunks.is_empty(), "html produced chunks");
    // The whole point: no markup in stored content.
    for c in &all_chunks {
        assert!(!c.content.contains("<div"), "tag soup leaked: {}", c.content);
        assert!(!c.content.contains("<html"), "tag soup leaked: {}", c.content);
    }
    let pdf_chunks: Vec<_> = all_chunks
        .iter()
        .filter(|c| c.file_path.ends_with("text-layer.pdf"))
        .collect();
    assert!(!pdf_chunks.is_empty(), "pdf produced chunks");
}
```

Note: `Chunk`'s fields — this test assumes `file_path: String`-like and `content: String` are public on `Chunk` (they are used by db.rs today). If `file_path` is a `PathBuf`, compare with `.to_string_lossy().ends_with(...)` — mechanical adaptation.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p msrch-core --test extraction_pipeline 2>&1 | tail -5` and `cargo test -p msrch-core extractable_extensions 2>&1 | tail -5`
Expected: chunker test FAILS (html routes to `Unknown` today); integration test FAILS (crawler drops the PDFs via null-byte check).

- [ ] **Step 3: Implement the three hooks**

`crates/core/src/crawler.rs::is_binary` — add before the null-byte check:

```rust
    fn is_binary(&self, path: &Path) -> Result<bool> {
        if !self.config.skip_binary {
            return Ok(false);
        }

        // Extractable document formats (pdf, docx) are binary but wanted —
        // the extraction stage turns them into text before chunking.
        if crate::extract::is_extractable(path) {
            return Ok(false);
        }

        let mut file = File::open(path)?;
        ...
```

`crates/core/src/chunker.rs::determine_file_type` — add arms after the Markdown arm:

```rust
            // Markdown
            "md" | "mdx" | "markdown" => FileType::Markdown,

            // Extracted documents: content arrives pre-extracted as
            // markdown-ish text (html/docx) or plain prose (pdf) — see extract.rs.
            "html" | "htm" | "xhtml" | "docx" => FileType::Markdown,
            "pdf" => FileType::Prose,
```

`crates/core/src/index.rs` — replace the read site (currently `let content = match fs::read_to_string(&file_path) { ... }`):

```rust
            let content = if crate::extract::is_extractable(&file_path) {
                let max_bytes = self.config.chunking.max_file_size_mb * 1024 * 1024;
                match crate::extract::extract(&file_path, max_bytes) {
                    Ok(Some(text)) => text,
                    Ok(None) => {
                        // Reason already warned to stderr by the extractor.
                        pb.inc(1);
                        continue;
                    }
                    Err(e) => {
                        warn!("Failed to extract {:?}: {}", file_path, e);
                        pb.inc(1);
                        continue;
                    }
                }
            } else {
                match fs::read_to_string(&file_path) {
                    Ok(c) => c,
                    Err(_) => {
                        pb.inc(1);
                        continue; // Skip non-utf8 for now
                    }
                }
            };
```

`crates/core/src/index.rs` — bump the schema constant and extend its changelog comment:

```rust
/// v4: lancedb 0.23 -> 0.31 upgrade (lance storage engine 1.x -> 8.x). ...
/// v5: HTML/PDF/docx extraction — .html content semantics changed from raw
/// markup to extracted text, so pre-v5 HTML chunks carry wrong embeddings.
pub const SCHEMA_VERSION: u32 = 5;
```

(Keep the existing v1–v4 comment lines; append the v5 line.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace 2>&1 | grep "test result"`
Expected: all green, including the new chunker routing test and the integration test. Also confirm the version-string test in the cli still passes (it reads `SCHEMA_VERSION`, now 5 — the test is written against the constant, not a literal; if any test hardcoded `v4`, update the literal).

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy 2>&1 | tail -3` — no new warnings.

```bash
git add -A
git commit -m "feat: wire extraction into crawl/chunk/index pipeline; schema v5

.html files were previously indexed as raw markup (FileType::Unknown);
extraction changes their content semantics, so SCHEMA_VERSION 5 forces a
wipe-and-rebuild on the next index/reindex. Run msrch reindex after
upgrading."
```

---

### Task 6: Release 0.3.0 — version, changelog, docs

**Files:**
- Modify: root `Cargo.toml` (`workspace.package.version`)
- Modify: `CHANGELOG.md`, `CLAUDE.md`

**Interfaces:** none (documentation + metadata only).

- [ ] **Step 1: Bump the version**

Root `Cargo.toml`: `version = "0.2.0"` → `version = "0.3.0"`. Run `cargo build -q` so `Cargo.lock` picks up the new package versions.

- [ ] **Step 2: CHANGELOG entry**

Insert at the top of `CHANGELOG.md` (below the intro paragraph, above `## [0.2.0]`):

```markdown
## [0.3.0] - 2026-07-15

### Added
- **Document extraction**: HTML, text-layer PDF, and .docx files are now
  indexed. HTML gets readability-style main-content extraction (with automatic
  whole-page fallback for non-article pages); docx headings map to markdown
  headings for structure-aware chunking; PDFs index their text layer.
  Graphics-only PDFs are skipped with a warning — no OCR.
- `max_file_size_mb` (config, default 10) is now enforced for extractable
  document types.

### Changed
- Index schema bumped to v5: `.html` files were previously indexed as raw
  markup; **run `msrch reindex` after upgrading** to replace tag-soup chunks
  with extracted text.
```

- [ ] **Step 3: CLAUDE.md updates**

In "Key Modules", after the `crawler.rs` bullet, add:

```markdown
- **`extract.rs`** - Text extraction for document formats (HTML via readability + fallback, text-layer PDF, docx); owns all format knowledge
```

In the "File Structure" tree, add `│   ├── extract.rs` in the core src listing (alphabetical position) — if the tree still shows the pre-workspace flat `src/` layout, update the whole tree to the `crates/core` + `crates/cli` reality while you're there (report what you found).

In "Indexing Pipeline" (Core Data Flow), insert between Crawler and Chunker:

```markdown
2. **Extractor** (`extract.rs`) - Converts HTML/PDF/docx to plain text before chunking (markdown-ish for HTML/docx, prose for PDF); skips graphics-only PDFs and oversize files
```

(renumber the following steps).

- [ ] **Step 4: Full suite + commit**

Run: `cargo test --workspace 2>&1 | grep "test result"` — all green.

```bash
git add -A
git commit -m "chore: release 0.3.0 — document extraction (see CHANGELOG)"
```

**Post-merge (controller/human, not this task):** on main after merge, `git tag v0.3.0 && git push --tags`; then `make install` and manually smoke-test against a real mixed-document repo (`msrch index . && msrch "some question" --limit 5`), confirming HTML results show article text rather than markup and `msrch --version` reports 0.3.0 / schema v5.

---

## Self-review notes

- Spec coverage: architecture + API (Task 1), HTML readability/fallback with exact thresholds (Task 2), docx mapping incl. tables/tabs/Title (Task 3), PDF text-layer + graphics-only skip + committed fixtures (Task 4), all three hooks + schema v5 + integration test (Task 5), release/versioning/docs (Task 6). Size gate implemented in Task 1's dispatcher and asserted by `extract_skips_oversize_files`. Failure semantics (warn + skip, never abort) exercised by corrupt-docx and corrupt-pdf tests. ✓
- YAGNI ledger respected: no OCR, no pptx/xlsx, no cache, no new config keys, no context-paths for documents. ✓
- Type consistency: `extract(path, max_bytes) -> Result<Option<String>>` used identically in Tasks 1/5 and the integration test; `html_to_markdown` shared by Tasks 1–2; fixture helper names consistent. ✓
- Known judgment points made explicit for implementers: dom_smoothie/quick-xml/zip API adaptation clause (Global Constraints), cupsfilter availability gate (Task 4 Step 1), Chunk field shape note (Task 5 Step 1).
