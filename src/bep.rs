use crate::workspace::Workspace;
use crate::{BazelExitCode, Cli, Commands};
use anyhow::{Context, Result};
use bytes::BytesMut;
use clap::parser::ValueSource;
use clap::{ArgMatches, ValueEnum};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use uuid::Uuid;

#[cfg(not(bazel))]
#[allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/bep_proto.rs"));
    pub use build_event_stream::*;
}

#[cfg(bazel)]
use build_event_stream_proto::build_event_stream as proto;
use prost_types::Timestamp;

const DESCRIPTOR_BYTES: &[u8] = include_bytes!(env!("RAZEL_BEP_DESCRIPTOR_PATH"));

const TRACE_TARGET: &str = "razel::bep";
const OUTPUT_BUFFER_CAPACITY: usize = 64 * 1_024;

pub(crate) struct BepLayer {
    state: Arc<Mutex<State>>,
}

pub(crate) struct BepHandle {
    state: Option<Arc<Mutex<State>>>,
}

struct Invocation {
    args: Vec<String>,
    command: String,
    explicit_options: Vec<String>,
    host: String,
    patterns: Vec<String>,
    start_time: Timestamp,
    user: String,
    uuid: String,
    working_directory: String,
    workspace_directory: String,
}

#[derive(Default)]
struct OutputPaths {
    binary: Option<PathBuf>,
    json: Option<PathBuf>,
    text: Option<PathBuf>,
}

struct State {
    invocation: Invocation,
    writer: Writer,
    writer_error: Option<std::io::Error>,
    started: bool,
    command_line_emitted: bool,
    options_parsed_emitted: bool,
    pattern_emitted: bool,
    finished: bool,
}

enum Sink {
    Binary(BufWriter<File>),
    Json(BufWriter<File>),
    Text(BufWriter<File>),
}

struct Writer {
    binary_buffer: BytesMut,
    descriptor: prost_reflect::MessageDescriptor,
    message_buffer: BytesMut,
    sinks: Vec<Sink>,
}

#[derive(Default)]
struct TraceEventVisitor {
    event: Option<String>,
    exit_code: Option<u8>,
}

impl BepLayer {
    pub(crate) async fn new(cli: &Cli, matches: &ArgMatches) -> Result<(Option<Self>, BepHandle)> {
        let paths = OutputPaths::from_cli(cli);
        if paths.is_empty() {
            return Ok((None, BepHandle { state: None }));
        }

        let invocation = Invocation::new(cli, matches).await?;
        let writer = Writer::new(paths)?;
        let state = Arc::new(Mutex::new(State::new(invocation, writer)));
        Ok((
            Some(Self {
                state: state.clone(),
            }),
            BepHandle { state: Some(state) },
        ))
    }
}

impl BepHandle {
    pub(crate) fn shutdown(mut self) -> Result<()> {
        let Some(state) = self.state.take() else {
            return Ok(());
        };
        state
            .lock()
            .map_err(|_| anyhow::anyhow!("Build Event Protocol state lock was poisoned"))?
            .shutdown()
    }
}

pub(crate) async fn report_command_line_error(args: &[String]) -> Result<()> {
    let paths = OutputPaths::from_args(args);
    if paths.is_empty() {
        return Ok(());
    }

    let invocation = Invocation::for_command_line_error(args).await?;
    let writer = Writer::new(paths)?;
    let mut state = State::new(invocation, writer);
    let events = [
        state.started_event(),
        state.command_line_event(),
        state.options_parsed_event(),
        state.finished_event(BazelExitCode::CommandLineError),
    ];
    for event in events {
        state.write_event(&event);
    }
    state.shutdown()
}

