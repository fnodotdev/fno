//! The help walk: `fno-agent --help` and one `--help` per help-displayed
//! routable command exit 0. Binary-direct machine doors are not
//! help-listed; the follow-up node widens this walk to the full committed
//! leaf inventory.

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

#[test]
fn fno_agent_answers_help_on_every_routable_command() {
    let bin = env!("CARGO_BIN_EXE_fno-agent");
    let mut rows: Vec<&str> = vec!["-h", "help", "version"];
    rows.extend(
        fno_agent::spawn::usage::CLIENT_COMMAND_USAGE
            .iter()
            .map(|usage| {
                usage
                    .split_whitespace()
                    .next()
                    .expect("usage row starts with the command")
            }),
    );
    let mut seen = std::collections::BTreeSet::new();
    for command in rows {
        assert!(seen.insert(command), "{command} listed twice");
        let out = run(bin, &[command, "--help"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{command} --help must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
