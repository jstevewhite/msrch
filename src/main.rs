mod chunker;
mod config;
mod crawler;
mod db;
mod embedding;
mod index;
mod reranker;
mod search;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum OutputFormat {
    /// File paths only
    Plain,
    /// File paths with code snippets (default)
    #[default]
    Context,
    /// JSON output for scripting
    Json,
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Implicit search query (shorthand for `msrch query "text"`)
    #[arg(trailing_var_arg = true)]
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
}

#[derive(Subcommand)]
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
            let config = config::Config::load_global_config_or_default();
            // Resolve absolute path
            let root_path = std::fs::canonicalize(path).unwrap_or(path.clone());
            let indexer = index::Indexer::new(root_path, config);
            indexer.index().await.context("Indexing failed")?;
            println!("Indexing completed successfully.");
        }
        Commands::Query {
            text,
            limit,
            format,
            rerank,
        } => {
            let searcher = search::Searcher::new(None)
                .await
                .context("Initialization failed")?;
            searcher
                .search(text, *limit, *format, *rerank)
                .await
                .context("Search failed")?;
        }
        Commands::Reindex => {
            let current_dir = std::env::current_dir()?;
            let root_path = index::find_index_root(&current_dir)
                .context("No .msrch index found in directory tree")?;
            let msrch_dir = root_path.join(".msrch");
            if msrch_dir.exists() {
                std::fs::remove_dir_all(&msrch_dir).context("Failed to remove old index")?;
            }
            let config = config::Config::load_global_config_or_default();
            let indexer = index::Indexer::new(root_path, config);
            indexer.index().await.context("Reindexing failed")?;
            println!("Reindexing completed successfully.");
        }
        Commands::Stats => {
            let current_dir = std::env::current_dir()?;
            index::get_stats(&current_dir).await?.print();
        }
        Commands::Similar { file } => {
            use colored::*;
            use std::collections::HashSet;

            // Resolve file path
            let file_path = std::fs::canonicalize(file).unwrap_or(file.clone());

            if !file_path.exists() {
                anyhow::bail!("File not found: {}", file_path.display());
            }

            // Read file content
            let content = std::fs::read_to_string(&file_path).context("Failed to read file")?;

            if content.trim().is_empty() {
                anyhow::bail!("File is empty");
            }

            // Load config and create embedding client
            let config = config::Config::load_global_config_or_default();
            let embedder = embedding::EmbeddingClient::new(config.embedding.clone())?;

            let current_dir = std::env::current_dir()?;
            let index_root = index::find_index_root(&current_dir)
                .context("No .msrch index found in directory tree")?;

            let msrch_dir = index_root.join(".msrch");
            let db = db::VectorDB::new(msrch_dir.join("index.db")).await?;

            // Create embedding for the input file (use truncated content to fit model limits)
            let truncated = if content.len() > 8000 {
                content[..8000].to_string()
            } else {
                content.clone()
            };

            println!(
                "Finding files similar to: {}",
                file_path.display().to_string().cyan()
            );

            let embeddings = match embedder.embed(vec![truncated]).await {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Failed to embed file: {}", e);
                    return Ok(());
                }
            };

            let query_vector = embeddings.into_iter().next().unwrap();

            // Search for similar chunks
            let results = db.search(query_vector, 20, 0.0).await?;

            // Deduplicate by file path (show each file only once, with best score)
            let mut seen_files: HashSet<String> = HashSet::new();
            let mut unique_results = Vec::new();

            // Exclude the query file itself
            let query_file_str = file_path.display().to_string();

            for result in results {
                let result_file = result
                    .payload
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Skip the query file
                if result_file == query_file_str {
                    continue;
                }

                if !seen_files.contains(&result_file) {
                    seen_files.insert(result_file.clone());
                    unique_results.push((result_file, result.score));
                }

                if unique_results.len() >= 10 {
                    break;
                }
            }

            if unique_results.is_empty() {
                println!("No similar files found.");
            } else {
                println!(
                    "{}",
                    format!("\nFound {} similar files:", unique_results.len()).bold()
                );
                for (file, score) in unique_results {
                    println!("  {} {}", format!("{:.2}", score).yellow(), file.cyan());
                }
            }
        }
        Commands::Config => {
            let config = config::Config::load_global_config_or_default();
            println!("{:#?}", config);
        }
    }

    Ok(())
}
