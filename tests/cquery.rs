use assert_cmd::Command;

fn command() -> Command {
    let binary = std::fs::canonicalize(assert_cmd::cargo::cargo_bin!("razel"))
        .expect("razel binary should exist");
    let mut command = Command::new(binary);
    command.current_dir(format!(
        "{}/tests/fixtures/configurable_attributes",
        env!("CARGO_MANIFEST_DIR")
    ));
    command
}

fn output(args: &[&str]) -> std::process::Output {
    static COMMAND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = COMMAND_LOCK.lock().unwrap();
    command().args(args).output().expect("razel should execute")
}

fn configured_labels(stdout: &[u8]) -> Vec<(&str, &str)> {
    std::str::from_utf8(stdout)
        .unwrap()
        .lines()
        .map(|line| {
            let (label, configuration) = line.rsplit_once(" (").unwrap();
            (label, configuration.strip_suffix(')').unwrap())
        })
        .collect()
}

#[test]
fn query_overapproximates_while_cquery_selects() {
    let query = output(&["query", "deps(//:selected)"]);
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    assert_eq!(
        std::str::from_utf8(&query.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "//:arm",
            "//:arm.txt",
            "//:common.txt",
            "//:feature.txt",
            "//:feature_on",
            "//:other.txt",
            "//:selected",
        ]
    );

    let cquery = output(&[
        "cquery",
        "--cpu=arm",
        "--define=feature=on",
        "deps(//:selected)",
    ]);
    assert!(
        cquery.status.success(),
        "{}",
        String::from_utf8_lossy(&cquery.stderr)
    );
    let labels = configured_labels(&cquery.stdout);
    assert_eq!(
        labels.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
        [
            "//:selected",
            "//:arm",
            "//:arm.txt",
            "//:common.txt",
            "//:feature.txt",
            "//:feature_on",
        ]
    );
    let configuration = |name| labels.iter().find(|(label, _)| *label == name).unwrap().1;
    assert_eq!(configuration("//:arm.txt"), "null");
    assert_ne!(configuration("//:selected"), "null");
    assert_eq!(configuration("//:selected"), configuration("//:arm"));
}

#[test]
fn specialization_and_equal_values_are_unambiguous() {
    let specialized = output(&["cquery", "--cpu=arm", "-c", "dbg", "deps(//:specialized)"]);
    assert!(specialized.status.success());
    let labels = configured_labels(&specialized.stdout);
    assert!(labels.iter().any(|(label, _)| *label == "//:arm_dbg.txt"));
    assert!(!labels.iter().any(|(label, _)| *label == "//:arm.txt"));

    let equal = output(&[
        "cquery",
        "--cpu=arm",
        "--force_pic",
        "deps(//:equal_matches)",
    ]);
    assert!(equal.status.success());
    let labels = configured_labels(&equal.stdout);
    assert_eq!(
        labels
            .iter()
            .filter(|(label, _)| *label == "//:same.txt")
            .count(),
        1
    );
}

#[test]
fn ambiguity_and_custom_no_match_errors_fail_analysis() {
    let ambiguous = output(&["cquery", "--cpu=arm", "--define=feature=on", "//:ambiguous"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));

    let no_match = output(&["cquery", "--cpu=arm", "//:no_match"]);
    assert!(!no_match.status.success());
    assert!(String::from_utf8_lossy(&no_match.stderr).contains("choose a supported CPU"));

    let transitive = output(&["cquery", "--cpu=arm", "//:transitive_no_match"]);
    assert!(!transitive.status.success());
    assert!(String::from_utf8_lossy(&transitive.stderr).contains("choose a supported CPU"));
}

