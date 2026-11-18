use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use tempfile::TempDir;

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn artifact(root: &Path, path: &str, text: &str) -> Value {
    let dest = root.join(path);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, text).unwrap();
    json!({"path": path, "sha256": hash(text.as_bytes())})
}

fn corpus() -> (TempDir, Value) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        json!({"harness":"codex","model":"fixed-model","effort":"high","budget_seconds":600});
    let mut runs = vec![];
    let mut tasks = vec![];
    for id in ["empty-result", "selected-result"] {
        let input = format!("Return the recorded result for {id}.");
        let expected = json!({"task":id,"count":if id=="empty-result" {0} else {2}});
        tasks.push(json!({"id":id,"input_sha256":hash(input.as_bytes()),"expected":expected}));
        for arm in ["baseline", "candidate"] {
            for repeat in 1..=3 {
                let prefix = format!("{arm}/{id}/{repeat}");
                let input_ref = artifact(dir.path(), &format!("{prefix}/input.txt"), &input);
                let result_ref = artifact(
                    dir.path(),
                    &format!("{prefix}/result.json"),
                    &expected.to_string(),
                );
                let scratch_dir = format!("{prefix}/scratch");
                fs::create_dir_all(dir.path().join(&scratch_dir)).unwrap();
                let scratch = if arm == "baseline" {
                    let text = "import json\nprint(json.loads(raw))\n";
                    let record = artifact(dir.path(), &format!("{scratch_dir}/wrap.py"), text);
                    vec![json!({"path":"wrap.py","sha256":record["sha256"]})]
                } else {
                    vec![]
                };
                runs.push(json!({
                    "task_id":id,"repeat":repeat,"arm":arm,
                    "attempt_id":prefix,"started_at":"2026-09-12T01:00:00Z",
                    "revision":if arm=="baseline" {"a".repeat(40)} else {"b".repeat(40)},
                    "runtime":runtime,"termination":"completed","exit_code":0,
                    "input":input_ref,"result":result_ref,"capture_complete":true,
                    "scratch_dir":scratch_dir,"scratch":scratch
                }));
            }
        }
    }
    let manifest = json!({
        "schema_version":1,"kind":"calibration","cohort_id":"wrapper-calibration",
        "declared_at":"2026-09-12T00:00:00Z","intervention":"Return one JSON document on stdout",
        "baseline_revision":"a".repeat(40),"candidate_revision":"b".repeat(40),
        "runtime":runtime,"repetitions":3,"tasks":tasks,"runs":runs
    });
    (dir, manifest)
}

#[test]
fn fewer_wrappers_with_correct_outputs_has_exact_denominators() {
    let (dir, m) = corpus();
    let r = evaluate(dir.path(), &m).unwrap();
    assert_eq!(r["comparison"], "improved");
    assert_eq!(r["baseline"]["attempts"], 6);
    assert_eq!(r["baseline"]["wrappers"], 6);
    assert_eq!(r["candidate"]["completed"], 6);
    assert_eq!(r["candidate"]["wrappers_per_attempt"], 0.0);
    assert_eq!(r["tasks"].as_array().unwrap().len(), 2);
}

#[test]
fn incomplete_or_altered_capture_is_rejected() {
    let (dir, mut m) = corpus();
    m["runs"][0]["capture_complete"] = json!(false);
    assert!(evaluate(dir.path(), &m).unwrap_err().contains("capture"));
    m["runs"][0]["capture_complete"] = json!(true);
    fs::write(
        dir.path().join("baseline/empty-result/1/scratch/wrap.py"),
        "changed",
    )
    .unwrap();
    assert!(evaluate(dir.path(), &m).unwrap_err().contains("hash"));
}

#[test]
fn all_failed_and_no_baseline_signal_never_improve() {
    let (dir, mut m) = corpus();
    for run in m["runs"].as_array_mut().unwrap() {
        run["termination"] = json!("task_failed");
        run["exit_code"] = json!(1);
        run["result"] = Value::Null;
    }
    assert_eq!(
        evaluate(dir.path(), &m).unwrap()["comparison"],
        "insufficient"
    );
    let (dir, mut m) = corpus();
    for run in m["runs"].as_array_mut().unwrap() {
        for file in run["scratch"].as_array().unwrap() {
            fs::remove_file(
                dir.path()
                    .join(run["scratch_dir"].as_str().unwrap())
                    .join(file["path"].as_str().unwrap()),
            )
            .unwrap();
        }
        run["scratch"] = json!([]);
    }
    assert_eq!(
        evaluate(dir.path(), &m).unwrap()["comparison"],
        "insufficient"
    );
}
