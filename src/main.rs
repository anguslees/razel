use clap::{Parser, Subcommand};
use fastrace::collector::ConsoleReporter;
use tokio::io::AsyncWriteExt;

mod query;
mod starlark;
mod bazel;
mod workspace;

use crate::bazel::Configuration;
use std::sync::Arc;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
#[clap(propagate_version = true)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Prints version information
    Version,
    /// Builds the specified targets
    Build {
        #[clap(value_parser)]
        targets: Vec<String>,
    },
    /// Tests the specified targets
    Test {
        #[clap(value_parser)]
        targets: Vec<String>,
    },
    /// Runs the specified target
    Run {
        #[clap(value_parser)]
        target: String,
    },
    /// Queries for information about the build graph
    Query {
        #[clap(value_parser)]
        query: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut stdout = tokio::io::stdout();

    fastrace::set_reporter(ConsoleReporter, fastrace::collector::Config::default());

    let cli = Cli::parse();

    // TODO: initialise config from flags
    let config = Arc::new(Configuration::new());

    match &cli.command {
        Commands::Version => {
            // The version is automatically handled by clap if --version is passed.
            // This explicit subcommand can be used if `razel version` is preferred.
            println!("Razel version: {}", env!("CARGO_PKG_VERSION"));
        }
        Commands::Build { targets } => {
            println!("Building targets: {targets:?}");
            unimplemented!("Build command is not yet implemented.");
        }
        Commands::Test { targets } => {
            println!("Testing targets: {targets:?}");
            unimplemented!("Test command is not yet implemented.");
        }
        Commands::Run { target } => {
            println!("Running target: {target}");
            unimplemented!("Run command is not yet implemented.");
        }
        Commands::Query { query: query_str } => {
            println!("Querying: {query_str}");
            query::query(&mut stdout, config, query_str).await?;
        }
    }

    fastrace::flush();
    stdout.flush().await?;
    Ok(())
}