impl<S> Layer<S> for BepLayer
where
    S: Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != TRACE_TARGET {
            return;
        }

        let mut visitor = TraceEventVisitor::default();
        event.record(&mut visitor);
        let Some(event) = visitor.event.as_deref() else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        let build_event = match event {
            "started" if !state.started => {
                state.started = true;
                Some(state.started_event())
            }
            "command_line" if state.started && !state.command_line_emitted && !state.finished => {
                state.command_line_emitted = true;
                Some(state.command_line_event())
            }
            "options_parsed"
                if state.started && !state.options_parsed_emitted && !state.finished =>
            {
                state.options_parsed_emitted = true;
                Some(state.options_parsed_event())
            }
            "pattern_aborted" if state.started && !state.pattern_emitted && !state.finished => {
                state.pattern_emitted = true;
                state.pattern_aborted_event()
            }
            "finished" if state.started && !state.finished => {
                state.finished = true;
                Some(state.finished_event(BazelExitCode::from_code(visitor.exit_code.unwrap_or(1))))
            }
            _ => None,
        };
        if let Some(event) = build_event {
            state.write_event(&event);
        }
    }
}

impl Visit for TraceEventVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "exit_code" {
            self.exit_code = u8::try_from(value).ok();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "bep_event" {
            self.event = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl Invocation {
    async fn new(cli: &Cli, matches: &ArgMatches) -> Result<Self> {
        let args: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let working_directory =
            std::env::current_dir().context("failed to determine the working directory for BEP")?;
        let workspace_directory = Workspace::find_root(&working_directory)
            .await
            .context("failed to find the workspace directory for BEP")?
            .unwrap_or_else(|| working_directory.clone());
        let explicit_options = explicit_options(cli, matches);
        let (command, patterns) = match &cli.command {
            Commands::Version => ("version", Vec::new()),
            Commands::Build { targets } => ("build", targets.clone()),
            Commands::Test { targets } => ("test", targets.clone()),
            Commands::Run { target } => ("run", vec![target.clone()]),
            Commands::Query { .. } => ("query", Vec::new()),
        };

        Ok(Self {
            args,
            command: command.to_owned(),
            explicit_options,
            host: hostname(),
            patterns,
            start_time: timestamp_now(),
            user: username(),
            uuid: Uuid::new_v4().to_string(),
            working_directory: working_directory.display().to_string(),
            workspace_directory: workspace_directory.display().to_string(),
        })
    }

    async fn for_command_line_error(args: &[String]) -> Result<Self> {
        // Full Clap parsing failed, so this fallback can only recover basic BEP metadata.
        let working_directory =
            std::env::current_dir().context("failed to determine the working directory for BEP")?;
        let workspace_directory = Workspace::find_root(&working_directory)
            .await
            .context("failed to find the workspace directory for BEP")?
            .unwrap_or_else(|| working_directory.clone());
        let command = args
            .iter()
            .find(|arg| matches!(arg.as_str(), "version" | "build" | "test" | "run" | "query"))
            .cloned()
            .unwrap_or_default();
        Ok(Self {
            args: args.to_vec(),
            command,
            explicit_options: explicit_options_for_parse_error(args),
            host: hostname(),
            patterns: Vec::new(),
            start_time: timestamp_now(),
            user: username(),
            uuid: Uuid::new_v4().to_string(),
            working_directory: working_directory.display().to_string(),
            workspace_directory: workspace_directory.display().to_string(),
        })
    }
}

impl OutputPaths {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            binary: cli.build_event_binary_file.clone(),
            json: cli.build_event_json_file.clone(),
            text: cli.build_event_text_file.clone(),
        }
    }

    fn from_args(args: &[String]) -> Self {
        // This intentionally limited parser is only for reporting Clap failures. Do not use it
        // once standard command-line parsing has succeeded.
        let mut paths = Self::default();
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                break;
            }
            let (path, exact_flag) = if arg.starts_with("--build_event_binary_file=") {
                (&mut paths.binary, false)
            } else if arg.starts_with("--build_event_json_file=") {
                (&mut paths.json, false)
            } else if arg.starts_with("--build_event_text_file=") {
                (&mut paths.text, false)
            } else if arg == "--build_event_binary_file" {
                (&mut paths.binary, true)
            } else if arg == "--build_event_json_file" {
                (&mut paths.json, true)
            } else if arg == "--build_event_text_file" {
                (&mut paths.text, true)
            } else {
                index += 1;
                continue;
            };

            if exact_flag {
                if let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-')) {
                    *path = Some(value.into());
                    index += 1;
                }
            } else if let Some(value) = arg.split_once('=').map(|(_, value)| value)
                && !value.is_empty()
            {
                *path = Some(value.into());
            }
            index += 1;
        }
        paths
    }

    fn is_empty(&self) -> bool {
        self.binary.is_none() && self.json.is_none() && self.text.is_none()
    }
}

