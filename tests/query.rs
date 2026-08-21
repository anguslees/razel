use assert_cmd::Command;
use predicates::prelude::*;

fn basic_command() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    command.current_dir(format!("{}/examples/basic", env!("CARGO_MANIFEST_DIR")));
    command
}

fn inventory_command() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    command.current_dir(format!(
        "{}/tests/fixtures/query_inventory",
        env!("CARGO_MANIFEST_DIR")
    ));
    command
}

fn rule_types_command() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    command.current_dir(format!(
        "{}/tests/fixtures/rule_types",
        env!("CARGO_MANIFEST_DIR")
    ));
    command
}

fn recursive_prefix_command() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    command.current_dir(format!(
        "{}/tests/fixtures/recursive_prefix",
        env!("CARGO_MANIFEST_DIR")
    ));
    command
}

fn fixture_command(name: &str) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("razel"));
    command.current_dir(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ));
    command
}

#[test]
fn query_basic_exact_targets() {
    basic_command()
        .arg("query")
        .arg(":hello_world")
        .assert()
        .success()
        .stdout("//:hello_world\n");

    basic_command()
        .arg("query")
        .arg("//nested:hello_world_nested")
        .assert()
        .success()
        .stdout("//nested:hello_world_nested\n");

    basic_command()
        .arg("query")
        .arg("//:not_exist")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn query_basic_recursive_rules_remain_compatible() {
    basic_command()
        .arg("query")
        .arg("//...")
        .assert()
        .success()
        .stdout("//:hello_world\n//nested:hello_world_nested\n");
}

#[test]
fn query_label_kind_all_contains_rules_only() {
    inventory_command()
        .args(["query", "--output=label_kind", "//:all"])
        .assert()
        .success()
        .stdout(concat!(
            "filegroup rule //:condition\n",
            "genrule rule //:consume\n",
            "inventory_rule rule //:custom\n",
            "filegroup rule //:cycle_a\n",
            "filegroup rule //:cycle_b\n",
            "filegroup rule //:group\n",
        ));
}

#[test]
fn query_label_kind_all_targets_exposes_loaded_inventory() {
    inventory_command()
        .args(["query", "--output=label_kind", "//:*"])
        .assert()
        .success()
        .stdout(concat!(
            "source file //:BUILD.bazel\n",
            "filegroup rule //:condition\n",
            "genrule rule //:consume\n",
            "inventory_rule rule //:custom\n",
            "generated file //:custom.out\n",
            "filegroup rule //:cycle_a\n",
            "filegroup rule //:cycle_b\n",
            "source file //:exported.txt\n",
            "generated file //:final.out\n",
            "filegroup rule //:group\n",
            "source file //:missing_a.txt\n",
            "source file //:missing_b.txt\n",
            "source file //:missing_c.txt\n",
            "source file //:missing_default.txt\n",
            "source file //:private_tool\n",
            "source file //:scalar_condition\n",
        ));
}

#[test]
fn query_exact_source_and_generated_targets() {
    inventory_command()
        .args(["query", "--output=label_kind", "//:missing_a.txt"])
        .assert()
        .success()
        .stdout("source file //:missing_a.txt\n");

    inventory_command()
        .args(["query", "--output=label_kind", "//:custom.out"])
        .assert()
        .success()
        .stdout("generated file //:custom.out\n");
}

#[test]
fn query_recursive_membership_distinguishes_rules_and_all_targets() {
    inventory_command()
        .args(["query", "//..."])
        .assert()
        .success()
        .stdout(concat!(
            "//:condition\n",
            "//:consume\n",
            "//:custom\n",
            "//:cycle_a\n",
            "//:cycle_b\n",
            "//:group\n",
            "//nested:nested_group\n",
        ));

    inventory_command()
        .args(["query", "//...:*"])
        .assert()
        .success()
        .stdout(concat!(
            "//:BUILD.bazel\n",
            "//:condition\n",
            "//:consume\n",
            "//:custom\n",
            "//:custom.out\n",
            "//:cycle_a\n",
            "//:cycle_b\n",
            "//:exported.txt\n",
            "//:final.out\n",
            "//:group\n",
            "//:missing_a.txt\n",
            "//:missing_b.txt\n",
            "//:missing_c.txt\n",
            "//:missing_default.txt\n",
            "//:private_tool\n",
            "//:scalar_condition\n",
            "//nested:BUILD.bazel\n",
            "//nested:missing_nested.txt\n",
            "//nested:nested_group\n",
        ));
}

#[test]
fn query_all_targets_alias_matches_star() {
    let star = inventory_command()
        .args(["query", "//:*"])
        .output()
        .expect("query should execute");
    let alias = inventory_command()
        .args(["query", "//:all-targets"])
        .output()
        .expect("query should execute");
    assert!(star.status.success());
    assert!(alias.status.success());
    assert_eq!(star.stdout, alias.stdout);
}

#[test]
fn query_union_does_not_introduce_cross_operand_edges() {
    inventory_command()
        .args(["query", "//:group + //:custom.out"])
        .assert()
        .success()
        .stdout("//:group\n//:custom.out\n");
}

#[test]
fn query_union_deduplicates_targets() {
    inventory_command()
        .args(["query", "//:group + //:group"])
        .assert()
        .success()
        .stdout("//:group\n");
}

#[test]
fn query_loads_all_public_attribute_types() {
    rule_types_command()
        .args(["query", "--output=label_kind", "//:*"])
        .assert()
        .success()
        .stdout(concat!(
            "source file //:BUILD.bazel\n",
            "sample_binary rule //:binary\n",
            "source file //:missing_dep.txt\n",
            "source file //:missing_list_dep.txt\n",
            "sample_test rule //:test\n",
            "all_types_rule rule //:typed\n",
            "generated file //:typed.out\n",
            "generated file //:typed_a.out\n",
            "generated file //:typed_b.out\n",
        ));
}

#[test]
fn recursive_query_does_not_require_prefix_package() {
    recursive_prefix_command()
        .args(["query", "//foo/..."])
        .assert()
        .success()
        .stdout("//foo/bar:descendant\n");
}

#[test]
fn invalid_rule_definitions_and_missing_mandatory_attrs_fail_loading() {
    fixture_command("invalid_noncallable")
        .args(["query", "//:all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not callable"));

    fixture_command("invalid_mandatory")
        .args(["query", "//:all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "missing mandatory attribute `required`",
        ));

    fixture_command("invalid_output_select")
        .args(["query", "//:all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "attribute `out` is not configurable",
        ));

    fixture_command("recursive_missing_load")
        .args(["query", "//..."])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing.bzl"));

    fixture_command("invalid_defaults")
        .args(["query", "//:all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "non-default `provides` is not supported",
        ));

    fixture_command("invalid_test_name")
        .args(["query", "//:all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "test rule `bad_name` must have a name ending in `_test`",
        ));

    fixture_command("invalid_allow_empty")
        .args(["query", "//:all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "attribute default may not be empty",
        ));
}

#[test]
fn label_values_keep_defining_module_context_while_strings_use_build_context() {
    fixture_command("label_context")
        .args(["query", "--output=label_kind", "//:*"])
        .assert()
        .success()
        .stdout(concat!(
            "source file //:BUILD.bazel\n",
            "context_rule rule //:direct\n",
            "source file //:from_build.txt\n",
            "context_rule rule //:macro\n",
        ));
}
