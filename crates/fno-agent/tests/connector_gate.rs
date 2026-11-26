//! The connector gate's contract, driven through the binary: a clean
//! synthetic CA SOS-shaped fixture passes, a missing sampled key and a
//! malformed rows line are refused with distinct exit codes, and every
//! seeded defect class is rejected.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connector_gate")
        .join(name)
}

fn gate(rows: &Path, extra: &[&str]) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agent"))
        .args(["evals", "connector-gate", "--contract"])
        .arg(fixture("contract.toml"))
        .arg("--rows")
        .arg(rows)
        .arg("--sample")
        .arg(fixture("sample.jsonl"))
        .arg("--json")
        .args(extra)
        .envs(fno_agent::test_run::self_owner_env())
        .output()
        .expect("run fno-agent evals connector-gate");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let receipt = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (
        out.status.code().unwrap_or(-1),
        receipt,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn rows_with(edit: impl FnOnce(&mut Vec<String>)) -> (tempfile::TempDir, PathBuf) {
    let text = std::fs::read_to_string(fixture("rows.jsonl")).expect("read rows");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    edit(&mut lines);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rows.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").expect("write rows");
    (dir, path)
}

#[test]
fn clean_fixture_passes_with_every_field_exact() {
    let (code, receipt, stderr) = gate(&fixture("rows.jsonl"), &["--source-count", "300"]);
    assert_eq!(code, 0, "stderr: {stderr}\nreceipt: {receipt:#}");
    let base = &receipt["baseline"];
    assert_eq!(base["verdict"], "pass");
    let fields = base["value"]["fields"].as_array().expect("fields");
    assert_eq!(fields.len(), 6);
    for f in fields {
        assert_eq!(f["accuracy"], 1.0, "field {f}");
        assert_eq!(f["n"], 30, "field {f}");
    }
}

#[test]
fn a_sampled_key_with_no_row_fails_and_is_named() {
    let (_dir, rows) = rows_with(|lines| {
        lines.retain(|l| !l.contains("\"entity_number\": \"S0000002\""));
    });
    let (code, receipt, stderr) = gate(&rows, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    let missing = &receipt["baseline"]["completeness"]["missing_keys"];
    assert_eq!(missing, &serde_json::json!(["S0000002"]));
}

#[test]
fn a_malformed_rows_line_exits_two_naming_the_line() {
    let (_dir, rows) = rows_with(|lines| lines[6] = "{not json".to_string());
    let (code, _, stderr) = gate(&rows, &[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("line 7"), "stderr: {stderr}");
}

#[test]
fn every_seeded_defect_class_is_rejected() {
    let (code, receipt, stderr) = gate(
        &fixture("rows.jsonl"),
        &["--source-count", "300", "--mutate", "all"],
    );
    assert_eq!(code, 0, "stderr: {stderr}\nreceipt: {receipt:#}");
    assert_eq!(receipt["baseline"]["verdict"], "pass");
    let muts = receipt["mutations"].as_array().expect("mutations");
    let classes: Vec<&str> = muts.iter().filter_map(|m| m["class"].as_str()).collect();
    assert_eq!(classes, ["wrong-field", "dropped-page", "stale-session"]);
    for m in muts {
        assert_eq!(m["rejected"], true, "mutation {m}");
    }
    let dropped = &muts[1]["caught_by"];
    assert!(
        dropped
            .as_array()
            .is_some_and(|c| c.iter().any(|v| v == "completeness")),
        "dropped-page caught_by: {dropped}"
    );
}
