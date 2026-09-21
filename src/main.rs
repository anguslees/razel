use crate::bazel::InvocationOptions;
use crate::query::QueryOutput;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use fastrace::collector::ConsoleReporter;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod bazel;
mod bep;
mod query;
mod shared_error;
mod starlark;
pub mod stream_tee;
mod workspace;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(args_override_self = true)]
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

    /// Write build events as length-delimited protobuf messages
    #[arg(long = "build_event_binary_file", global = true, value_name = "PATH")]
    pub build_event_binary_file: Option<PathBuf>,

    /// Write build events as newline-delimited protobuf JSON
    #[arg(long = "build_event_json_file", global = true, value_name = "PATH")]
    pub build_event_json_file: Option<PathBuf>,

    /// Write build events in protobuf text format
    #[arg(long = "build_event_text_file", global = true, value_name = "PATH")]
    pub build_event_text_file: Option<PathBuf>,
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

#[derive(Debug)]
enum RazelError {
    Cli(clap::Error),
    BuildFailure(anyhow::Error),
    CommandLineError(anyhow::Error),
    QuerySyntax(query::QuerySyntaxError),
    QueryFailure(anyhow::Error),
    Interrupted(anyhow::Error),
    LocalEnvironmentalError(anyhow::Error),
    Context { error: Box<Self>, message: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BazelExitCode {
    Success,
    BuildFailure,
    CommandLineError,
    AnalysisFailure,
    Interrupted,
    LocalEnvironmentalError,
}

impl BazelExitCode {
    // Keep this list aligned with https://bazel.build/run/scripts#exit-codes.
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::BuildFailure => 1,
            Self::CommandLineError => 2,
            Self::AnalysisFailure => 7,
            Self::Interrupted => 8,
            Self::LocalEnvironmentalError => 36,
        }
    }

    pub(crate) fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Success,
            2 => Self::CommandLineError,
            7 => Self::AnalysisFailure,
            8 => Self::Interrupted,
            36 => Self::LocalEnvironmentalError,
            _ => Self::BuildFailure,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::BuildFailure => "BUILD_FAILURE",
            Self::CommandLineError => "COMMAND_LINE_ERROR",
            Self::AnalysisFailure => "ANALYSIS_FAILURE",
            Self::Interrupted => "INTERRUPTED",
            Self::LocalEnvironmentalError => "LOCAL_ENVIRONMENTAL_ERROR",
        }
    }
}

impl RazelError {
    fn from_query(error: query::QueryError) -> Self {
        match error {
            query::QueryError::Syntax(error) => Self::QuerySyntax(error),
            query::QueryError::NotInWorkspace(error) => Self::CommandLineError(error.into()),
            query::QueryError::Environment(error) | query::QueryError::Output(error) => {
                Self::LocalEnvironmentalError(error.into())
            }
            query::QueryError::Evaluation(error) => Self::QueryFailure(error),
        }
    }

    fn exit_code(&self) -> BazelExitCode {
        match self {
            Self::Cli(error) if error.exit_code() == 0 => BazelExitCode::Success,
            Self::Cli(_) | Self::CommandLineError(_) | Self::QuerySyntax(_) => {
                BazelExitCode::CommandLineError
            }
            Self::QueryFailure(_) => BazelExitCode::AnalysisFailure,
            Self::BuildFailure(_) => BazelExitCode::BuildFailure,
            Self::Interrupted(_) => BazelExitCode::Interrupted,
            Self::LocalEnvironmentalError(_) => BazelExitCode::LocalEnvironmentalError,
            Self::Context { error, .. } => error.exit_code(),
        }
    }

    fn context(self, message: impl fmt::Display) -> Self {
        Self::Context {
            error: Box::new(self),
            message: message.to_string(),
        }
    }

    fn report(&self) {
        if let Self::Cli(error) = self {
            if let Err(print_error) = error.print() {
                eprintln!("Error: {print_error}");
            }
        } else {
            eprintln!("Error: {self}");
        }
    }
}

impl fmt::Display for RazelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli(error) => error.fmt(formatter),
            Self::BuildFailure(error)
            | Self::CommandLineError(error)
            | Self::QueryFailure(error)
            | Self::Interrupted(error)
            | Self::LocalEnvironmentalError(error) => write!(formatter, "{error:#}"),
            Self::QuerySyntax(error) => error.fmt(formatter),
            Self::Context { error, message } => write!(formatter, "{error}\n{message}"),
        }
    }
}