impl State {
    fn new(invocation: Invocation, writer: Writer) -> Self {
        Self {
            invocation,
            writer,
            writer_error: None,
            started: false,
            command_line_emitted: false,
            options_parsed_emitted: false,
            pattern_emitted: false,
            finished: false,
        }
    }

    fn write_event(&mut self, event: &proto::BuildEvent) {
        if self.writer_error.is_none() {
            self.writer_error = self.writer.write_event(event).err();
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        let flush_error = self.writer.flush().err();
        if let Some(error) = self.writer_error.take() {
            return Err(error).context("failed to write Build Event Protocol output");
        }
        if let Some(error) = flush_error {
            return Err(error).context("failed to flush Build Event Protocol output");
        }
        Ok(())
    }

    // Bazel still populates the deprecated millisecond field for older BEP consumers.
    #[allow(deprecated)]
    fn started_event(&self) -> proto::BuildEvent {
        let mut children = vec![command_line_id(), options_parsed_id()];
        if !self.invocation.patterns.is_empty() {
            children.push(pattern_id(&self.invocation.patterns));
        }
        children.push(finished_id());

        proto::BuildEvent {
            id: Some(started_id()),
            children,
            last_message: false,
            payload: Some(proto::build_event::Payload::Started(proto::BuildStarted {
                uuid: self.invocation.uuid.clone(),
                start_time_millis: timestamp_millis(&self.invocation.start_time),
                start_time: Some(self.invocation.start_time),
                build_tool_version: env!("CARGO_PKG_VERSION").to_owned(),
                options_description: self.invocation.explicit_options.join(" "),
                command: self.invocation.command.clone(),
                working_directory: self.invocation.working_directory.clone(),
                workspace_directory: self.invocation.workspace_directory.clone(),
                server_pid: i64::from(std::process::id()),
                host: self.invocation.host.clone(),
                user: self.invocation.user.clone(),
            })),
        }
    }

    fn command_line_event(&self) -> proto::BuildEvent {
        proto::BuildEvent {
            id: Some(command_line_id()),
            children: Vec::new(),
            last_message: false,
            payload: Some(proto::build_event::Payload::UnstructuredCommandLine(
                proto::UnstructuredCommandLine {
                    args: self.invocation.args.clone(),
                },
            )),
        }
    }

    fn options_parsed_event(&self) -> proto::BuildEvent {
        proto::BuildEvent {
            id: Some(options_parsed_id()),
            children: Vec::new(),
            last_message: false,
            payload: Some(proto::build_event::Payload::OptionsParsed(
                proto::OptionsParsed {
                    startup_options: Vec::new(),
                    explicit_startup_options: Vec::new(),
                    cmd_line: self.invocation.explicit_options.clone(),
                    explicit_cmd_line: self.invocation.explicit_options.clone(),
                    invocation_policy: None,
                    tool_tag: String::new(),
                },
            )),
        }
    }

    fn pattern_aborted_event(&self) -> Option<proto::BuildEvent> {
        if self.invocation.patterns.is_empty() {
            return None;
        }
        Some(proto::BuildEvent {
            id: Some(pattern_id(&self.invocation.patterns)),
            children: Vec::new(),
            last_message: false,
            payload: Some(proto::build_event::Payload::Aborted(proto::Aborted {
                reason: proto::aborted::AbortReason::Incomplete.into(),
                description: "target pattern expansion is not implemented".to_owned(),
            })),
        })
    }

    // Bazel still populates the deprecated millisecond field for older BEP consumers.
    #[allow(deprecated)]
    fn finished_event(&self, exit_code: BazelExitCode) -> proto::BuildEvent {
        let finish_time = timestamp_now();
        proto::BuildEvent {
            id: Some(finished_id()),
            children: Vec::new(),
            last_message: true,
            payload: Some(proto::build_event::Payload::Finished(
                proto::BuildFinished {
                    overall_success: exit_code == BazelExitCode::Success,
                    finish_time_millis: timestamp_millis(&finish_time),
                    exit_code: Some(proto::build_finished::ExitCode {
                        name: exit_code.name().to_owned(),
                        code: i32::from(exit_code.code()),
                    }),
                    finish_time: Some(finish_time),
                    anomaly_report: None,
                    failure_detail: None,
                },
            )),
        }
    }
}

impl Writer {
    fn new(paths: OutputPaths) -> Result<Self> {
        let pool = DescriptorPool::decode(DESCRIPTOR_BYTES)
            .context("failed to decode the Build Event Protocol descriptor set")?;
        let descriptor = pool
            .get_message_by_name("build_event_stream.BuildEvent")
            .context("Build Event Protocol descriptor set has no BuildEvent message")?;
        // Bazel accepts its binary, JSON, and text file flags simultaneously.
        let mut sinks = Vec::new();
        if let Some(path) = paths.binary {
            sinks.push(Sink::Binary(open(&path)?));
        }
        if let Some(path) = paths.json {
            sinks.push(Sink::Json(open(&path)?));
        }
        if let Some(path) = paths.text {
            sinks.push(Sink::Text(open(&path)?));
        }
        Ok(Self {
            binary_buffer: BytesMut::new(),
            descriptor,
            message_buffer: BytesMut::new(),
            sinks,
        })
    }

