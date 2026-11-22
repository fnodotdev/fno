//! The help walk (x-9cb4, PR F): `fno-agent --help` and one `--help` per
//! help-displayed routable command exit 0. Binary-direct machine doors are
//! not help-listed; they join through the carveout (x-292e), which also
//! widens this walk to the full committed leaf inventory.

use std::process::Command;

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
        let out = Command::new(bin)
            .args([command, "--help"])
            .output()
            .expect("spawn fno-agent");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{command} --help must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
