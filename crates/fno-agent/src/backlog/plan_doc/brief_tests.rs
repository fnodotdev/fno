use super::*;
use crate::plan_doc::doc::{load_plan_text, PlanDocument};

const SAMPLE_PLAN: &str = r####"---
claims: ab-test-001
created: 2026-05-18
status: ready
type: think
feature: sample plan for brief generation tests
---

# Sample Plan

## Overview

This is the first paragraph of the overview section. It gives workers project context.

Second paragraph with more detail.

## Failure Modes

**Boundaries**
- The system must reject missing plan files with exit code 1
- The system must reject unknown task-ids with exit code 2

**Errors**
- The system must surface YAML parse failures with exit code 3

## Acceptance Criteria

**AC2-HP:** Given a plan with task 2.1, when I call brief, then I get a markdown brief.

**AC2-ERR:** Given an unknown task-id, when I call brief, then exit code is 2.

**AC2-UI:** With --format json, output matches the fixed schema.

**AC2-EDGE:** Given no tagged entries, all Locked Decisions are included.

**AC2-FR:** Given malformed Execution Strategy YAML, exit code is 3.

## Locked Decisions (DO NOT revisit)

1. **Use stdlib json.** Use the stdlib json module. *Why:* zero extra dependency. *How to apply:* import json.

2. **Fail-open on untagged entries.** Entries without surface tags are included. *Why:* avoids silent brief shrinkage. *How to apply:* include the entry.

## Patterns to Reuse

| Pattern | Source | Why reuse |
|---|---|---|
| `resolve_repo_root()` | fno.paths | path anchoring from any cwd |
| TyperRunner | cli/src/fno/plan/brief.py | consistent CliRunner pattern |

## Execution Strategy

> Bootstrapped by hand for testing.

```yaml
execution_mode: mixed
waves:
  - wave: 1
    mode: sequential
    name: "Foundation"
    tasks: [1.1]
  - wave: 2
    mode: sequential
    name: "Surface"
    tasks: [2.1]

tasks:
  - id: "1.1"
    title: "Foundation module"
    surface:
      - cli/src/fno/sample/core.py
      - cli/tests/unit/sample/test_core.py
    verify: "uv run pytest cli/tests/unit/sample/test_core.py -v"
    acceptance: [AC2-HP]
    notes: |
      Build the core module.

  - id: "2.1"
    title: "CLI entry point"
    surface:
      - cli/src/fno/plan/brief.py
      - cli/tests/unit/plan/test_brief.py
    verify: "uv run pytest cli/tests/unit/plan/test_brief.py -v"
    acceptance: [AC2-HP, AC2-ERR, AC2-UI, AC2-EDGE, AC2-FR]
    notes: |
      Build the CLI command for brief generation.
```
"####;

fn sample() -> PlanDocument {
    load_plan_text(SAMPLE_PLAN).unwrap()
}

#[test]
fn task_edges_refuse_unknown_ids_and_cycles_through_inherited_wave_blockers() {
    let unknown = json!({"tasks": [
        {"id": "1.1", "blocked_by": ["9.9"], "blocked_by_declared": true},
    ]});
    assert_eq!(
        validate_task_edges(&unknown),
        ["task 1.1 blocked_by unknown task 9.9"]
    );

    // 2.1 declares nothing, so it inherits wave 1 (1.1) while 1.1 waits on 2.1.
    let cyclic = json!({
        "tasks": [
            {"id": "1.1", "blocked_by": ["2.1"], "blocked_by_declared": true},
            {"id": "2.1", "blocked_by": [], "blocked_by_declared": false},
        ],
        "waves": [
            {"wave": 1, "tasks": ["1.1"]},
            {"wave": 2, "tasks": ["2.1"]},
        ],
    });
    let errors = validate_task_edges(&cyclic);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("cycle"), "{errors:?}");
    assert!(errors[0].contains("1.1") && errors[0].contains("2.1"));
}

#[test]
fn a_brief_carries_the_task_spec_scoped_acs_surface_files_and_matching_patterns() {
    let doc = sample();
    let brief = build_brief(&doc, "2.1", "relevant", "relevant")
        .ok()
        .unwrap();
    assert_eq!(brief.task_spec["id"], json!("2.1"));
    assert_eq!(brief.task_spec["title"], json!("CLI entry point"));
    assert_eq!(
        brief.verify_command,
        "uv run pytest cli/tests/unit/plan/test_brief.py -v"
    );
    assert!(brief
        .project_context
        .starts_with("This is the first paragraph"));
    assert_eq!(brief.files.len(), 2);
    assert_eq!(brief.acceptance_criteria.len(), 5);
    assert_eq!(brief.locked_decisions.len(), 2);
    let sources: Vec<&str> = brief
        .patterns
        .iter()
        .map(|p| p["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["cli/src/fno/plan/brief.py"]);

    // Task 1.1 asks for AC2-HP only; the other four codes are tagged for task 2.1.
    let first = build_brief(&doc, "1.1", "relevant", "relevant")
        .ok()
        .unwrap();
    let codes: Vec<&str> = first
        .acceptance_criteria
        .iter()
        .map(|a| a["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["AC2-HP"]);

    let quiet = build_brief(&doc, "2.1", "none", "all").ok().unwrap();
    assert!(quiet.failure_modes.is_empty());
}

#[test]
fn a_brief_renders_the_fixed_json_schema_and_the_markdown_sections() {
    let brief = build_brief(&sample(), "2.1", "relevant", "relevant")
        .ok()
        .unwrap();

    let value = brief.to_json();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "acceptance_criteria",
            "failure_modes",
            "files",
            "locked_decisions",
            "patterns",
            "project_context",
            "task_spec",
            "verify_command",
        ]
    );
    let mut spec_keys: Vec<&str> = value["task_spec"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    spec_keys.sort();
    assert_eq!(
        spec_keys,
        ["acceptance", "id", "notes", "surface", "title", "verify"]
    );

    let md = brief.to_markdown();
    assert!(md.starts_with("## Project Context\n"), "{md}");
    assert!(md.contains("## Task 2.1: CLI entry point"));
    assert!(md.contains("- `cli/src/fno/plan/brief.py`"));
    assert!(md.contains("**AC2-HP:** "));
    assert!(md.contains("1. **Use stdlib json**"));
    assert!(md.contains("   *How to apply:* import json"));
    assert!(md.contains("**Boundaries**"));
    assert!(md.contains("### Verify Command\n\n```\nuv run pytest"));
}
