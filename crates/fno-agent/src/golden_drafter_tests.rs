use super::*;
use std::cell::RefCell;

const BANK: &str = "id: golden-demo\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: reply.md\n  - kind: exit\n    command: \"grep -q yes reply.md\"\ntimeout_minutes: 5\ntags: [golden]\ngolden:\n  source: {kind: correction, ref: \"c:1\"}\n  quote: q\n  expected: e\n  rule: r\n  failure_type: evasive-answer\n  cost: trust\n  outcome: failed\n";

fn fixture(bad_passes: bool) -> String {
    let bad = if bad_passes { "yes" } else { "nope" };
    format!("cases:\n  - {{id: g1, label: good, files: {{\"reply.md\": \"yes\\n\"}}}}\n  - {{id: g2, label: good, files: {{\"reply.md\": \"yes 2\\n\"}}}}\n  - {{id: g3, label: good, files: {{\"reply.md\": \"yes 3\\n\"}}}}\n  - {{id: b1, label: bad, files: {{\"reply.md\": \"{bad}\\n\"}}}}\n  - {{id: b2, label: bad, files: {{\"reply.md\": \"no\\n\"}}}}\n  - {{id: b3, label: bad, files: {{}}}}\n")
}

fn repo_with_catalog() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let bank_dir = repo.path().join("evals").join("bank");
    std::fs::create_dir_all(&bank_dir).unwrap();
    std::fs::write(
        bank_dir.join("failure-types.yaml"),
        "cost_order: [trust]\ntypes:\n  - {id: evasive-answer, cost: trust, description: d}\n",
    )
    .unwrap();
    repo
}

/// Stage `id` into the drafts dir's pending batch without a model call.
fn stage_fake(dir: &Path, id: &str) {
    let bank = dir.join("pending").join("bank");
    let fix = dir.join("pending").join("fixtures");
    std::fs::create_dir_all(&bank).unwrap();
    std::fs::create_dir_all(&fix).unwrap();
    std::fs::write(bank.join(format!("{id}.yaml")), "bank-yaml").unwrap();
    std::fs::write(fix.join(format!("{id}.yaml")), "fixture-yaml").unwrap();
}

#[test]
fn kept_waits_below_threshold_and_stages() {
    let repo = repo_with_catalog();
    let dir = tempfile::tempdir().unwrap();
    for h in ["aaa", "bbb", "ccc"] {
        std::fs::write(dir.path().join(format!("{h}.yaml")), "id: x\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    let calls = RefCell::new(0);
    let spawn = |name: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| {
        *calls.borrow_mut() += 1;
        let reply = match name {
            "golden-drafter-aaa" => {
                json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)})
            }
            "golden-drafter-bbb" => json!({"verdict": "no_case", "reason": "no repo rule"}),
            _ => {
                json!({"verdict": "case", "bank": BANK.replace("golden-demo", "golden-demo2"), "fixture": fixture(true)})
            }
        };
        Ok((0, format!("sure: {reply}"), String::new()))
    };
    let opens = RefCell::new(0);
    let open = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| {
        *opens.borrow_mut() += 1;
        Ok("https://example/pr/1".to_string())
    };
    let out = fill(
        dir.path(),
        repo.path(),
        std::time::SystemTime::now(),
        5,
        &spawn,
        &open,
    )
    .unwrap();
    assert_eq!(
        out.pointer("/kept").and_then(|v| v.as_u64()),
        Some(1),
        "{out}"
    );
    assert_eq!(
        out.pointer("/dropped").and_then(|v| v.as_u64()),
        Some(2),
        "{out}"
    );
    assert!(
        out.pointer("/pr").map(|v| v.is_null()).unwrap_or(true),
        "{out}"
    );
    assert_eq!(*opens.borrow(), 0, "below threshold opens nothing");
    assert_eq!(*calls.borrow(), 3);
    let handled = std::fs::read_to_string(dir.path().join(".drafted")).unwrap();
    assert_eq!(handled.lines().count(), 3, "{handled}");
    assert!(handled.contains("aaa\tkept golden-demo"), "{handled}");
    assert!(handled.contains("bbb\tno_case"), "{handled}");
    assert!(handled.contains("ccc\tgrade_unproved"), "{handled}");
    assert!(open_drafts(dir.path()).is_empty());
    assert!(dir
        .path()
        .join("pending")
        .join("bank")
        .join("golden-demo.yaml")
        .exists());
}

