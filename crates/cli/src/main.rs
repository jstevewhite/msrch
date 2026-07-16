mod dates;
mod output;

use anyhow::Context;
use clap::{Parser, Subcommand};
use msrch_core::{config, index, search};
use output::OutputFormat;
use std::path::PathBuf;

/// Version line for `msrch --version`: semver, index schema, and build commit.
/// e.g. `msrch 0.4.0 (index schema v5, commit a1b2c3d)`
fn version_string() -> String {
    format!(
        "{} (index schema v{}, commit {})",
        env!("CARGO_PKG_VERSION"),
        msrch_core::index::SCHEMA_VERSION,
        env!("MSRCH_GIT_HASH"),
    )
}

#[derive(Parser, Debug)]
#[command(author, version = version_string(), about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Implicit search query (shorthand for `msrch query "text"`)
    ///
    /// Collected as positional words so unquoted multi-word queries work; global
    /// flags like `--format`/`--limit` are still parsed even when placed after the
    /// query text (do not re-add `trailing_var_arg`, which swallows them).
    query_args: Vec<String>,

    /// Max results (for implicit query)
    #[arg(long, global = true)]
    limit: Option<usize>,

    /// Output format (for implicit query)
    #[arg(long, short, value_enum, global = true)]
    format: Option<OutputFormat>,

    /// Use reranker for more precise results (slower)
    #[arg(long, global = true)]
    rerank: bool,

    /// Only match files whose path contains this substring
    #[arg(long, global = true)]
    path: Option<String>,

    /// Only match files modified on/after this date (YYYY-MM-DD, or 7d/2w/3m ago)
    #[arg(long, global = true, value_parser = dates::parse_date_arg)]
    after: Option<std::time::SystemTime>,

    /// Only match files modified before this date (YYYY-MM-DD, or 7d/2w/3m ago)
    #[arg(long, global = true, value_parser = dates::parse_date_arg)]
    before: Option<std::time::SystemTime>,

    /// Skip the automatic index refresh even if query.auto_index is enabled
    #[arg(long, global = true)]
    no_auto_index: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create/update index in <path>
    Index {
        path: PathBuf,
        #[arg(long)]
        debug: bool,
    },
    /// Search (implicit query if not a subcommand)
    Query {
        text: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, short, value_enum, default_value_t = OutputFormat::Context)]
        format: OutputFormat,
        /// Use reranker for more precise results (slower)
        #[arg(long)]
        rerank: bool,
        /// Only match files whose path contains this substring
        #[arg(long)]
        path: Option<String>,
        /// Only match files modified on/after this date (YYYY-MM-DD, or 7d/2w/3m ago)
        #[arg(long, value_parser = dates::parse_date_arg)]
        after: Option<std::time::SystemTime>,
        /// Only match files modified before this date (YYYY-MM-DD, or 7d/2w/3m ago)
        #[arg(long, value_parser = dates::parse_date_arg)]
        before: Option<std::time::SystemTime>,
    },
    /// Force full rebuild
    Reindex,
    /// Show index statistics
    Stats,
    /// Find semantically similar files
    Similar { file: PathBuf },
    /// Show effective configuration
    Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Bind no_auto_index before cli.command is moved (consumed by auto-index wiring)
    let no_auto_index = cli.no_auto_index;

    // Handle implicit query (msrch "search text" without subcommand)
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            if cli.query_args.is_empty() {
                // No args at all - show help
                Cli::parse_from(["msrch", "--help"]);
                return Ok(());
            }
            // Join all args as query text
            Commands::Query {
                text: cli.query_args.join(" "),
                limit: cli.limit,
                format: cli.format.unwrap_or_default(),
                rerank: cli.rerank,
                path: cli.path.clone(),
                after: cli.after,
                before: cli.before,
            }
        }
    };

    match &command {
        Commands::Index { path, debug } => {
            if *debug {
                env_logger::Builder::from_default_env()
                    .filter_level(log::LevelFilter::Debug)
                    .init();
            }
            // Resolve absolute path
            let root_path = std::fs::canonicalize(path).unwrap_or(path.clone());
            let config = config::Config::load_for_index(&root_path);
            let indexer = index::Indexer::new(root_path, config);
            indexer.index().await.context("Indexing failed")?;
            println!("Indexing completed successfully.");
        }
        Commands::Query {
            text,
            limit,
            format,
            rerank,
            path,
            after,
            before,
        } => {
            let current_dir = std::env::current_dir()?;
            let index_root = index::find_index_root(&current_dir)
                .context("No .msrch index found in directory tree")?;
            let config = config::Config::load_for_index(&index_root);

            if config.query.auto_index && !no_auto_index {
                let indexer = index::Indexer::new(index_root.clone(), config.clone());
                match indexer.index_quiet().await {
                    Ok(0) => {}
                    Ok(n) => eprintln!("auto-index: refreshed {n} file(s)"),
                    Err(e) => eprintln!(
                        "warning: auto-index failed ({e}); searching the existing index"
                    ),
                }
            }

            let searcher = search::Searcher::new(Some(index_root))
                .await
                .context("Initialization failed")?;
            let opts = search::SearchOptions {
                limit: *limit,
                use_rerank: *rerank,
                path_contains: path.clone(),
                after: *after,
                before: *before,
            };
            let results = searcher
                .search(text, &opts)
                .await
                .context("Search failed")?;
            output::render(*format, text, &searcher.msrch_dir(), &results);
        }
        Commands::Reindex => {
            let current_dir = std::env::current_dir()?;
            let root_path = index::find_index_root(&current_dir)
                .context("No .msrch index found in directory tree")?;
            // Load the effective config BEFORE touching .msrch; artifact removal
            // preserves a project config.toml (see index::remove_index_artifacts).
            let config = config::Config::load_for_index(&root_path);
            index::remove_index_artifacts(&root_path)?;
            let indexer = index::Indexer::new(root_path, config);
            indexer.index().await.context("Reindexing failed")?;
            println!("Reindexing completed successfully.");
        }
        Commands::Stats => {
            let current_dir = std::env::current_dir()?;
            let stats = index::get_stats(&current_dir).await?;
            output::print_stats(&stats);
        }
        Commands::Similar { file } => {
            use colored::*;

            let file_path = std::fs::canonicalize(file).unwrap_or(file.clone());
            if !file_path.exists() {
                anyhow::bail!("File not found: {}", file_path.display());
            }

            let searcher = search::Searcher::new(None).await?;

            println!(
                "Finding files similar to: {}",
                file_path.display().to_string().cyan()
            );

            let results = searcher.find_similar(&file_path, 10).await?;
            output::print_similar(&results);
        }
        Commands::Config => {
            let current_dir = std::env::current_dir()?;
            match index::find_index_root(&current_dir) {
                Some(root) => {
                    println!("# effective config for index at {}", root.display());
                    println!("{:#?}", config::Config::load_for_index(&root));
                }
                None => {
                    println!("# global config (no .msrch index found)");
                    println!("{:#?}", config::Config::load_global_config_or_default());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn implicit_query_honors_trailing_format_flag() {
        let cli = Cli::try_parse_from(["msrch", "find the chunker", "--format", "filename"])
            .expect("should parse");
        assert!(cli.command.is_none(), "no subcommand => implicit query");
        assert_eq!(cli.query_args, vec!["find the chunker".to_string()]);
        assert_eq!(cli.format, Some(OutputFormat::Filename));
    }

    #[test]
    fn implicit_query_honors_trailing_limit_flag() {
        let cli = Cli::try_parse_from(["msrch", "find the chunker", "--limit", "3"])
            .expect("should parse");
        assert_eq!(cli.limit, Some(3));
        assert_eq!(cli.query_args, vec!["find the chunker".to_string()]);
    }

    #[test]
    fn implicit_query_collects_unquoted_words() {
        // Multi-word queries without quotes still collect as positional words.
        let cli = Cli::try_parse_from(["msrch", "find", "the", "chunker"]).expect("should parse");
        assert_eq!(cli.query_args, vec!["find", "the", "chunker"]);
        assert!(cli.format.is_none());
    }

    #[test]
    fn version_string_carries_semver_schema_and_commit() {
        let v = version_string();
        assert!(v.starts_with(env!("CARGO_PKG_VERSION")));
        assert!(v.contains(&format!(
            "index schema v{}",
            msrch_core::index::SCHEMA_VERSION
        )));
        // Build hash is best-effort but never empty: real hash or "unknown".
        assert!(!v.contains("commit )"), "commit hash must not be empty: {v}");
        assert!(v.contains("commit "));
    }

    #[test]
    fn implicit_query_honors_filter_flags() {
        let cli = Cli::try_parse_from([
            "msrch", "budget concerns", "--path", "2026/07", "--after", "2026-07-01",
        ])
        .expect("should parse");
        assert_eq!(cli.path, Some("2026/07".to_string()));
        assert!(cli.after.is_some());
        assert!(cli.before.is_none());
        assert_eq!(cli.query_args, vec!["budget concerns".to_string()]);
    }

    #[test]
    fn bad_date_is_a_parse_error_listing_forms() {
        let err = Cli::try_parse_from(["msrch", "q", "--after", "tomorrow"]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("YYYY-MM-DD"), "clap error lists accepted forms: {msg}");
    }

    #[test]
    fn no_auto_index_flag_parses() {
        let cli = Cli::try_parse_from(["msrch", "q", "--no-auto-index"]).expect("should parse");
        assert!(cli.no_auto_index);
    }
}