impl std::error::Error for RazelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cli(error) => Some(error),
            Self::BuildFailure(error)
            | Self::CommandLineError(error)
            | Self::QueryFailure(error)
            | Self::Interrupted(error)
            | Self::LocalEnvironmentalError(error) => Some(error.as_ref()),
            Self::QuerySyntax(error) => Some(error),
            Self::Context { error, .. } => Some(error),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let exit_code = error.exit_code();
            error.report();
            ExitCode::from(exit_code.code())
        }
    }
}

async fn run() -> Result<(), RazelError> {
    let mut stdout = tokio::io::stdout();

    let matches = match Cli::command().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => {
            if error.exit_code() != 0 {
                let args = std::env::args_os()
                    .skip(1)
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                if let Err(bep_error) = bep::report_command_line_error(&args).await {
                    return Err(RazelError::LocalEnvironmentalError(
                        bep_error.context(format!("command line parsing also failed: {error}")),
                    ));
                }
            }
            return Err(RazelError::Cli(error));
        }
    };
    let cli = Cli::from_arg_matches(&matches).map_err(RazelError::Cli)?;
    let shutdown_signal = ShutdownSignal::new()
        .map_err(anyhow::Error::from)
        .map_err(RazelError::LocalEnvironmentalError)?;
    let options = Arc::new(InvocationOptions::from_flags(&cli));
    let (bep_layer, bep_handle) = bep::BepLayer::new(&cli, &matches)
        .await
        .map_err(RazelError::LocalEnvironmentalError)?;

    fastrace::set_reporter(ConsoleReporter, fastrace::collector::Config::default());

    let console_layer = console_subscriber::spawn();

    tracing_subscriber::registry()
        .with(console_layer)
        .with(IndicatifLayer::new())
        .with(bep_layer)
        .init();

    bep::started();
    bep::command_line();
    bep::options_parsed();

    let command = async {
        match &cli.command {
            Commands::Version => {
                // The version is automatically handled by clap if --version is passed.
                // This explicit subcommand can be used if `razel version` is preferred.
                println!("Razel version: {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            Commands::Build { targets } => {
                println!("Building targets: {targets:?}");
                bep::pattern_aborted();
                Err(RazelError::BuildFailure(anyhow::anyhow!(
                    "Build command is not yet implemented."
                )))
            }
            Commands::Test { targets } => {
                println!("Testing targets: {targets:?}");
                bep::pattern_aborted();
                Err(RazelError::BuildFailure(anyhow::anyhow!(
                    "Test command is not yet implemented."
                )))
            }
            Commands::Run { target } => {
                println!("Running target: {target}");
                bep::pattern_aborted();
                Err(RazelError::BuildFailure(anyhow::anyhow!(
                    "Run command is not yet implemented."
                )))
            }
            Commands::Query {
                output,
                query: query_str,
            } => query::query(&mut stdout, options, *output, query_str)
                .await
                .map_err(RazelError::from_query),
        }
    };

    let result = tokio::select! {
        result = command => result,
        signal = shutdown_signal.recv() => {
            bep::pattern_aborted();
            match signal {
                Ok(()) => Err(RazelError::Interrupted(anyhow::anyhow!("Command interrupted"))),
                Err(error) => Err(RazelError::LocalEnvironmentalError(error.into())),
            }
        }
    };

    let exit_code = result
        .as_ref()
        .map_or_else(RazelError::exit_code, |()| BazelExitCode::Success);
    bep::finished(exit_code);
    fastrace::flush();
    let bep_result = bep_handle
        .shutdown()
        .map_err(RazelError::LocalEnvironmentalError);

    match (result, bep_result) {
        (Err(error), Err(bep_error)) => {
            Err(error.context(format!("also failed to write BEP output: {bep_error:#}")))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(unix)]
struct ShutdownSignal {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ShutdownSignal {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn recv(mut self) -> std::io::Result<()> {
        tokio::select! {
            _ = self.interrupt.recv() => Ok(()),
            _ = self.terminate.recv() => Ok(()),
        }
    }
}

#[cfg(not(unix))]
struct ShutdownSignal;

#[cfg(not(unix))]
impl ShutdownSignal {
    fn new() -> std::io::Result<Self> {
        Ok(Self)
    }

    async fn recv(self) -> std::io::Result<()> {
        tokio::signal::ctrl_c().await
    }
}
