use std::process::{Command, Stdio};

/// `Command::output` is disallowed (spawn seams); this reproduces it
/// through spawn + wait_with_output: stdin nulled, stdout/stderr piped.
fn run(bin: &str, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the binary")
        .wait_with_output()
        .expect("wait for the binary")
}

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
            let out = run(bin, &["--help"]);
            assert_eq!(out.status.code(), Some(0), "fno --help must exit 0");
            walked += 1;
            continue;
        }
        let tails = tail_carriers();
        if tails.iter().any(|t| t == path) {
            continue;
        }
        let mut args: Vec<&str> = path.split(' ').collect();
        args.push("--help");
        let out = run(bin, &args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{path} --help must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        walked += 1;
    }
    assert!(walked > 25, "the walk covered only {walked} rows");
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
        let mut args: Vec<&str> = std::iter::once("agent")
            .chain(path.iter().copied())
            .collect();
        args.push("--help");
        let out = run(bin, &args);
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
        let mut args: Vec<&str> = path.clone();
        args.push("--help");
        let out = run(bin, &args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "fno {} --help must exit 0; stderr: {}",
            path.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The transcript door's own command table, walked against the real binary:
/// the advertised leaf and both sub-leaves answer help with exit 0 and usage
/// on stdout. The leaf walk above stops at menu rows; `send` and `receive`
/// live one level beneath them, and that is where `--help` was consumed as
/// data (x-02e6: the leaf exited 2, `send --help` searched for a session
/// literally named "--help").
#[test]
fn fno_transcript_door_answers_help_at_leaf_and_subleaves() {
    let bin = env!("CARGO_BIN_EXE_fno");
    for args in [
        vec!["agent", "registry", "transcript", "--help"],
        vec!["agent", "registry", "transcript", "send", "--help"],
        vec!["agent", "registry", "transcript", "receive", "--help"],
    ] {
        let out = run(bin, &args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "fno {} must exit 0; stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.stdout.is_empty(),
            "fno {}: help must print usage to stdout",
            args.join(" ")
        );
    }
}
