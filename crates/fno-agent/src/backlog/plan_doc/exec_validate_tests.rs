use super::*;
use crate::plan_doc::doc::load_plan_text;
use std::collections::BTreeSet;

const FULL_FM: &str = "status: ready\nkind: plan\ncreated: 2026-07-25\n";
const QUICK_FM: &str = "status: ready\nkind: quick-plan\ncreated: 2026-07-25\n";

const VALID_STRATEGY: &str = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    name: Build\n    tasks: [\"1.1\"]\ntasks:\n  - id: \"1.1\"\n    title: Build the validator\n    surface: [cli/src/plan/cli.py]\n    verify: fno doctor test tests/test_a.py\n    acceptance: [AC1-HP]\n";

fn doc_of(frontmatter: &str, body: &str) -> PlanDocument {
    load_plan_text(&format!("---\n{frontmatter}---\n\n# Plan\n\n{body}")).unwrap()
}

/// The fixture states the kind its body actually is: only a full plan opens
/// with an Overview.
fn validate_body(body: &str) -> ExecutionValidationResult {
    let fm = if body.contains("## Overview") {
        FULL_FM
    } else {
        QUICK_FM
    };
    validate_execution(&doc_of(fm, body), None)
}

fn full_plan(strategy: &str) -> String {
    format!("## Overview\n\nExecutable plan.\n\n## Execution Strategy\n\n```yaml\n{strategy}```\n")
}

fn quick_plan(files: &str, verification: &str) -> String {
    format!("## Context\n\nWhy this is needed.\n\n## Changes\n\n### 1. Do the work\n\nMake the change.\n\n## Files to Modify\n\n{files}\n\n## Verification\n\n{verification}\n")
}

const QUICK_FILES: &str = "| File | Action |\n|---|---|\n| `cli/src/plan/exec.py` | modify |";

fn strategy_with_verify(verify: &str) -> String {
    VALID_STRATEGY.replace(
        "verify: fno doctor test tests/test_a.py",
        &format!("verify: {}", serde_json::to_string(verify).unwrap()),
    )
}

fn fields(result: &ExecutionValidationResult) -> BTreeSet<String> {
    result.violations.iter().map(|v| v.field.clone()).collect()
}

fn messages(result: &ExecutionValidationResult) -> String {
    result
        .violations
        .iter()
        .map(|v| v.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn prose_and_non_commands_fail_verify_in_both_plan_shapes() {
    for verification in [
        "Manually inspect the result",
        "make sure",
        "timeout 30",
        "README.md",
        "pytest tests/ && echo \"unclosed",
    ] {
        let quick = validate_body(&quick_plan(QUICK_FILES, &format!("1. {verification}")));
        assert_eq!(
            fields(&quick),
            BTreeSet::from(["Verification".to_string()]),
            "{verification}"
        );
        let full = validate_body(&full_plan(&strategy_with_verify(verification)));
        assert_eq!(
            fields(&full),
            BTreeSet::from(["tasks.1.1.verify".to_string()]),
            "{verification}"
        );
    }
}

#[test]
fn task_and_wave_structure_errors_are_each_reported() {
    let no_tasks = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    tasks: [\"1.1\"]\n";
    assert!(messages(&validate_body(&full_plan(no_tasks)))
        .contains("Execution Strategy must declare at least one task"));

    let duplicate = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    tasks: [\"1.1\", \"9.9\"]\ntasks:\n  - id: \"1.1\"\n    title: First\n    surface: [src/a.py]\n    verify: fno doctor test tests/test_a.py\n    acceptance: [AC1]\n  - id: \"1.1\"\n    title: Duplicate\n    surface: [src/b.py]\n    verify: fno doctor test tests/test_b.py\n    acceptance: [AC2]\n";
    let text = messages(&validate_body(&full_plan(duplicate)));
    assert!(text.contains("duplicate task id"), "{text}");
    assert!(text.contains("unknown task '9.9'"), "{text}");

    let cycle = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    depends_on: 2\n    tasks: [\"1.1\"]\n  - wave: 2\n    mode: sequential\n    depends_on: 1\n    tasks: [\"2.1\"]\ntasks:\n  - id: \"1.1\"\n    title: First\n    surface: [src/a.py]\n    verify: fno doctor test tests/test_a.py\n    acceptance: [AC1]\n  - id: \"2.1\"\n    title: Second\n    surface: [src/b.py]\n    verify: fno doctor test tests/test_b.py\n    acceptance: [AC2]\n";
    assert!(messages(&validate_body(&full_plan(cycle))).contains("dependency cycle"));

    let bad_wave = VALID_STRATEGY.replace("wave: 1", "wave: nope").replace(
        "mode: sequential\n    name: Build",
        "mode: mixed\n    name: Build",
    );
    let got = fields(&validate_body(&full_plan(&bad_wave)));
    assert!(got.contains("waves.nope.wave"), "{got:?}");
    assert!(got.contains("waves.nope.mode"), "{got:?}");

    let dup_key = format!("{VALID_STRATEGY}tasks: []\n");
    let text = messages(&validate_body(&full_plan(&dup_key)));
    assert!(
        text.contains("duplicate") && text.contains("tasks"),
        "{text}"
    );
}
