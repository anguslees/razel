use crate::bazel::InvocationOptions;
use crate::query::{OrderOutput, QueryOutput};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use fastrace::collector::ConsoleReporter;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod bazel;
mod bep;
mod cquery;
mod query;
mod query_syntax;
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
    Build {
        #[command(flatten)]
        options: BuildOptions,
        targets: Vec<String>,
    },
    /// Tests the specified targets
    Test {
        #[command(flatten)]
        options: BuildOptions,
        targets: Vec<String>,
    },
    /// Runs the specified target
    Run {
        #[command(flatten)]
        options: BuildOptions,
        target: String,
    },
    /// Queries for information about the build graph
    Query(QueryArgs),
    /// Queries the configured target graph
    Cquery(CqueryArgs),
}

/// Options of every query command, like Bazel's `CommonQueryOptions`.
#[derive(Args, Debug)]
pub struct CommonQueryArgs {
    #[arg(long, value_enum, default_value_t = QueryOutput::Label)]
    output: QueryOutput,
    #[arg(required_unless_present = "query_file", num_args = 1..)]
    query: Vec<String>,
    #[arg(long = "query_file", conflicts_with = "query")]
    query_file: Option<std::path::PathBuf>,
}

impl CommonQueryArgs {
    async fn expression(&self) -> anyhow::Result<String> {
        use anyhow::Context;

        match &self.query_file {
            Some(path) => {
                // Bazel reports the file relative to the client's working directory.
                let path =
                    std::env::current_dir().map_or_else(|_| path.clone(), |cwd| cwd.join(path));
                tokio::fs::read_to_string(&path)
                    .await
                    .with_context(|| format!("I/O error reading from {}", path.display()))
            }
            None => Ok(self.query.join(" ")),
        }
    }
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    #[command(flatten)]
    query: CommonQueryArgs,
    /// How to order the results
    #[arg(long = "order_output", value_enum, default_value_t = OrderOutput::Auto)]
    order_output: OrderOutput,
}

#[derive(Args, Debug)]
pub struct CqueryArgs {
    #[command(flatten)]
    query: CommonQueryArgs,
    #[command(flatten)]
    build: BuildOptions,
}

/// Options shared by commands that perform configured analysis.
#[derive(Args, Debug)]
pub struct BuildOptions {
    #[arg(long, default_value = default_cpu())]
    cpu: String,
    #[arg(long = "host_cpu", default_value = default_cpu())]
    host_cpu: String,
    #[arg(
        short = 'c',
        long = "compilation_mode",
        default_value = "fastbuild",
        value_parser = ["fastbuild", "dbg", "opt"]
    )]
    compilation_mode: String,
    #[arg(
        long = "host_compilation_mode",
        default_value = "opt",
        value_parser = ["fastbuild", "dbg", "opt"]
    )]
    host_compilation_mode: String,
    #[arg(
        long = "force_pic",
        require_equals = true,
        default_missing_value = "true",
        num_args(0..=1),
        value_parser = bazel::config::parse_bool,
        overrides_with = "no_force_pic"
    )]
    force_pic: Option<bool>,
    #[arg(
        long = "noforce_pic",
        require_equals = true,
        default_missing_value = "true",
        num_args(0..=1),
        value_parser = bazel::config::parse_bool
    )]
    no_force_pic: Option<bool>,
    #[arg(long = "define", value_parser = parse_define)]
    defines: Vec<(String, String)>,
    #[arg(long = "platforms")]
    platform: Option<String>,
}

fn parse_define(value: &str) -> Result<(String, String), String> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| "defines must have the form NAME=VALUE".to_owned())?;
    if name.is_empty() {
        return Err("define name cannot be empty".to_owned());
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn default_cpu() -> &'static str {
    cpu_for_host(std::env::consts::OS, std::env::consts::ARCH)
}

fn cpu_for_host(os: &str, architecture: &str) -> &'static str {
    match (os, architecture) {
        ("macos", "aarch64") => "darwin_arm64",
        ("macos", "x86_64") => "darwin_x86_64",
        ("windows", "x86_64") => "x64_windows",
        ("windows", "aarch64") => "arm64_windows",
        ("freebsd", _) => "freebsd",
        ("openbsd", _) => "openbsd",
        ("linux", "x86") => "piii",
        ("linux", "x86_64") => "k8",
        ("linux", "powerpc" | "powerpc64") => "ppc",
        ("linux", "arm") => "arm",
        ("linux", "aarch64") => "aarch64",
        ("linux", "s390x") => "s390x",
        ("linux", "mips64") => "mips64",
        ("linux", "riscv64") => "riscv64",
        ("linux", "loongarch64") => "loongarch64",
        _ => "unknown",
    }
}

#[test]
fn host_cpu_defaults_use_bazel_names() {
    for (os, architecture, expected) in [
        ("macos", "aarch64", "darwin_arm64"),
        ("macos", "x86_64", "darwin_x86_64"),
        ("windows", "x86_64", "x64_windows"),
        ("windows", "aarch64", "arm64_windows"),
        ("linux", "x86", "piii"),
        ("linux", "x86_64", "k8"),
        ("linux", "aarch64", "aarch64"),
        ("linux", "powerpc64", "ppc"),
        ("freebsd", "x86_64", "freebsd"),
        ("openbsd", "aarch64", "openbsd"),
        ("other", "x86_64", "unknown"),
    ] {
        assert_eq!(cpu_for_host(os, architecture), expected);
    }
}

