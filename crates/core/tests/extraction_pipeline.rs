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
