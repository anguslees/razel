use clap::{Parser, Subcommand};
use fastrace::collector::ConsoleReporter;

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
async fn main() {
    fastrace::set_reporter(ConsoleReporter, fastrace::collector::Config::default());

    let cli = Cli::parse();

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
        Commands::Query { query } => {
            println!("Querying: {query}");
            unimplemented!("Query command is not yet implemented.");
        }
    }

    fastrace::flush();
}
