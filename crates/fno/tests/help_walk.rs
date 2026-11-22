use std::process::Command;

/// The tail carriers, one per line: a `--help` after a selector is the
/// pane payload's, never the family's. One committed list:
/// scripts/ci/help-walk-tails.tsv, read by both the walk (below) and the
/// unit test that pins the typed tree to it.
fn tail_carriers() -> Vec<String> {
    include_str!("../../../scripts/ci/help-walk-tails.tsv")
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}
#[test]
fn fno_clap_tree_answers_help_everywhere_except_tails() {
    let bin = env!("CARGO_BIN_EXE_fno");
    let mut walked = 0usize;
    for row in fno::cli_args::render_inventory().lines() {
        if row.starts_with('#') || row.is_empty() {
            continue;
        }
        let path = row.split('\t').next().expect("inventory row");
        if path == "(root)" {
            let out = Command::new(bin).arg("--help").output().expect("spawn fno");
            assert_eq!(out.status.code(), Some(0), "fno --help must exit 0");
            walked += 1;
            continue;
        }
        let tails = tail_carriers();
        if tails.iter().any(|t| t == path) {
            continue;
        }
        let tokens: Vec<&str> = path.split(' ').collect();
        let out = Command::new(bin)
            .args(tokens)
            .arg("--help")
            .output()
            .expect("spawn fno");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{path} --help must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        walked += 1;
    }
    assert!(walked > 30, "the walk covered only {walked} rows");
    for tail in tail_carriers() {
        assert!(
            fno::cli_args::render_inventory()
                .lines()
                .any(|row| row.split('\t').next() == Some(tail.as_str())),
            "named tail carrier {tail} must exist in the inventory"
        );
    }
}
