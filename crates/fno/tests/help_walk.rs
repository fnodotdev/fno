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

/// The agents table's non-retired leaves, walked in-process and probed
/// through the real binary: the user path for every command the menu lists.
#[test]
fn fno_agents_table_leaves_answer_help() {
    let bin = env!("CARGO_BIN_EXE_fno");
    for (path, node) in fno::cli::agents_menu::walk(&fno::cli::agents_menu::ROOT) {
        if node.retired {
            continue;
        }
        let words: Vec<&str> = std::iter::once("agent")
            .chain(path.iter().copied())
            .collect();
        let out = Command::new(bin)
            .args(words)
            .arg("--help")
            .output()
            .expect("spawn fno");
        assert_eq!(
            out.status.code(),
            Some(0),
            "fno agent {} --help must exit 0; stderr: {}",
            path.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Every live command-names row answers --help: the generic arm prints the
/// row's note and exits 0, so a help request never reaches a door that
/// would run real work on it.
#[test]
fn fno_tsv_rows_answer_help() {
    let bin = env!("CARGO_BIN_EXE_fno");
    let rows: Vec<Vec<&str>> = include_str!("../command-names.tsv")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.split('\t').nth(1) == Some("live"))
        .map(|l| l.split('\t').next().expect("tsv row").split(' ').collect())
        .collect();
    assert!(
        rows.len() >= 20,
        "the tsv walk covered only {} rows",
        rows.len()
    );
    for path in &rows {
        let out = Command::new(bin)
            .args(path)
            .arg("--help")
            .output()
            .expect("spawn fno");
        assert_eq!(
            out.status.code(),
            Some(0),
            "fno {} --help must exit 0; stderr: {}",
            path.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