    fn write_event(&mut self, event: &proto::BuildEvent) -> std::io::Result<()> {
        self.message_buffer.clear();
        event
            .encode(&mut self.message_buffer)
            .map_err(std::io::Error::other)?;
        let dynamic = DynamicMessage::decode(self.descriptor.clone(), self.message_buffer.as_ref())
            .map_err(std::io::Error::other)?;
        self.binary_buffer.clear();
        event
            .encode_length_delimited(&mut self.binary_buffer)
            .map_err(std::io::Error::other)?;
        for sink in &mut self.sinks {
            sink.write_event(&self.binary_buffer, &dynamic)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        for sink in &mut self.sinks {
            sink.flush()?;
        }
        Ok(())
    }
}

impl Sink {
    fn write_event(&mut self, binary: &[u8], dynamic: &DynamicMessage) -> std::io::Result<()> {
        match self {
            Self::Binary(writer) => writer.write_all(binary),
            Self::Json(writer) => {
                let mut encoded = Vec::new();
                let mut serializer = serde_json::Serializer::new(&mut encoded);
                dynamic
                    .serialize_with_options(&mut serializer, &SerializeOptions::new())
                    .map_err(std::io::Error::other)?;
                encoded.push(b'\n');
                writer.write_all(&encoded)
            }
            Self::Text(writer) => {
                let message = format!("{dynamic:#}");
                writer.write_all(b"event {\n")?;
                writer.write_all(message.as_bytes())?;
                writer.write_all(b"\n}\n\n")
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Binary(writer) | Self::Json(writer) | Self::Text(writer) => writer.flush(),
        }
    }
}

fn open(path: &Path) -> Result<BufWriter<File>> {
    File::create(path)
        .map(|file| BufWriter::with_capacity(OUTPUT_BUFFER_CAPACITY, file))
        .with_context(|| format!("failed to create BEP output file {}", path.display()))
}

fn started_id() -> proto::BuildEventId {
    event_id(proto::build_event_id::Id::Started(
        proto::build_event_id::BuildStartedId {},
    ))
}

fn command_line_id() -> proto::BuildEventId {
    event_id(proto::build_event_id::Id::UnstructuredCommandLine(
        proto::build_event_id::UnstructuredCommandLineId {},
    ))
}

fn options_parsed_id() -> proto::BuildEventId {
    event_id(proto::build_event_id::Id::OptionsParsed(
        proto::build_event_id::OptionsParsedId {},
    ))
}

fn pattern_id(patterns: &[String]) -> proto::BuildEventId {
    event_id(proto::build_event_id::Id::Pattern(
        proto::build_event_id::PatternExpandedId {
            pattern: patterns.to_vec(),
        },
    ))
}

fn finished_id() -> proto::BuildEventId {
    event_id(proto::build_event_id::Id::BuildFinished(
        proto::build_event_id::BuildFinishedId {},
    ))
}

fn event_id(id: proto::build_event_id::Id) -> proto::BuildEventId {
    proto::BuildEventId { id: Some(id) }
}

fn timestamp_now() -> Timestamp {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
    let nanos = i32::try_from(duration.subsec_nanos()).unwrap_or(999_999_999);
    Timestamp { seconds, nanos }
}

fn timestamp_millis(timestamp: &Timestamp) -> i64 {
    timestamp
        .seconds
        .saturating_mul(1_000)
        .saturating_add(i64::from(timestamp.nanos / 1_000_000))
}

#[cfg(unix)]
fn hostname() -> String {
    // SAFETY: on success uname initializes utsname, whose fields are NUL-terminated C strings.
    let hostname = unsafe {
        let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if libc::uname(name.as_mut_ptr()) != 0 {
            return hostname_from_env();
        }
        let name = name.assume_init();
        std::ffi::CStr::from_ptr(name.nodename.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    if let Some(short) = hostname.split('.').next()
        && !short.is_empty()
    {
        return short.to_owned();
    }
    hostname_from_env()
}

#[cfg(not(unix))]
fn hostname() -> String {
    hostname_from_env()
}

fn hostname_from_env() -> String {
    first_environment_value(&["HOSTNAME", "COMPUTERNAME"])
        .split('.')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn username() -> String {
    first_environment_value(&["USER", "LOGNAME", "USERNAME"])
}

fn first_environment_value(names: &[&str]) -> String {
    names
        .iter()
        .filter_map(std::env::var_os)
        .map(|value| value.to_string_lossy().into_owned())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn explicit_options(cli: &Cli, matches: &ArgMatches) -> Vec<String> {
    let mut options = Vec::new();
    if matches.value_source("ignore_dev_dependency") == Some(ValueSource::CommandLine) {
        options.push(format!(
            "--ignore_dev_dependency={}",
            cli.ignore_dev_dependency
        ));
    }
    for (name, path) in [
        ("build_event_binary_file", &cli.build_event_binary_file),
        ("build_event_json_file", &cli.build_event_json_file),
        ("build_event_text_file", &cli.build_event_text_file),
    ] {
        if matches.value_source(name) == Some(ValueSource::CommandLine)
            && let Some(path) = path
        {
            options.push(format!("--{name}={}", path.display()));
        }
    }
    if let Commands::Query { output, .. } = &cli.command
        && matches
            .subcommand_matches("query")
            .and_then(|matches| matches.value_source("output"))
            == Some(ValueSource::CommandLine)
    {
        options.push(format!(
            "--output={}",
            output.to_possible_value().unwrap().get_name()
        ));
    }
    options
}

fn explicit_options_for_parse_error(args: &[String]) -> Vec<String> {
    // Full Clap parsing failed. Keep this scanner private to that fallback rather than treating it
    // as a second command-line parser.
    const FLAGS_WITH_VALUES: &[&str] = &[
        "--build_event_binary_file",
        "--build_event_json_file",
        "--build_event_text_file",
        "--output",
    ];

    let mut options = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if !arg.starts_with('-') {
            continue;
        }
        if !arg.contains('=') && FLAGS_WITH_VALUES.contains(&arg.as_str()) {
            if let Some(value) = args.next() {
                options.push(format!("{arg}={value}"));
            } else {
                options.push(arg.clone());
            }
        } else {
            options.push(arg.clone());
        }
    }
    options
}

// These helpers emit ordinary tracing events; BepLayer is only one possible subscriber.
pub(crate) fn started() {
    tracing::info!(target: TRACE_TARGET, bep_event = "started");
}

pub(crate) fn command_line() {
    tracing::info!(target: TRACE_TARGET, bep_event = "command_line");
}

pub(crate) fn options_parsed() {
    tracing::info!(target: TRACE_TARGET, bep_event = "options_parsed");
}

pub(crate) fn pattern_aborted() {
    tracing::info!(target: TRACE_TARGET, bep_event = "pattern_aborted");
}

pub(crate) fn finished(exit_code: BazelExitCode) {
    tracing::info!(
        target: TRACE_TARGET,
        bep_event = "finished",
        exit_code = i64::from(exit_code.code())
    );
}
