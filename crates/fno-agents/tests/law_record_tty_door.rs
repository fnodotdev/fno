//! The record door's operator proof, proven against real processes.
//!
//! `fno inbox law set` used to pipe the record request to the worker, so the
//! worker's fd 0 was never a tty and a user at a real terminal was refused
//! with "no session identity and no terminal". A terminal `set` now rides the
//! request on argv with stdin inherited, the same shape the retract door
//! uses, and the worker reads the inherited tty as the operator proof.
//! These tests spawn the worker through both transports and assert the two
//! outcomes: a real tty records under operator authority, a piped stdin
//! refuses and records nothing.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn worker_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents-worker")
}

struct Fixture(TempDir);

impl Fixture {
    /// A scratch root whose work map names the cwd, so a project-scope law
    /// records: the scope stamp refuses a cwd no settings file names, and a
    /// fresh temp dir is named by nothing.
    fn new() -> Fixture {
        let dir = TempDir::new().expect("scratch dir");
        let config = dir.path().join(".fno").join("config.toml");
        std::fs::create_dir_all(config.parent().expect("config parent")).expect("mkdir");
        std::fs::write(
            &config,
            format!(
                "[work.projects.lawtty]\npath = \"{}\"\n",
                dir.path().display()
            ),
        )
        .expect("write work map");
        std::fs::create_dir_all(dir.path().join("agents-home")).expect("mkdir");
        Fixture(dir)
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn home(&self) -> PathBuf {
        self.0.path().join("agents-home")
    }
}

fn record_request(subject: &str, decision: &str) -> String {
    // The statement validator runs in the record door's preflight, before
    // the identity gate, so every request carries a rationale to get past
    // it: these tests prove the transport, not the wording.
    json!({
        "mode": "record",
        "argv": [subject, decision, "--rationale", "Prove the transport, not the wording."],
        "stdin": "",
    })
    .to_string()
}

fn history_request(subject: &str) -> String {
    json!({"mode": "history", "argv": [subject]}).to_string()
}

/// The transport the fixed front door uses at a terminal: the request rides
/// argv and the worker's fd 0 is a real tty.
fn spawn_attended(fixture: &Fixture, request: &str) -> (i32, String) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut command = CommandBuilder::new(worker_bin());
    command.arg("--law-exec-arg");
    command.arg(request);
    command.env_clear();
    for (key, value) in fno_agents::test_run::self_owner_env() {
        command.env(key, value);
    }
    command.env("HOME", fixture.root());
    command.env("FNO_AGENTS_HOME", fixture.home());
    command.cwd(fixture.root());
    let mut child = pair.slave.spawn_command(command).expect("spawn worker");
    drop(pair.slave);
    let mut output = String::new();
    reader
        .read_to_string(&mut output)
        .expect("pty output reads");
    let status = child.wait().expect("worker exits");
    (status.exit_code() as i32, output)
}

/// The piped transport: what a daemon or an agent lane sends, and what the
/// front door used to send for every `set`.
fn spawn_piped(fixture: &Fixture, request: &str) -> (i32, String, String) {
    let mut cmd = Command::new(worker_bin());
    cmd.arg("--law-exec")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.env_clear();
    for (key, value) in fno_agents::test_run::self_owner_env() {
        cmd.env(key, value);
    }
    cmd.env("HOME", fixture.root());
    cmd.env("FNO_AGENTS_HOME", fixture.home());
    cmd.current_dir(fixture.root());
    let mut child = cmd.spawn().expect("spawn worker");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(request.as_bytes())
        .expect("request writes");
    let out = child.wait_with_output().expect("worker exits");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A row field under either shape: the envelope nests the columns under
/// `data`, readers flatten. Both name the same fact.
fn field<'a>(row: &'a Value, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    if let Some(v) = row.get(key) {
        return v;
    }
    row.get("data").and_then(|d| d.get(key)).unwrap_or(&NULL)
}

#[test]
fn a_terminal_set_records_under_operator_authority() {
    let fixture = Fixture::new();
    let (code, output) = spawn_attended(
        &fixture,
        &record_request("tty-door", "A real terminal is the operator proof."),
    );
    assert_eq!(code, 0, "attended record refused: {output}");
    let id = output
        .split_whitespace()
        .find(|t| t.starts_with("d-"))
        .expect("stdout carries the decision id")
        .to_owned();
    let (code, stdout, stderr) = spawn_piped(&fixture, &history_request("tty-door"));
    assert_eq!(code, 0, "history reads: {stderr}");
    let answer: Value = serde_json::from_str(stdout.trim()).expect("history json");
    let rows = answer
        .get("chain")
        .and_then(Value::as_array)
        .expect("history chain");
    assert!(!rows.is_empty(), "the recorded law reads back: {answer}");
    let live = rows
        .first()
        .expect("the non-empty chain holds the live row");
    assert_eq!(field(live, "decision_id"), &json!(id), "{live}");
    assert_eq!(
        field(live, "authority_source"),
        &json!("operator"),
        "{live}"
    );
    assert_eq!(field(live, "decided_by"), &json!("operator"), "{live}");
    assert_eq!(field(live, "attested_by"), &json!("operator"), "{live}");
}

#[test]
fn a_payload_read_from_the_terminal_rides_argv_attended() {
    let fixture = Fixture::new();
    let (code, output) = spawn_attended(
        &fixture,
        &json!({
            "mode": "record",
            "argv": [
                "payload-door",
                "--decision-file",
                "-",
                "--rationale",
                "Prove the payload rides argv attended.",
            ],
            "stdin": "The terminal fed the decision text.",
        })
        .to_string(),
    );
    assert_eq!(code, 0, "attended payload record refused: {output}");
    let (code, stdout, stderr) = spawn_piped(&fixture, &history_request("payload-door"));
    assert_eq!(code, 0, "history reads: {stderr}");
    let answer: Value = serde_json::from_str(stdout.trim()).expect("history json");
    let rows = answer
        .get("chain")
        .and_then(Value::as_array)
        .expect("history chain");
    assert!(!rows.is_empty(), "the recorded law reads back: {answer}");
    let live = rows
        .first()
        .expect("the non-empty chain holds the live row");
    assert_eq!(
        field(live, "decision"),
        &json!("The terminal fed the decision text."),
        "{live}"
    );
    assert_eq!(
        field(live, "authority_source"),
        &json!("operator"),
        "{live}"
    );
}

#[test]
fn a_piped_set_refuses_and_records_nothing() {
    let fixture = Fixture::new();
    let (code, stdout, stderr) = spawn_piped(
        &fixture,
        &record_request("piped-door", "A pipe proves no operator."),
    );
    assert_eq!(code, 3, "piped record must refuse: {stdout} {stderr}");
    assert!(
        stderr.contains("no session identity and no terminal"),
        "the refusal names what is missing: {stderr}"
    );
    let (code, _stdout, stderr) = spawn_piped(&fixture, &history_request("piped-door"));
    assert_eq!(code, 3, "nothing recorded under the refused subject");
    assert!(
        stderr.contains("no decision under"),
        "history is empty: {stderr}"
    );
}
