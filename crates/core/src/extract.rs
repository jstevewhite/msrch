//! Text extraction for document formats (HTML, text-layer PDF, docx).
//!
//! One module owns all format knowledge. The indexer calls [`extract`] for
//! paths where [`is_extractable`] is true, instead of reading the file as
//! UTF-8. `Ok(None)` means "skip this file; the reason was already warned to
//! stderr" — no text layer, over the size cap, or unparseable.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

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
#[allow(dead_code)] // called by extract_html from Task 2 on; exercised by tests now
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

    // `pending_space` tracks whether the SOURCE had whitespace between the
    // previously-emitted node and whatever comes next — it is threaded
    // through the recursion rather than inferred from the output buffer, so
    // that inline elements don't fabricate spaces before trailing
    // punctuation (e.g. `<b>bold</b>,`) while whitespace-only text nodes
    // between elements (e.g. `<b>a</b> <b>b</b>`) still join with a space.
    fn emit(node: ego_tree::NodeRef<Node>, out: &mut String, pending_space: &mut bool) {
        match node.value() {
            Node::Text(t) => {
                let raw: &str = t;
                let starts_ws = raw.starts_with(|c: char| c.is_whitespace());
                let ends_ws = raw.ends_with(|c: char| c.is_whitespace());
                let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                if cleaned.is_empty() {
                    // Whitespace-only (or empty) text node: it carries no
                    // content of its own, but still marks that source
                    // whitespace separates whatever comes before and after.
                    if !raw.is_empty() {
                        *pending_space = true;
                    }
                    return;
                }
                if (*pending_space || starts_ws)
                    && !out.is_empty()
                    && !out.ends_with(|c: char| c.is_whitespace())
                {
                    out.push(' ');
                }
                out.push_str(&cleaned);
                *pending_space = ends_ws;
            }
            Node::Element(el) => {
                let tag = el.name();
                if matches!(tag, "script" | "style" | "noscript" | "template" | "head") {
                    return;
                }
                if let Some(level) = heading_level(tag) {
                    ensure_block_break(out);
                    *pending_space = false;
                    out.push_str(&"#".repeat(level));
                    out.push(' ');
                } else if is_block(tag) {
                    ensure_block_break(out);
                    *pending_space = false;
                    if tag == "li" {
                        out.push_str("- ");
                    }
                }
                for child in node.children() {
                    emit(child, out, pending_space);
                }
                if tag == "br" {
                    out.push('\n');
                    *pending_space = false;
                } else if matches!(tag, "td" | "th") {
                    out.push_str(" | ");
                    *pending_space = false;
                } else if heading_level(tag).is_some() || is_block(tag) {
                    ensure_block_break(out);
                    *pending_space = false;
                }
            }
            _ => {
                for child in node.children() {
                    emit(child, out, pending_space);
                }
            }
        }
    }

    let doc = Html::parse_document(html);
    let mut out = String::new();
    let mut pending_space = false;
    emit(doc.tree.root(), &mut out, &mut pending_space);

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

    #[test]
    fn html_to_markdown_preserves_source_spacing_around_inline_elements() {
        let md = html_to_markdown(
            "<p>This is <b>bold</b>, right? Hello <em>world</em>! See <a href='x'>the link</a>.</p>\
             <p>word<b>glued</b> and <b>a</b> <b>b</b></p>",
        );
        assert!(md.contains("This is bold, right?"), "no space before comma: {md}");
        assert!(md.contains("Hello world!"), "no space before bang: {md}");
        assert!(md.contains("See the link."), "no space before period: {md}");
        assert!(md.contains("wordglued"), "source had no space — none fabricated: {md}");
        assert!(md.contains("glued and a b"), "whitespace-only text nodes still separate: {md}");
    }
}