#[test]
fn platform_constraints_select_direct_and_config_setting_conditions() {
    let result = output(&[
        "cquery",
        "--platforms=//:red_platform",
        "deps(//:constraint_selected)",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let labels = configured_labels(&result.stdout);
    assert!(
        labels
            .iter()
            .any(|(label, _)| *label == "//:direct_red.txt")
    );
    assert!(
        labels
            .iter()
            .any(|(label, _)| *label == "//:setting_red.txt")
    );
    assert!(
        labels
            .iter()
            .any(|(label, _)| *label == "//:platform_red.txt")
    );
    assert!(!labels.iter().any(|(label, _)| *label == "//:not_red.txt"));
}

#[test]
fn platform_options_resolve_exact_labels_not_patterns() {
    for platform in ["//:all", "//:all-targets"] {
        let result = output(&[
            "cquery",
            &format!("--platforms={platform}"),
            "deps(//:constraint_selected)",
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            configured_labels(&result.stdout)
                .iter()
                .any(|(label, _)| *label == "//:direct_red.txt")
        );
    }
}

#[test]
fn generated_targets_keep_configuration_and_tools_use_exec_configuration() {
    let generated = output(&["cquery", "--cpu=arm", "//:generated.txt"]);
    assert!(generated.status.success());
    let labels = configured_labels(&generated.stdout);
    assert_eq!(labels[0].0, "//:generated.txt");
    assert_ne!(labels[0].1, "null");

    let result = output(&[
        "cquery",
        "--cpu=arm",
        "--host_cpu=x86",
        "deps(//:generated)",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let labels = configured_labels(&result.stdout);
    let tool = labels
        .iter()
        .find(|(label, _)| *label == "//:tool")
        .unwrap();
    let condition = labels
        .iter()
        .find(|(label, _)| *label == "//:host_x86")
        .unwrap();
    assert_ne!(tool.1, labels[0].1);
    assert_eq!(condition.1, tool.1);
    assert!(labels.iter().any(|(label, _)| *label == "//:host.txt"));
    assert!(!labels.iter().any(|(label, _)| *label == "//:target.txt"));

    let default_host = output(&["cquery", "--cpu=arm", "deps(//:generated)"]);
    assert!(default_host.status.success());
    let labels = configured_labels(&default_host.stdout);
    assert!(labels.iter().any(|(label, _)| *label == "//:target.txt"));
    assert!(!labels.iter().any(|(label, _)| *label == "//:host.txt"));
}

#[test]
fn maximal_equal_conditions_and_normalized_defines_specialize() {
    for target in ["//:maximal_equal_matches", "//:define_specialization"] {
        let result = output(&[
            "cquery",
            "--cpu=arm",
            "-c",
            "dbg",
            "--define=feature=on",
            &format!("deps({target})"),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let labels = configured_labels(&result.stdout);
        assert!(
            labels
                .iter()
                .any(|(label, _)| *label == "//:same.txt" || *label == "//:feature.txt")
        );
        assert!(!labels.iter().any(|(label, _)| *label == "//:other.txt"));
    }
}

#[test]
fn configured_universe_and_checksum_filter_include_exec_targets() {
    let intersection = output(&[
        "cquery",
        "--cpu=arm",
        "--host_cpu=x86",
        "deps(//:generated) intersect //:tool",
    ]);
    assert!(
        intersection.status.success(),
        "{}",
        String::from_utf8_lossy(&intersection.stderr)
    );
    let labels = configured_labels(&intersection.stdout);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].0, "//:tool");

    let reversed = output(&[
        "cquery",
        "--cpu=arm",
        "--host_cpu=x86",
        "//:tool intersect deps(//:generated)",
    ]);
    assert!(
        reversed.status.success(),
        "{}",
        String::from_utf8_lossy(&reversed.stderr)
    );
    assert_eq!(configured_labels(&reversed.stdout), labels);

    let difference = output(&[
        "cquery",
        "--cpu=arm",
        "--host_cpu=x86",
        "//:tool except deps(//:generated)",
    ]);
    assert!(difference.status.success());
    let target_labels = configured_labels(&difference.stdout);
    assert_eq!(target_labels.len(), 1);
    assert_eq!(target_labels[0].0, "//:tool");
    assert_ne!(target_labels[0].1, labels[0].1);

    let union = output(&[
        "cquery",
        "--cpu=arm",
        "--host_cpu=x86",
        "//:tool + deps(//:generated)",
    ]);
    assert!(union.status.success());
    let tools = configured_labels(&union.stdout)
        .into_iter()
        .filter(|(label, _)| *label == "//:tool")
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    assert!(tools.contains(&labels[0]));
    assert!(tools.contains(&target_labels[0]));

    let prefix = &labels[0].1[..4];
    let expression = format!("config(deps(//:generated), {prefix}) intersect //:tool");
    let by_checksum = output(&["cquery", "--cpu=arm", "--host_cpu=x86", &expression]);
    assert!(
        by_checksum.status.success(),
        "{}",
        String::from_utf8_lossy(&by_checksum.stderr)
    );
    assert_eq!(configured_labels(&by_checksum.stdout)[0].0, "//:tool");

    let remapping = format!("config(config(//:tool, target), {prefix}) + //:generated");
    let result = output(&["cquery", "--cpu=arm", "--host_cpu=x86", &remapping]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let remapped = configured_labels(&result.stdout);
    assert_eq!(remapped.len(), 2);
    assert!(remapped.contains(&labels[0]));
    assert!(remapped.iter().any(|(label, _)| *label == "//:generated"));

    let absent_variant = format!("config(//:selected, {}) + //:generated", labels[0].1);
    let result = output(&["cquery", "--cpu=arm", "--host_cpu=x86", &absent_variant]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("no targets found in configuration"));

    let absent_configuration = format!("config(//:tool, {})", labels[0].1);
    let result = output(&[
        "cquery",
        "--cpu=arm",
        "--host_cpu=x86",
        &absent_configuration,
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown configuration"));

    let literal_exec = output(&["cquery", "config(deps(//:generated), exec)"]);
    assert!(!literal_exec.status.success());
    assert!(String::from_utf8_lossy(&literal_exec.stderr).contains("unknown configuration"));
}

#[test]
fn bounded_deps_measures_depth_from_the_nearest_root() {
    for command in [&["query"][..], &["query", "--order_output=no"], &["cquery"]] {
        for (expression, expected) in [
            ("deps(//:generated.txt + //:generated, 1)", "//:tool"),
            ("deps(//:generated + //:generated.txt, 1)", "//:tool"),
            ("deps(//:diamond_root + //:shared, 2)", "//:common.txt"),
            ("deps(//:shared + //:diamond_root, 2)", "//:common.txt"),
        ] {
            let result = output(&[command, &[expression]].concat());
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let stdout = std::str::from_utf8(&result.stdout).unwrap();
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.starts_with(expected))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn query_sorts_by_label_while_unordered_deps_stream_breadth_first() {
    let labels = |args: &[&str]| {
        let result = output(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::str::from_utf8(&result.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                line.split_once(" (")
                    .map_or(line, |(label, _)| label)
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    // Like Bazel's default `--order_output=auto`.
    assert_eq!(
        labels(&["query", "deps(//:diamond_root)"]),
        [
            "//:common.txt",
            "//:diamond_a",
            "//:diamond_b",
            "//:diamond_root",
            "//:shared",
        ]
    );
    // Each level of dependencies follows the previous one, as in Bazel's cquery.
    for args in [
        &["query", "--order_output=no", "deps(//:diamond_root)"][..],
        &["cquery", "deps(//:diamond_root)"],
    ] {
        assert_eq!(
            labels(args),
            [
                "//:diamond_root",
                "//:diamond_a",
                "//:diamond_b",
                "//:shared",
                "//:common.txt",
            ]
        );
    }

    // Loading-phase cycles are legal.
    assert_eq!(
        labels(&["query", "deps(//:cycle_a)"]),
        ["//:cycle_a", "//:cycle_b"]
    );
}

#[test]
fn cquery_checks_the_whole_universe_even_when_results_are_filtered() {
    for expression in [
        "deps(//:cycle_a)",
        "//:cycle_a",
        "deps(//:cycle_a, 0)",
        "//:selected except //:cycle_a",
        "//:cycle_a intersect //:tool",
        "let unused = //:cycle_a in //:selected",
    ] {
        let result = output(&["cquery", expression]);
        assert!(!result.status.success(), "{expression}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("cycle"));
    }
}

#[test]
fn cquery_streams_results_before_late_analysis_errors() {
    for (expression, expected, message) in [
        (
            "//:selected + //:no_match",
            "//:selected",
            "choose a supported CPU",
        ),
        (
            "//:transitive_no_match",
            "//:transitive_no_match",
            "choose a supported CPU",
        ),
        (
            "//:selected + //invalid_duplicates:duplicate_selected_label",
            "//:selected",
            "duplicate label",
        ),
    ] {
        let result = output(&["cquery", expression]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(message));
        let labels = configured_labels(&result.stdout);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].0, expected);
    }
}

#[test]
fn let_bindings_replay_in_their_definition_environment() {
    for expression in [
        "let x = //:selected in $x + $x",
        "let x = //:selected in $x intersect $x",
        "let x = //:selected in let y = $x in let x = //:tool in $y",
        "let x = //:selected in let x = $x in $x",
    ] {
        let result = output(&["cquery", expression]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let labels = configured_labels(&result.stdout);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].0, "//:selected");
    }
    let undefined = output(&["cquery", "let x = $y in let y = //:selected in $x"]);
    assert!(!undefined.status.success());
    assert!(String::from_utf8_lossy(&undefined.stderr).contains("Undefined variable y"));
}

#[test]
fn nested_let_bindings_are_evaluated_once() {
    // Re-evaluating every reference would make the final variable cost 2^LEVELS evaluations.
    const LEVELS: usize = 32;
    let mut expression = String::from("let v0 = //:selected in ");
    for level in 1..=LEVELS {
        let previous = level - 1;
        expression.push_str(&format!("let v{level} = $v{previous} + $v{previous} in "));
    }
    expression.push_str(&format!("$v{LEVELS}"));
    let result = command()
        .args(["cquery", &expression])
        .timeout(std::time::Duration::from_secs(60))
        .output()
        .expect("razel should finish before the timeout");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let labels = configured_labels(&result.stdout);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].0, "//:selected");
}

#[test]
fn exact_patterns_must_name_existing_targets() {
    let error = "no such target '//:does_not_exist': target 'does_not_exist' not declared in \
                 package ''";
    for expression in [
        "//:does_not_exist",
        "//:selected + //:does_not_exist",
        "//:selected - //:does_not_exist",
        "//:selected ^ //:does_not_exist",
        "deps(//:does_not_exist)",
        "config(//:does_not_exist, target)",
        "let unused = //:does_not_exist in //:selected",
    ] {
        let result = output(&["cquery", expression]);
        assert_eq!(result.status.code(), Some(7), "{expression}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(error), "{expression}: {stderr}");
    }
}

#[test]
fn none_select_branches_leave_attributes_unset() {
    // An optional label without a default is unset, while its select() keys stay dependencies.
    for (args, selected) in [(&[][..], None), (&["--cpu=arm"][..], Some("//:arm.txt"))] {
        let result = output(&[&["cquery"], args, &["deps(//:optional_label_unset)"]].concat());
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let labels = configured_labels(&result.stdout);
        let has = |name| labels.iter().any(|(label, _)| *label == name);
        assert!(has("//:arm") && has("//:common.txt"));
        assert_eq!(has("//:arm.txt"), selected.is_some());
    }

    // As in Bazel, a mandatory attribute may not resolve to None.
    let result = output(&["cquery", "//:mandatory_label_unset"]);
    assert_eq!(result.status.code(), Some(7));
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("mandatory attribute `required` resolved to None after evaluating select()")
    );
    let result = output(&["cquery", "--cpu=arm", "deps(//:mandatory_label_unset)"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn starlark_rules_with_builtin_names_keep_starlark_semantics() {
    for target in [
        "//:starlark_alias_condition",
        "//:starlark_constraint_condition",
    ] {
        let result = output(&["cquery", target]);
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("is not a config_setting or constraint_value"),
            "{stderr}"
        );
    }

    let result = output(&[
        "cquery",
        "//:starlark_named_constraint + //:selected + //:red",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let labels = configured_labels(&result.stdout);
    let configuration = |name| labels.iter().find(|(label, _)| *label == name).unwrap().1;
    assert_eq!(
        configuration("//:starlark_named_constraint"),
        configuration("//:selected")
    );
    assert_ne!(configuration("//:red"), configuration("//:selected"));
}

#[test]
fn set_operators_have_equal_left_associative_precedence() {
    let result = output(&["cquery", "//:selected + //:tool ^ //:tool"]);
    assert!(result.status.success());
    let labels = configured_labels(&result.stdout);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].0, "//:tool");
}

#[test]
fn aliases_forward_configuration_providers() {
    let condition = output(&["cquery", "--cpu=arm", "deps(//:alias_condition)"]);
    assert!(condition.status.success());
    let labels = configured_labels(&condition.stdout);
    assert!(labels.iter().any(|(label, _)| *label == "//:arm.txt"));
    assert!(!labels.iter().any(|(label, _)| *label == "//:other.txt"));

    let constraint = output(&[
        "cquery",
        "--platforms=//:alias_red_platform",
        "deps(//:constraint_selected)",
    ]);
    assert!(constraint.status.success());
    assert!(
        configured_labels(&constraint.stdout)
            .iter()
            .any(|(label, _)| *label == "//:direct_red.txt")
    );

    let platform_alias = output(&[
        "cquery",
        "--platforms=//:red_platform_alias",
        "deps(//:constraint_selected)",
    ]);
    assert!(platform_alias.status.success());
    assert!(
        configured_labels(&platform_alias.stdout)
            .iter()
            .any(|(label, _)| *label == "//:direct_red.txt")
    );
}

#[test]
fn constraint_targets_use_exec_configuration() {
    let result = output(&[
        "cquery",
        "--platforms=//:red_platform",
        "deps(//:constraint_selected)",
    ]);
    assert!(result.status.success());
    let labels = configured_labels(&result.stdout);
    let root = labels
        .iter()
        .find(|(label, _)| *label == "//:constraint_selected")
        .unwrap();
    let value = labels.iter().find(|(label, _)| *label == "//:red").unwrap();
    let setting = labels
        .iter()
        .find(|(label, _)| *label == "//:color")
        .unwrap();
    assert_ne!(value.1, root.1);
    assert_eq!(value.1, setting.1);
}

#[test]
fn unsupported_and_invalid_configuration_rules_fail_explicitly() {
    let flag = output(&["cquery", "//:unsupported_flag_value"]);
    assert!(!flag.status.success());
    assert!(String::from_utf8_lossy(&flag.stderr).contains("flag_values"));

    let duplicate = output(&[
        "cquery",
        "--platforms=//:invalid_color_platform",
        "//:selected",
    ]);
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("multiple values"));

    for (args, message) in [
        (
            vec![
                "cquery",
                "--platforms=//:unsupported_flags_platform",
                "//:selected",
            ],
            "platform flags",
        ),
        (vec!["cquery", "//:empty_setting"], "at least one condition"),
        (
            vec!["cquery", "//:invalid_constraint_setting"],
            "multiple values",
        ),
        (
            vec!["cquery", "//:invalid_color_platform"],
            "multiple values",
        ),
        (
            vec!["cquery", "//:unsupported_refinement"],
            "constraint refinement",
        ),
        (vec!["cquery", "//:condition_alias_cycle"], "alias cycle"),
        (
            vec![
                "cquery",
                "--cpu=arm",
                "//invalid_duplicates:duplicate_selected_label",
            ],
            "duplicate label",
        ),
    ] {
        let result = output(&args);
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn cli_modes_and_boolean_spellings_are_validated() {
    let short_bool = output(&["cquery", "--force_pic", "deps(//:short_bool_spelling)"]);
    assert!(short_bool.status.success());
    assert!(
        configured_labels(&short_bool.stdout)
            .iter()
            .any(|(label, _)| *label == "//:same.txt")
    );

    let invalid_mode = output(&["cquery", "-c", "nonsense", "//:selected"]);
    assert!(!invalid_mode.status.success());
    assert!(String::from_utf8_lossy(&invalid_mode.stderr).contains("invalid value"));

    let false_assignment = output(&[
        "cquery",
        "--force_pic=false",
        "deps(//:short_bool_spelling)",
    ]);
    assert!(false_assignment.status.success());
    assert!(
        !configured_labels(&false_assignment.stdout)
            .iter()
            .any(|(label, _)| *label == "//:same.txt")
    );
}

#[test]
fn target_compatibility_uses_the_selected_platform() {
    let compatible = output(&[
        "cquery",
        "--platforms=//:red_platform",
        "deps(//:red_compatible)",
    ]);
    assert!(compatible.status.success());
    assert!(
        configured_labels(&compatible.stdout)
            .iter()
            .any(|(label, _)| *label == "//:common.txt")
    );

    // cquery reports incompatible targets instead of failing; only builds reject them.
    let mixed = output(&[
        "cquery",
        "--platforms=//:red_platform",
        "//:red_compatible + //:blue_incompatible + //:blue_dependent",
    ]);
    assert!(
        mixed.status.success(),
        "{}",
        String::from_utf8_lossy(&mixed.stderr)
    );
    let labels = configured_labels(&mixed.stdout);
    for expected in [
        "//:red_compatible",
        "//:blue_incompatible",
        "//:blue_dependent",
    ] {
        assert!(labels.iter().any(|(label, _)| *label == expected));
    }

    // A directly incompatible target keeps only its select() keys and compatibility
    // constraints as dependencies; its other attributes are never resolved.
    for (target, select_key) in [
        ("//:blue_incompatible", None),
        ("//:blue_incompatible_unresolved", Some("//:never")),
    ] {
        let result = output(&[
            "cquery",
            "--platforms=//:red_platform",
            &format!("deps({target})"),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let labels = configured_labels(&result.stdout);
        let has = |name| labels.iter().any(|(label, _)| *label == name);
        assert!(has(target) && has("//:blue") && has("//:color"));
        assert!(!has("//:common.txt") && !has("//:never.txt"));
        assert_eq!(select_key.is_some_and(has), select_key.is_some());
    }

    // Indirectly incompatible targets are still configured and traversed.
    let dependent = output(&[
        "cquery",
        "--platforms=//:red_platform",
        "deps(//:blue_dependent)",
    ]);
    assert!(dependent.status.success());
    let labels = configured_labels(&dependent.stdout);
    assert!(
        labels
            .iter()
            .any(|(label, _)| *label == "//:blue_incompatible")
    );
    assert!(!labels.iter().any(|(label, _)| *label == "//:common.txt"));

    // Without an explicit target platform compatibility is not checked.
    let unchecked = output(&["cquery", "deps(//:blue_incompatible)"]);
    assert!(unchecked.status.success());
    assert!(
        configured_labels(&unchecked.stdout)
            .iter()
            .any(|(label, _)| *label == "//:common.txt")
    );
    let unresolved = output(&["cquery", "//:blue_incompatible_unresolved"]);
    assert!(!unresolved.status.success());
    assert!(
        String::from_utf8_lossy(&unresolved.stderr)
            .contains("configurable attribute `srcs` doesn't match")
    );
}

#[test]
fn config_function_finds_target_and_null_configurations() {
    let result = output(&[
        "cquery",
        "config(//:selected, target) + config(//:common.txt, null)",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let labels = configured_labels(&result.stdout);
    assert_eq!(labels[0].0, "//:selected");
    assert_eq!(labels[1], ("//:common.txt", "null"));
}

#[test]
fn aliases_query_files_and_multi_token_expressions_are_supported() {
    let alias = output(&["cquery", "--cpu=arm", "deps(//:selected_alias)"]);
    assert!(alias.status.success());
    let labels = configured_labels(&alias.stdout);
    assert!(labels.iter().any(|(label, _)| *label == "//:selected"));
    assert!(!labels.iter().any(|(label, _)| *label == "//:tool"));

    let query_file = output(&["cquery", "--cpu=arm", "--query_file=selection.cquery"]);
    assert!(query_file.status.success());
    assert!(
        configured_labels(&query_file.stdout)
            .iter()
            .any(|(label, _)| *label == "//:arm.txt")
    );

    let multi_token = output(&["cquery", "//:selected", "+", "//:tool"]);
    assert!(multi_token.status.success());
    let labels = configured_labels(&multi_token.stdout);
    assert_eq!(
        labels.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
        ["//:selected", "//:tool"]
    );
}
