use assert_cmd::Command;
use assert_fs::TempDir;
use predicates::prelude::*; // Used for writing assertions

#[cfg(bazel)]
mod bep;

#[test]
fn test_razel_version() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));

    cmd.arg("version");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));

    Ok(())
}

#[test]
fn test_razel_version_flag() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));

    cmd.arg("--version");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));

    Ok(())
}

#[test]
fn bep_file_creation_failure_uses_local_environmental_exit_code() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("missing").join("events.json");

    Command::new(assert_cmd::cargo::cargo_bin!("razel"))
        .arg("version")
        .arg(format!("--build_event_json_file={}", output.display()))
        .assert()
        .code(36)
        .stderr(predicate::str::contains("failed to create BEP output file"));
}
