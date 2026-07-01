use assert_cmd::Command;
use predicates::prelude::*; // Used for writing assertions

#[test]
fn test_query_basic_hello_world() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    cmd.current_dir("examples/basic");

    cmd.arg("query").arg(":hello_world");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("//:hello_world"));

    Ok(())
}

#[test]
fn test_query_basic_nested_hello_world() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    cmd.current_dir("examples/basic");

    cmd.arg("query").arg("//nested:hello_world_nested");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("//nested:hello_world"));

    Ok(())
}

#[test]
fn test_query_basic_notfound() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    cmd.current_dir("examples/basic");

    cmd.arg("query").arg("//:not_exist");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("//:hello_world").not());

    Ok(())
}

#[test]
fn test_query_dots() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    cmd.current_dir("examples/basic");

    cmd.arg("query").arg("//...");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("//:hello_world"));

    Ok(())
}