#[test]
fn build_boolean_flags_accept_bazel_spellings() {
    for command in ["build", "test", "run", "cquery"] {
        for (value, expected) in [
            ("true", true),
            ("T", true),
            ("1", true),
            ("YES", true),
            ("y", true),
            ("false", false),
            ("F", false),
            ("0", false),
            ("NO", false),
            ("n", false),
        ] {
            for flag in ["force_pic", "noforce_pic"] {
                let cli = Cli::try_parse_from([
                    "razel",
                    command,
                    &format!("--{flag}={value}"),
                    "//:target",
                ])
                .unwrap();
                let options = match cli.command {
                    Commands::Build { options, .. }
                    | Commands::Test { options, .. }
                    | Commands::Run { options, .. } => options,
                    Commands::Cquery(args) => args.build,
                    _ => unreachable!(),
                };
                assert_eq!(
                    if flag == "force_pic" {
                        options.force_pic
                    } else {
                        options.no_force_pic
                    },
                    Some(expected)
                );
            }
        }
        assert!(Cli::try_parse_from(["razel", command, "--force_pic=maybe", "//:target"]).is_err());
    }
}

#[test]
fn verify_cli() {
    Cli::command().debug_assert();

    let cli = Cli::try_parse_from(["razel", "query", "//:all"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Query(QueryArgs {
            query: CommonQueryArgs {
                output: QueryOutput::Label,
                ..
            },
            order_output: OrderOutput::Auto,
        })
    ));

    let cli = Cli::try_parse_from([
        "razel",
        "query",
        "--output=label_kind",
        "--order_output=no",
        "//:all",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Query(QueryArgs {
            query: CommonQueryArgs {
                output: QueryOutput::LabelKind,
                ..
            },
            order_output: OrderOutput::No,
        })
    ));
    // Like Bazel, cquery has no --order_output.
    assert!(Cli::try_parse_from(["razel", "cquery", "--order_output=no", "//:all"]).is_err());

    let cli = Cli::try_parse_from([
        "razel",
        "cquery",
        "--cpu=arm",
        "--define=feature=on",
        "deps(//:all)",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Commands::Cquery(CqueryArgs { build: BuildOptions { cpu, defines, .. }, .. })
            if cpu == "arm" && defines == [("feature".to_owned(), "on".to_owned())]
    ));

    for command in ["build", "test", "run", "cquery"] {
        let cli = Cli::try_parse_from([
            "razel",
            command,
            "--cpu=arm",
            "--define=feature=on",
            "//:target",
        ])
        .unwrap();
        let build = match cli.command {
            Commands::Build { options, .. }
            | Commands::Test { options, .. }
            | Commands::Run { options, .. } => options,
            Commands::Cquery(args) => args.build,
            _ => unreachable!(),
        };
        assert_eq!(build.cpu, "arm");
        assert_eq!(build.defines, [("feature".to_owned(), "on".to_owned())]);
    }
}

#[derive(Debug)]
enum RazelError {
    Cli(clap::Error),
    BuildFailure(anyhow::Error),
    CommandLineError(anyhow::Error),
    QuerySyntax(query_syntax::QuerySyntaxError),
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
            error @ query::QueryError::UnsupportedOrderOutput(_) => {
                Self::CommandLineError(error.into())
            }
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
            Commands::Build { targets, .. } => {
                println!("Building targets: {targets:?}");
                bep::pattern_aborted();
                Err(RazelError::BuildFailure(anyhow::anyhow!(
                    "Build command is not yet implemented."
                )))
            }
            Commands::Test { targets, .. } => {
                println!("Testing targets: {targets:?}");
                bep::pattern_aborted();
                Err(RazelError::BuildFailure(anyhow::anyhow!(
                    "Test command is not yet implemented."
                )))
            }
            Commands::Run { target, .. } => {
                println!("Running target: {target}");
                bep::pattern_aborted();
                Err(RazelError::BuildFailure(anyhow::anyhow!(
                    "Run command is not yet implemented."
                )))
            }
            Commands::Query(args) => {
                // Bazel classifies this as QUERY_FILE_READ_FAILURE, a command-line error.
                let expression = args
                    .query
                    .expression()
                    .await
                    .map_err(RazelError::CommandLineError)?;
                query::query(
                    &mut stdout,
                    options,
                    args.query.output,
                    args.order_output,
                    &expression,
                )
                .await
                .map_err(RazelError::from_query)
            }
            Commands::Cquery(args) => {
                let expression = args
                    .query
                    .expression()
                    .await
                    .map_err(RazelError::CommandLineError)?;
                cquery::cquery(
                    &mut stdout,
                    options,
                    &args.build,
                    args.query.output,
                    &expression,
                )
                .await
                .map_err(RazelError::from_query)
            }
        }
    };

    let result = tokio::select! {
        result = command => result,
        signal = shutdown_signal.recv() => {
            match signal {
                Ok(()) => {
                    bep::pattern_interrupted();
                    Err(RazelError::Interrupted(anyhow::anyhow!("Command interrupted")))
                }
                Err(error) => {
                    bep::pattern_aborted();
                    Err(RazelError::LocalEnvironmentalError(error.into()))
                }
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
