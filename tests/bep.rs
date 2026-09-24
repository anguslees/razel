use assert_cmd::Command;
use assert_fs::TempDir;
use prost::Message;
use serde_json::Value;
use std::fs;

#[cfg(not(bazel))]
#[allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/bep_proto.rs"));
    pub use build_event_stream::*;
}

#[cfg(bazel)]
use build_event_stream_proto::build_event_stream as proto;
use prost_types::Timestamp;
use proto::BuildEvent;
use proto::build_event::Payload;
use proto::build_event_id::Id as EventId;

// Verify the legacy millisecond fields stay synchronized with their replacement timestamps.
#[allow(deprecated)]
#[test]
fn writes_compatible_binary_json_and_text_streams() {
    let temp = TempDir::new().unwrap();
    let binary = temp.path().join("events.bin");
    let json = temp.path().join("events.json");
    let text = temp.path().join("events.txt");

    Command::new(assert_cmd::cargo::cargo_bin!("razel"))
        .arg("version")
        .arg(format!("--build_event_binary_file={}", binary.display()))
        .arg("--build_event_json_file")
        .arg(&json)
        .arg(format!("--build_event_text_file={}", text.display()))
        .assert()
        .success();

    let events = decode_events(&fs::read(binary).unwrap());
    assert_eq!(events.len(), 4);
    assert!(matches!(id_kind(&events[0]), Some(EventId::Started(_))));
    assert_eq!(events[0].children.len(), 3);
    for (announced, event) in events[0].children.iter().zip(&events[1..]) {
        assert_eq!(event.id.as_ref(), Some(announced));
    }
    let Some(Payload::Started(started)) = events[0].payload.as_ref() else {
        panic!("first event should be BuildStarted");
    };
    assert_eq!(started.command, "version");
    assert!(!started.host.is_empty());
    if let Some(user) = ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|user| !user.is_empty())
    {
        assert_eq!(started.user, user.to_string_lossy());
    }
    let start_time = started.start_time.as_ref().unwrap();
    assert_eq!(started.start_time_millis, timestamp_millis(start_time));

    let Some(Payload::Finished(finished)) = events[3].payload.as_ref() else {
        panic!("last event should be BuildFinished");
    };
    assert!(finished.overall_success);
    let finish_time = finished.finish_time.as_ref().unwrap();
    assert_eq!(finished.finish_time_millis, timestamp_millis(finish_time));
    assert!(events[3].last_message);
    assert_eq!(events.iter().filter(|event| event.last_message).count(), 1);

    let json_events = fs::read_to_string(&json)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(json_events.len(), 4);
    assert_eq!(json_events[0]["started"]["command"], "version");
    assert_eq!(
        json_events[2]["optionsParsed"]["explicitCmdLine"][1],
        format!("--build_event_json_file={}", json.display())
    );
    assert_eq!(json_events[3]["finished"]["overallSuccess"], true);
    assert_eq!(json_events[3]["lastMessage"], true);

    let text = fs::read_to_string(text).unwrap();
    assert_eq!(text.matches("event {\n").count(), 4);
    assert!(text.contains(&format!(
        "build_tool_version: \"{}\"",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(text.contains("overall_success: true"));
}

#[test]
fn reports_command_line_errors() {
    let temp = TempDir::new().unwrap();
    let clap_json = temp.path().join("clap-error.json");
    let query_json = temp.path().join("query-error.json");

    Command::new(assert_cmd::cargo::cargo_bin!("razel"))
        .arg("query")
        .arg(format!("--build_event_json_file={}", clap_json.display()))
        .assert()
        .code(2);
    assert_command_line_error(&clap_json);

    Command::new(assert_cmd::cargo::cargo_bin!("razel"))
        .arg("query")
        .arg("(")
        .arg(format!("--build_event_json_file={}", query_json.display()))
        .assert()
        .code(2);
    assert_command_line_error(&query_json);
}

#[test]
fn reports_no_workspace_as_command_line_error() {
    let temp = TempDir::new().unwrap();
    let json = temp.path().join("no-workspace.json");
    let binary = fs::canonicalize(assert_cmd::cargo::cargo_bin!("razel")).unwrap();

    Command::new(binary)
        .current_dir(temp.path())
        .arg("query")
        .arg("//:all")
        .arg(format!("--build_event_json_file={}", json.display()))
        .assert()
        .code(2);

    assert_command_line_error(&json);
}

#[cfg(unix)]
#[test]
fn workspace_io_failure_is_local_environmental_error() {
    let temp = TempDir::new().unwrap();
    std::os::unix::fs::symlink("MODULE.bazel", temp.path().join("MODULE.bazel")).unwrap();
    let binary = fs::canonicalize(assert_cmd::cargo::cargo_bin!("razel")).unwrap();

    Command::new(binary)
        .current_dir(temp.path())
        .arg("query")
        .arg("//:all")
        .assert()
        .code(36);
}

#[test]
fn repeated_output_flag_uses_last_path() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first.json");
    let second = temp.path().join("second.json");

    Command::new(assert_cmd::cargo::cargo_bin!("razel"))
        .arg("version")
        .arg(format!("--build_event_json_file={}", first.display()))
        .arg(format!("--build_event_json_file={}", second.display()))
        .assert()
        .success();

    assert!(!first.exists());
    assert_eq!(read_json_events(&second).len(), 4);
}

#[test]
fn reports_query_analysis_failure() {
    let temp = TempDir::new().unwrap();
    let json = temp.path().join("analysis-error.json");
    fs::write(
        temp.path().join("MODULE.bazel"),
        "module(name = \"analysis_error\", version = \"1.0\")\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("BUILD.bazel"),
        "unknown_rule(name = \"broken\")\n",
    )
    .unwrap();

    let binary = fs::canonicalize(assert_cmd::cargo::cargo_bin!("razel")).unwrap();
    Command::new(binary)
        .current_dir(temp.path())
        .arg("query")
        .arg("//:all")
        .arg(format!("--build_event_json_file={}", json.display()))
        .assert()
        .code(7);

    let events = read_json_events(&json);
    assert_eq!(events.len(), 4);
    assert_eq!(events[3]["finished"]["exitCode"]["code"], 7);
    assert_eq!(
        events[3]["finished"]["exitCode"]["name"],
        "ANALYSIS_FAILURE"
    );
}

#[cfg(unix)]
#[test]
fn flushes_interrupted_stream() {
    use std::fmt::Write;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let temp = TempDir::new().unwrap();
    let json = temp.path().join("interrupted.json");
    fs::write(
        temp.path().join("MODULE.bazel"),
        "module(name = \"signal_test\", version = \"1.0\")\n",
    )
    .unwrap();
    let mut build = String::from("exports_files([\n");
    for index in 0..2_000 {
        writeln!(build, "    \"target_{index}_{}\",", "x".repeat(180)).unwrap();
    }
    build.push_str("])\n");
    fs::write(temp.path().join("BUILD.bazel"), build).unwrap();

    let binary = fs::canonicalize(assert_cmd::cargo::cargo_bin!("razel")).unwrap();
    let mut child = std::process::Command::new(binary)
        .current_dir(temp.path())
        .arg("query")
        .arg("//:*")
        .arg(format!("--build_event_json_file={}", json.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let readiness_deadline = Instant::now() + Duration::from_secs(5);
    while fs::metadata(&json).is_err() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "razel exited before SIGTERM"
        );
        assert!(
            Instant::now() < readiness_deadline,
            "razel did not initialize BEP output"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: pid identifies the live child process, and SIGTERM requires no pointer arguments.
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(
        result,
        0,
        "failed to send SIGTERM: {}",
        std::io::Error::last_os_error()
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("razel did not exit after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(8),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let events = fs::read_to_string(json)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 4);
    assert_eq!(events[3]["finished"]["exitCode"]["code"], 8);
    assert_eq!(events[3]["finished"]["exitCode"]["name"], "INTERRUPTED");
    assert_eq!(events[3]["lastMessage"], true);
}

#[test]
fn reports_patterns_and_command_failure() {
    let temp = TempDir::new().unwrap();
    let json = temp.path().join("failed.json");

    Command::new(assert_cmd::cargo::cargo_bin!("razel"))
        .arg("build")
        .arg("//:missing")
        .arg(format!("--build_event_json_file={}", json.display()))
        .assert()
        .failure();

    let events = fs::read_to_string(json)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 5);
    assert_eq!(events[3]["id"]["pattern"]["pattern"][0], "//:missing");
    assert_eq!(events[3]["aborted"]["reason"], "INCOMPLETE");
    assert_eq!(events[4]["finished"]["exitCode"]["code"], 1);
    assert_eq!(events[4]["finished"]["exitCode"]["name"], "BUILD_FAILURE");
}

fn id_kind(event: &BuildEvent) -> Option<&EventId> {
    event.id.as_ref()?.id.as_ref()
}

fn decode_events(mut bytes: &[u8]) -> Vec<BuildEvent> {
    let mut events = Vec::new();
    while !bytes.is_empty() {
        events.push(BuildEvent::decode_length_delimited(&mut bytes).unwrap());
    }
    events
}

fn timestamp_millis(timestamp: &Timestamp) -> i64 {
    timestamp.seconds * 1_000 + i64::from(timestamp.nanos / 1_000_000)
}

fn assert_command_line_error(path: &std::path::Path) {
    let events = read_json_events(path);
    assert_eq!(events.len(), 4);
    assert!(events[3]["finished"]["overallSuccess"].is_null());
    assert_eq!(events[3]["finished"]["exitCode"]["code"], 2);
    assert_eq!(
        events[3]["finished"]["exitCode"]["name"],
        "COMMAND_LINE_ERROR"
    );
    assert_eq!(events[3]["lastMessage"], true);
}

fn read_json_events(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}
