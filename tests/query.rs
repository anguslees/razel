use assert_cmd::prelude::*; // Add methods on commands
use predicates::prelude::*; // Used for writing assertions
use std::process::Command;

#[test]
fn test_query_basic_hello_world() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("razel")?;
    cmd.current_dir("examples/basic");

    cmd.arg("query").arg("//:hello_world_txt");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("//:hello_world_txt"));

    Ok(())
}
