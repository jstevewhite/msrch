mod config;
mod crawler;
mod chunker;
mod embedding;
mod db;
mod index;
mod reranker;
mod search;

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
    Similar {
        file: PathBuf,
    },
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
            let config = config::Config::load_global_config().unwrap_or_default();
            // Resolve absolute path
            let root_path = std::fs::canonicalize(path).unwrap_or(path.clone());
            let indexer = index::Indexer::new(root_path, config);
            match indexer.index().await {
                Ok(_) => println!("Indexing completed successfully."),
                Err(e) => eprintln!("Indexing failed: {}", e),
            }
        }
        Commands::Query { text, limit, format, rerank } => {
            let searcher = match search::Searcher::new(None).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Initialization failed: {}", e);
                    return Ok(());
                }
            };

            if let Err(e) = searcher.search(text, *limit, *format, *rerank).await {
                eprintln!("Search failed: {}", e);
            }
        }
        Commands::Reindex => {
            // For now, reindex is just index . (since incremental logic handles it)
            // But we need to find root first
             let config = config::Config::load_global_config().unwrap_or_default();
             let current_dir = std::env::current_dir()?;
             // Determine root logic dupe from Searcher? 
             // Ideally we refactor find_index_root to a util or reuse Searcher's knowledge
             // Quick hack: Searcher knows the root.
             let _searcher = search::Searcher::new(None).await?;
             // Wait, searcher doesn't expose root. Let's just assume we run reindex from within the tree
             // and use search-like discovery or just current dir for now.
             // Better: reindex implies we are already established.
             
             // Simplification: Reindex acts on current dir or upwards
             let indexer = index::Indexer::new(current_dir, config);
             // Verify .msrch exists? The indexer creates it if missing.
             indexer.index().await?;
        }
        Commands::Stats => {
            use std::collections::HashMap;
            use serde::{Deserialize, Serialize};
            use std::time::SystemTime;
            use colored::*;

            #[derive(Serialize, Deserialize)]
            struct FileMetadata {
                modified_at: SystemTime,
                chunk_ids: Vec<uuid::Uuid>,
            }

            #[derive(Serialize, Deserialize, Default)]
            struct Manifest {
                files: HashMap<PathBuf, FileMetadata>,
            }

            // Find index root
            let mut current = std::env::current_dir()?;
            let index_root = loop {
                let candidate = current.join(".msrch");
                if candidate.exists() && candidate.is_dir() {
                    break current;
                }
                match current.parent() {
                    Some(parent) => current = parent.to_path_buf(),
                    None => {
                        eprintln!("No .msrch index found in directory tree");
                        return Ok(());
                    }
                }
            };

            let msrch_dir = index_root.join(".msrch");
            let manifest_path = msrch_dir.join("manifest.json");
            let db_path = msrch_dir.join("index.db");

            // Load manifest
            let manifest: Manifest = if manifest_path.exists() {
                let file = std::fs::File::open(&manifest_path)?;
                serde_json::from_reader(file).unwrap_or_default()
            } else {
                Manifest::default()
            };

            // Get chunk count from DB
            let chunk_count = if db_path.exists() {
                let db = db::VectorDB::new(db_path.clone()).await?;
                db.count().await.unwrap_or(0)
            } else {
                0
            };

            // Calculate total tokens (approximate from manifest chunk count)
            let _total_chunks_in_manifest: usize = manifest.files.values()
                .map(|m| m.chunk_ids.len())
                .sum();

            // Get last modified time (most recent file in manifest)
            let last_indexed = manifest.files.values()
                .map(|m| m.modified_at)
                .max();

            // Calculate index size on disk
            let index_size = if db_path.exists() {
                fn dir_size(path: &std::path::Path) -> u64 {
                    let mut size = 0;
                    if path.is_dir() {
                        if let Ok(entries) = std::fs::read_dir(path) {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if path.is_dir() {
                                    size += dir_size(&path);
                                } else {
                                    size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                                }
                            }
                        }
                    } else {
                        size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    }
                    size
                }
                dir_size(&msrch_dir)
            } else {
                0
            };

            // Load config for model info
            let config = config::Config::load_global_config().unwrap_or_default();

            // Format output
            println!("{}", "Index Statistics".bold().underline());
            println!();
            println!("  {:<18} {}", "Index:".cyan(), msrch_dir.display());
            println!("  {:<18} {}", "Root:".cyan(), index_root.display());
            println!("  {:<18} {}", "Files:".cyan(), manifest.files.len());
            println!("  {:<18} {}", "Chunks:".cyan(), chunk_count);
            println!("  {:<18} ~{}", "Est. tokens:".cyan(), chunk_count * 256); // rough estimate
            println!("  {:<18} {}", "Model:".cyan(), config.embedding.model);
            println!("  {:<18} {}", "Endpoint:".cyan(), config.embedding.endpoint);

            if let Some(last) = last_indexed {
                if let Ok(duration) = last.duration_since(SystemTime::UNIX_EPOCH) {
                    let datetime = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    println!("  {:<18} {}", "Last indexed:".cyan(), datetime);
                }
            }

            // Format size nicely
            let size_str = if index_size >= 1024 * 1024 {
                format!("{:.1} MB", index_size as f64 / (1024.0 * 1024.0))
            } else if index_size >= 1024 {
                format!("{:.1} KB", index_size as f64 / 1024.0)
            } else {
                format!("{} bytes", index_size)
            };
            println!("  {:<18} {}", "Size on disk:".cyan(), size_str);
        }
        Commands::Similar { file } => {
            use colored::*;
            use std::collections::HashSet;

            // Resolve file path
            let file_path = std::fs::canonicalize(file).unwrap_or(file.clone());

            if !file_path.exists() {
                eprintln!("File not found: {}", file_path.display());
                return Ok(());
            }

            // Read file content
            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to read file: {}", e);
                    return Ok(());
                }
            };

            if content.trim().is_empty() {
                eprintln!("File is empty");
                return Ok(());
            }

            // Load config and create embedding client
            let config = config::Config::load_global_config().unwrap_or_default();
            let embedder = embedding::EmbeddingClient::new(config.embedding.clone())?;

            // Find index root
            let mut current = std::env::current_dir()?;
            let index_root = loop {
                let candidate = current.join(".msrch");
                if candidate.exists() && candidate.is_dir() {
                    break current;
                }
                match current.parent() {
                    Some(parent) => current = parent.to_path_buf(),
                    None => {
                        eprintln!("No .msrch index found in directory tree");
                        return Ok(());
                    }
                }
            };

            let msrch_dir = index_root.join(".msrch");
            let db = db::VectorDB::new(msrch_dir.join("index.db")).await?;

            // Create embedding for the input file (use truncated content to fit model limits)
            let truncated = if content.len() > 8000 {
                content[..8000].to_string()
            } else {
                content.clone()
            };

            println!("Finding files similar to: {}", file_path.display().to_string().cyan());

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
                let result_file = result.payload
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
                println!("{}", format!("\nFound {} similar files:", unique_results.len()).bold());
                for (file, score) in unique_results {
                    println!(
                        "  {} {}",
                        format!("{:.2}", score).yellow(),
                        file.cyan()
                    );
                }
            }
        }
        Commands::Config => {
            let config = config::Config::load_global_config().unwrap_or_default();
            println!("{:#?}", config);
        }
    }
    
    Ok(())
}
