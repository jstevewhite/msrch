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

/// Minimum main-content size (chars) below which readability output is
/// considered degenerate and the whole-page text is used instead.
const HTML_MIN_MAIN_CHARS: usize = 200;
/// Main content must be at least this fraction of the whole-page text (5%).
const HTML_MIN_MAIN_FRACTION_DENOM: usize = 20;

fn extract_html(path: &Path) -> Result<Option<String>> {
    let raw = fs::read_to_string(path).context("read html file")?;
    let whole_page = html_to_markdown(&raw);

    let text = match readability_markdown(&raw) {
        Some((title, body)) if readability_wins(&body, &whole_page) => {
            // dom_smoothie strips an article h1 that duplicates the page title;
            // the title is prime search content, so re-inject it as a heading.
            if !title.is_empty() && !body.trim_start().starts_with('#') {
                format!("# {title}\n\n{body}")
            } else {
                body
            }
        }
        _ => whole_page,
    };

    if text.trim().is_empty() {
        eprintln!("warning: skipping {}: no extractable text", path.display());
        return Ok(None);
    }
    Ok(Some(text))
}

/// True when readability's extracted body is substantial enough to use
/// instead of the whole-page fallback. Measures the BODY ONLY — the injected
/// title is synthesized metadata and must not count toward the gate.
fn readability_wins(body: &str, whole_page: &str) -> bool {
    body.trim().len() >= HTML_MIN_MAIN_CHARS
        && body.trim().len() * HTML_MIN_MAIN_FRACTION_DENOM >= whole_page.trim().len()
}

/// Readability-style main-content extraction; None when parsing fails or the
/// crate finds no article (callers fall back to whole-page text). Returns the
/// article title and body markdown separately so the degenerate gate can
/// measure the body alone.
fn readability_markdown(raw: &str) -> Option<(String, String)> {
    let mut readability = dom_smoothie::Readability::new(raw, None, None).ok()?;
    let article = readability.parse().ok()?;
    let title = article.title.trim().to_string();
    let body = html_to_markdown(article.content.as_ref());
    Some((title, body))
}

fn extract_pdf(_path: &Path) -> Result<Option<String>> {
    anyhow::bail!("extract_pdf: implemented in Task 4")
}
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
                // quick-xml 0.41 dropped `BytesText::unescape()`; `decode()`
                // only resolves byte encoding, not XML entities like
                // `&amp;`, so entity-unescaping is a separate explicit step
                // via the free function `quick_xml::escape::unescape`.
                if let Ok(decoded) = t.decode() {
                    match quick_xml::escape::unescape(&decoded) {
                        Ok(s) => para.push_str(&s),
                        Err(_) => para.push_str(&decoded),
                    }
                }
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
        // dom_smoothie strips an h1 that duplicates the page title, so
        // readability_markdown re-injects the title as a `#` heading.
        assert!(text.contains("# Quarterly Report"), "article heading kept: {text}");
        assert!(text.contains("## Highlights"), "article subheading kept: {text}");
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

    #[test]
    fn degenerate_gate_measures_body_not_injected_title() {
        let whole_page = "x".repeat(4000);
        let thin_body = "Only forty characters of real content."; // < 200 chars
        assert!(
            !readability_wins(thin_body, &whole_page),
            "thin body must lose regardless of how long the page title is"
        );
        let real_body = "y".repeat(300);
        assert!(readability_wins(&real_body, &whole_page), "substantial body wins");
        let tiny_page_thin_body = "z".repeat(100);
        assert!(
            !readability_wins(thin_body, &tiny_page_thin_body),
            "sub-200-char body loses even when over 5% of the page"
        );
    }
}
