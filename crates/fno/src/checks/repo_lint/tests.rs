//! Contract tests for the ported lint checks, on fixture trees. The Python
//! suites died with their legs; these hold the load-bearing contracts:
//! each scanner fires on its shape, the ratchets fail both directions, and
//! the dispatcher refuses an unknown check with the real names.

use super::*;

fn fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fno-repo-lint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fixture dir");
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

#[test]
fn dispatcher_refuses_an_unknown_check_with_the_real_names() {
    let err = parse(&["bogus-check".to_string()]).unwrap_err();
    assert!(err.contains("unknown check"), "{err}");
    assert!(err.contains("state-roots"), "{err}");
    assert!(err.contains("seam-crossings"), "{err}");
}

#[test]
fn state_roots_rule_a_fires_on_a_hand_built_path_and_both_directions_hold() {
    let dir = fixture("state-roots");
    write(
        &dir,
        "cli/src/fno/bad.py",
        "from pathlib import Path\n\ndef bad(root):\n    return root / '.fno' / 'ledger.json'\n",
    );
    let hits = state_roots::rule_a_violations(&dir);
    assert_eq!(hits.len(), 1, "{hits:?}");
    let (filename, rel, _msg) = &hits[0];
    assert_eq!(filename, "ledger.json");
    assert_eq!(rel, "cli/src/fno/bad.py");
    // The stale direction: a baselined site that no longer matches fails.
    let baseline = dir.join("scripts/ci/state-roots-baseline.txt");
    std::fs::create_dir_all(baseline.parent().unwrap()).unwrap();
    std::fs::write(&baseline, "A\tcli/src/fno/fixed.py\tledger.json\n").unwrap();
    let inv = Invocation {
        check: "state-roots".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(state_roots::run(&inv), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn state_roots_rule_c_names_a_planted_join_the_table_does_not_carry() {
    let dir = fixture("state-roots-c");
    write(
        &dir,
        "docs/state-root-layout.tsv",
        "db\tdb\tkeep\tdaemon\nbriefs\tdb/briefs\tdir\tdaemon\n.reconcile-*\t-\tpark\tdaemon\n",
    );
    write(
        &dir,
        "crates/x/src/a.rs",
        "fn a(state_root: &Path) {\n    let _ = state_root.join(\"db\").join(\"x\");\n    let _ = state_root.join(\"briefs\");\n    let _ = state_root.join(\".reconcile-1\");\n    let _ = place(&state_root, \"planted-new\");\n}\n",
    );
    let hits = state_roots::rule_c_violations(&dir);
    assert_eq!(hits.len(), 1, "only the untabled name fires: {hits:?}");
    let (name, rel, msg) = hits.first().expect("one hit");
    assert_eq!(
        (name.as_str(), rel.as_str()),
        ("planted-new", "crates/x/src/a.rs")
    );
    assert!(msg.contains("crates/x/src/a.rs:5"), "{msg}");
    std::fs::remove_file(dir.join("docs/state-root-layout.tsv")).unwrap();
    assert_eq!(
        state_roots::rule_c_violations(&dir).len(),
        1,
        "no table fails closed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_paths_fires_on_a_hand_assembled_session_argv() {
    let dir = fixture("spawn-paths");
    write(
        &dir,
        "cli/src/fno/rogue.py",
        "cmd = ['claude', '-p', 'go']\n",
    );
    let inv = Invocation {
        check: "spawn-paths".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(spawn_paths(&inv), 1);
    // The allowlisted files stay silent: a clean tree passes.
    std::fs::remove_file(dir.join("cli/src/fno/rogue.py")).unwrap();
    assert_eq!(spawn_paths(&inv), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn plan_filenames_flags_a_filename_that_disagrees_with_its_claim() {
    let dir = fixture("plan-filenames");
    write(
        &dir,
        "plans/plan-x-aaaa.md",
        "---\nnode: x-bbbb\n---\nbody\n",
    );
    let plans_dir = dir.join("plans");
    let prefixes = ["x".to_string(), "ab".to_string()].into_iter().collect();
    let scan = plan_filenames::scan_dir(&plans_dir, &prefixes);
    match scan {
        plan_filenames::Scan::Mismatched(rows) => {
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert!(rows[0].line.contains("x-aaaa"), "{rows:?}");
            assert!(rows[0].line.contains("x-bbbb"), "{rows:?}");
        }
        plan_filenames::Scan::Clean => panic!("expected a mismatch"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn command_ratchet_refuses_a_rust_leaf_the_baseline_lacks() {
    let dir = fixture("ratchet");
    write(
        &dir,
        "scripts/ci/native-command-tree.txt",
        "# gen\nmux\tgroup\t-\nversion\troot\t-\n",
    );
    write(
        &dir,
        "scripts/ci/command-collapse-map.tsv",
        "current-leaf\ttier\nwhoami x\tT1\n",
    );
    write(&dir, "scripts/ci/command-collapse-flags.txt", "# c\n");
    write(&dir, "scripts/ci/command-baseline.txt", "# baseline\nmux\n");
    let inv = Invocation {
        check: "command-ratchet".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(command_ratchet::run(&inv), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn claims_pattern_gains_only_the_sequential_arm() {
    assert_eq!(
        plan_filenames::claims_pattern(false, "t"),
        r"^[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}$"
    );
    let seq = regex::Regex::new(&plan_filenames::claims_pattern(true, "t"))
        .expect("claims regex compiles");
    assert!(seq.is_match("t-15"));
    // Hex ids stay claims in a sequential project.
    assert!(seq.is_match("x-ffff"));
    // Another prefix's token and a zero lead stay prose.
    assert!(!seq.is_match("u-9"));
    assert!(!seq.is_match("t-07"));
}

#[test]
fn door_prefix_answers_the_documented_default() {
    // Admit arm: a receipt naming x- trims to x.
    assert_eq!(door_prefix_from(Some(r#"{"prefix": "x-"}"#)), "x");
    // Refuse arms: a failed, empty or malformed door answers the
    // documented default, never the legacy ab.
    assert_eq!(door_prefix_from(None), "x");
    assert_eq!(door_prefix_from(Some("")), "x");
    assert_eq!(door_prefix_from(Some("not json")), "x");
    // The allowlist shape keeps the legacy ab explicitly.
    let allowlist: std::collections::BTreeSet<String> = [door_prefix_from(None), "ab".to_string()]
        .into_iter()
        .collect();
    assert!(allowlist.contains("x"));
    assert!(allowlist.contains("ab"));
}
