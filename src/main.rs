use crate::bazel::InvocationOptions;
use crate::query::QueryOutput;
use clap::{Parser, Subcommand};
use fastrace::collector::ConsoleReporter;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod bazel;
mod query;
mod shared_error;
mod starlark;
pub mod stream_tee;
mod workspace;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(rename_all = "snake_case")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Whether to ignore dev dependencies
    #[arg(
        long,
        global = true,
        require_equals = true,
        default_missing_value = "true",
        num_args(0..=1),
        value_name = "BOOL"
    )]
    pub ignore_dev_dependency: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Prints version information
    Version,
    /// Builds the specified targets
    Build { targets: Vec<String> },
    /// Tests the specified targets
    Test { targets: Vec<String> },
    /// Runs the specified target
    Run { target: String },
    /// Queries for information about the build graph
    Query {
        #[arg(long, value_enum, default_value_t = QueryOutput::Label)]
        output: QueryOutput,
        query: String,
    },
}

#[test]
fn verify_cli() {
    use clap::CommandFactory;
    Cli::command().debug_assert();

    let cli = Cli::try_parse_from(["razel", "query", "//:all"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Query {
            output: QueryOutput::Label,
            ..
        }
    ));

    let cli = Cli::try_parse_from(["razel", "query", "--output=label_kind", "//:all"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Query {
            output: QueryOutput::LabelKind,
            ..
        }
    ));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut stdout = tokio::io::stdout();

    let cli = Cli::parse();

    let options = Arc::new(InvocationOptions::from_flags(&cli));

    fastrace::set_reporter(ConsoleReporter, fastrace::collector::Config::default());

    let console_layer = console_subscriber::spawn();

    tracing_subscriber::registry()
        .with(console_layer)
        .with(IndicatifLayer::new())
        .init();

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
        Commands::Query {
            output,
            query: query_str,
        } => {
            query::query(&mut stdout, options, *output, query_str).await?;
        }
    }

    fastrace::flush();
    stdout.flush().await?;
    Ok(())
}
