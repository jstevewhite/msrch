use clap::ValueEnum;
use colored::*;
use msrch_core::index::IndexStats;
use msrch_core::search::{ScoreKind, SearchOutcome, SearchResult, SimilarFile};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputFormat {
    /// File paths only
    Plain,
    /// File paths with code snippets (default)
    #[default]
    Context,
    /// JSON output for scripting
    Json,
    /// Deduplicated file paths only (like `grep -l`)
    Filename,
}

#[derive(Serialize)]
struct JsonOutput {
    query: String,
    index_path: String,
    score_kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    results: Vec<JsonResult>,
}

#[derive(Serialize)]
struct JsonResult {
    file_path: String,
    chunk_index: u64,
    similarity: f32,
    context: String,
    content: String,
}

/// Header line for the context format; reranked sets are labeled so the
/// score scale is self-explanatory.
fn context_header(n: usize, kind: ScoreKind) -> String {
    match kind {
        ScoreKind::Reranker => format!("Found {n} results (reranked):"),
        ScoreKind::Vector => format!("Found {n} results:"),
    }
}

/// Render search results in the requested format. Handles the empty case.
pub fn render(format: OutputFormat, query: &str, msrch_dir: &Path, outcome: &SearchOutcome) {
    let results = &outcome.results;
    if results.is_empty() {
        match format {
            OutputFormat::Json => {
                let mut empty = serde_json::json!({
                    "query": query,
                    "index_path": msrch_dir.display().to_string(),
                    "score_kind": outcome.score_kind.as_str(),
                    "results": []
                });
                if !outcome.warnings.is_empty() {
                    empty["warnings"] = serde_json::json!(outcome.warnings);
                }
                println!("{}", empty);
            }
            _ => println!("No results found."),
        }
        return;
    }

    match format {
        OutputFormat::Plain => display_plain(results),
        OutputFormat::Context => display_context(results, outcome.score_kind),
        OutputFormat::Json => display_json(query, msrch_dir, outcome),
        OutputFormat::Filename => display_filename(results),
    }
}

fn display_plain(results: &[SearchResult]) {
    for result in results {
        println!("{}:{}", result.file_path, result.chunk_index);
    }
}

fn display_context(results: &[SearchResult], kind: ScoreKind) {
    println!("{}", context_header(results.len(), kind).bold());
    for result in results {
        let context_suffix = if result.context.is_empty() {
            String::new()
        } else {
            format!("  {}", result.context.dimmed())
        };

        println!(
            "\n{} {}:{}{}",
            format!("{:.2}", result.score).yellow(),
            result.file_path.cyan(),
            result.chunk_index,
            context_suffix
        );

        for line in result.content.lines().take(3) {
            println!("  │ {}", line);
        }
    }
}

fn display_json(query: &str, msrch_dir: &Path, outcome: &SearchOutcome) {
    let json_results: Vec<JsonResult> = outcome
        .results
        .iter()
        .map(|r| JsonResult {
            file_path: r.file_path.clone(),
            chunk_index: r.chunk_index,
            similarity: r.score,
            context: r.context.clone(),
            content: r.content.clone(),
        })
        .collect();

    let output = JsonOutput {
        query: query.to_string(),
        index_path: msrch_dir.display().to_string(),
        score_kind: outcome.score_kind.as_str(),
        warnings: outcome.warnings.clone(),
        results: json_results,
    };

    match serde_json::to_string_pretty(&output) {
        Ok(text) => println!("{}", text),
        Err(e) => eprintln!("Failed to serialize results: {}", e),
    }
}

fn display_filename(results: &[SearchResult]) {
    for file_path in unique_file_paths(results) {
        println!("{}", file_path);
    }
}

/// Collect the distinct `file_path` values from results, preserving the order
/// in which each path is first seen (so the most relevant file leads).
fn unique_file_paths(results: &[SearchResult]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for result in results {
        if seen.insert(result.file_path.clone()) {
            paths.push(result.file_path.clone());
        }
    }
    paths
}

/// Pretty-print index statistics (moved from `IndexStats::print`).
pub fn print_stats(stats: &IndexStats) {
    println!("{}", "Index Statistics".bold().underline());
    println!();
    println!("  {:<18} {}", "Index:".cyan(), stats.index_path.display());
    println!("  {:<18} {}", "Root:".cyan(), stats.root_path.display());
    println!("  {:<18} {}", "Files:".cyan(), stats.file_count);
    println!("  {:<18} {}", "Chunks:".cyan(), stats.chunk_count);
    println!("  {:<18} ~{}", "Est. tokens:".cyan(), stats.estimated_tokens);
    println!("  {:<18} {}", "Model:".cyan(), stats.model);
    println!("  {:<18} {}", "Endpoint:".cyan(), stats.endpoint);

    if let Some(last) = stats.last_indexed {
        if let Ok(duration) = last.duration_since(std::time::SystemTime::UNIX_EPOCH) {
            let datetime = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            println!("  {:<18} {}", "Last indexed:".cyan(), datetime);
        }
    }

    let size_str = if stats.size_on_disk >= 1024 * 1024 {
        format!("{:.1} MB", stats.size_on_disk as f64 / (1024.0 * 1024.0))
    } else if stats.size_on_disk >= 1024 {
        format!("{:.1} KB", stats.size_on_disk as f64 / 1024.0)
    } else {
        format!("{} bytes", stats.size_on_disk)
    };
    println!("  {:<18} {}", "Size on disk:".cyan(), size_str);
}

/// Print `msrch similar` results (moved from main.rs).
pub fn print_similar(results: &[SimilarFile]) {
    if results.is_empty() {
        println!("No similar files found.");
    } else {
        println!(
            "{}",
            format!("\nFound {} similar files:", results.len()).bold()
        );
        for similar in results {
            println!(
                "  {} {}",
                format!("{:.2}", similar.score).yellow(),
                similar.file_path.cyan()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(file_path: &str) -> SearchResult {
        SearchResult {
            file_path: file_path.to_string(),
            chunk_index: 0,
            score: 1.0,
            context: String::new(),
            content: String::new(),
        }
    }

    #[test]
    fn unique_file_paths_dedupes_preserving_first_seen_order() {
        let results = vec![
            result("src/a.rs"),
            result("src/b.rs"),
            result("src/a.rs"),
            result("src/c.rs"),
            result("src/b.rs"),
        ];
        assert_eq!(
            unique_file_paths(&results),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
    }

    #[test]
    fn context_header_marks_reranked_sets() {
        use msrch_core::search::ScoreKind;
        assert_eq!(context_header(5, ScoreKind::Vector), "Found 5 results:");
        assert_eq!(
            context_header(5, ScoreKind::Reranker),
            "Found 5 results (reranked):"
        );
    }
}
