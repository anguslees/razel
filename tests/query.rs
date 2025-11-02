use assert_cmd::Command;
use predicates::prelude::*; // Used for writing assertions

#[test]
fn test_query_basic_hello_world() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    cmd.current_dir("examples/basic");

    cmd.arg("query").arg("//:hello_world_txt");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("//:hello_world_txt"));

    Ok(())
}