#[test]
fn batch_opens_at_threshold() {
    let repo = repo_with_catalog();
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=9 {
        stage_fake(dir.path(), &format!("golden-f{i}"));
    }
    std::fs::write(dir.path().join("aaa.yaml"), "id: x\n").unwrap();
    let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| {
        let reply = json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)});
        Ok((0, format!("sure: {reply}"), String::new()))
    };
    let edits_seen: RefCell<Vec<(PathBuf, Vec<u8>)>> = RefCell::new(Vec::new());
    let open = |_: &str, _: &str, _: &str, edits: &[(PathBuf, Vec<u8>)]| {
        *edits_seen.borrow_mut() = edits.to_vec();
        Ok("https://example/pr/2".to_string())
    };
    let out = fill(
        dir.path(),
        repo.path(),
        std::time::SystemTime::now(),
        5,
        &spawn,
        &open,
    )
    .unwrap();
    assert_eq!(
        out.pointer("/kept").and_then(|v| v.as_u64()),
        Some(1),
        "{out}"
    );
    assert_eq!(
        out.pointer("/pr").and_then(|v| v.as_str()),
        Some("https://example/pr/2"),
        "{out}"
    );
    let edits = edits_seen.into_inner();
    assert_eq!(edits.len(), 20);
    let paths: Vec<String> = edits.iter().map(|(p, _)| p.display().to_string()).collect();
    let ids: Vec<String> = (1..=9)
        .map(|i| format!("golden-f{i}"))
        .chain(["golden-demo".to_string()])
        .collect();
    for id in &ids {
        assert!(
            paths.contains(&format!("evals/bank/{id}.yaml")),
            "{id}: {paths:?}"
        );
        assert!(
            paths.contains(&format!("evals/fixtures/metrics/{id}.yaml")),
            "{id}: {paths:?}"
        );
    }
    let demo = edits
        .iter()
        .find(|(p, _)| p.display().to_string() == "evals/bank/golden-demo.yaml")
        .unwrap();
    assert_eq!(demo.1, BANK.as_bytes());
    assert!(pending_files(dir.path()).is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("pending").join("fixtures"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn waits_below_threshold_with_staged_goldens() {
    let repo = repo_with_catalog();
    let dir = tempfile::tempdir().unwrap();
    stage_fake(dir.path(), "golden-f1");
    stage_fake(dir.path(), "golden-f2");
    let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| panic!("no drafts queued");
    let opens = RefCell::new(0);
    let open = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| {
        *opens.borrow_mut() += 1;
        Ok("https://example/pr/3".to_string())
    };
    let out = fill(
        dir.path(),
        repo.path(),
        std::time::SystemTime::now(),
        0,
        &spawn,
        &open,
    )
    .unwrap();
    assert_eq!(
        out.pointer("/waiting").and_then(|v| v.as_u64()),
        Some(2),
        "{out}"
    );
    assert_eq!(
        out.pointer("/kept").and_then(|v| v.as_u64()),
        Some(0),
        "{out}"
    );
    assert!(
        out.pointer("/pr").map(|v| v.is_null()).unwrap_or(true),
        "{out}"
    );
    assert_eq!(*opens.borrow(), 0);
    assert_eq!(pending_files(dir.path()).len(), 2);
}

#[test]
fn batch_opens_when_oldest_is_14_days_old() {
    let repo = repo_with_catalog();
    let dir = tempfile::tempdir().unwrap();
    for id in ["golden-f1", "golden-f2", "golden-f3"] {
        stage_fake(dir.path(), id);
    }
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(15 * 24 * 60 * 60);
    let staged = dir
        .path()
        .join("pending")
        .join("bank")
        .join("golden-f1.yaml");
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&staged)
        .unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
    let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| panic!("no drafts queued");
    let edits_seen: RefCell<Vec<(PathBuf, Vec<u8>)>> = RefCell::new(Vec::new());
    let open = |_: &str, _: &str, _: &str, edits: &[(PathBuf, Vec<u8>)]| {
        *edits_seen.borrow_mut() = edits.to_vec();
        Ok("https://example/pr/4".to_string())
    };
    let out = fill(
        dir.path(),
        repo.path(),
        std::time::SystemTime::now(),
        0,
        &spawn,
        &open,
    )
    .unwrap();
    assert_eq!(
        out.pointer("/pr").and_then(|v| v.as_str()),
        Some("https://example/pr/4"),
        "{out}"
    );
    let edits = edits_seen.into_inner();
    assert_eq!(edits.len(), 6);
    let paths: Vec<String> = edits.iter().map(|(p, _)| p.display().to_string()).collect();
    for id in ["golden-f1", "golden-f2", "golden-f3"] {
        assert!(
            paths.contains(&format!("evals/bank/{id}.yaml")),
            "{id}: {paths:?}"
        );
        assert!(
            paths.contains(&format!("evals/fixtures/metrics/{id}.yaml")),
            "{id}: {paths:?}"
        );
    }
    assert!(pending_files(dir.path()).is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("pending").join("fixtures"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn failed_ship_leaves_pending_for_retry() {
    let repo = repo_with_catalog();
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=10 {
        stage_fake(dir.path(), &format!("golden-f{i}"));
    }
    std::fs::write(dir.path().join("aaa.yaml"), "id: x\n").unwrap();
    let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| {
        let reply = json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)});
        Ok((0, format!("sure: {reply}"), String::new()))
    };
    let fail = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| Err("gh down".to_string());
    let out = fill(
        dir.path(),
        repo.path(),
        std::time::SystemTime::now(),
        5,
        &spawn,
        &fail,
    );
    assert!(out.is_err(), "{out:?}");
    assert_eq!(pending_files(dir.path()).len(), 11);
    assert_eq!(
        std::fs::read_dir(dir.path().join("pending").join("fixtures"))
            .unwrap()
            .count(),
        11
    );
    let handled = std::fs::read_to_string(dir.path().join(".drafted")).unwrap();
    assert!(handled.contains("aaa\tkept golden-demo"), "{handled}");
    let open = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| {
        Ok("https://example/pr/5".to_string())
    };
    let out = fill(
        dir.path(),
        repo.path(),
        std::time::SystemTime::now(),
        0,
        &spawn,
        &open,
    )
    .unwrap();
    assert_eq!(
        out.pointer("/pr").and_then(|v| v.as_str()),
        Some("https://example/pr/5"),
        "{out}"
    );
    assert!(pending_files(dir.path()).is_empty());
}
