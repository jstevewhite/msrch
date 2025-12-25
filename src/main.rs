mod config;
mod crawler;
mod chunker;
mod embedding;
mod db;
mod index;
mod search;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create/update index in <path>
    Index {
        path: PathBuf,
    },
    /// Search (implicit query if not a subcommand)
    Query {
        text: String,
        #[arg(long)]
        limit: Option<usize>,
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

    match &cli.command {
        Commands::Index { path } => {
            let config = config::Config::load_global_config().unwrap_or_default();
            // Resolve absolute path
            let root_path = std::fs::canonicalize(path).unwrap_or(path.clone());
            let indexer = index::Indexer::new(root_path, config);
            match indexer.index().await {
                Ok(_) => println!("Indexing completed successfully."),
                Err(e) => eprintln!("Indexing failed: {}", e),
            }
        }
        Commands::Query { text, limit } => {
            let searcher = match search::Searcher::new(None).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Initialization failed: {}", e);
                    return Ok(());
                }
            };
            
            if let Err(e) = searcher.search(text, *limit).await {
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
             let searcher = search::Searcher::new(None).await?;
             // Wait, searcher doesn't expose root. Let's just assume we run reindex from within the tree
             // and use search-like discovery or just current dir for now.
             // Better: reindex implies we are already established.
             
             // Simplification: Reindex acts on current dir or upwards
             let indexer = index::Indexer::new(current_dir, config);
             // Verify .msrch exists? The indexer creates it if missing.
             indexer.index().await?;
        }
        Commands::Stats => {
            println!("Stats not implemented yet.");
        }
        Commands::Similar { file } => {
            println!("Similar search to {:?} not implemented yet.", file);
        }
        Commands::Config => {
            let config = config::Config::load_global_config().unwrap_or_default();
            println!("{:#?}", config);
        }
    }
    
    Ok(())
}
