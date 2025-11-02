use assert_cmd::Command;
use predicates::prelude::*; // Used for writing assertions

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
