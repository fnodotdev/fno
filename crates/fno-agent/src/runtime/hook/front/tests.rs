use super::*;

fn context_leg(_args: &[String]) -> i32 {
    crate::hook::emit_line(
        r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"from json"}}"#,
    );
    0
}

fn plain_leg(_args: &[String]) -> i32 {
    crate::hook::emit_line("## plain notice");
    0
}

fn echo_session_leg(_args: &[String]) -> i32 {
    let (_, payload) = payload();
    crate::hook::emit_line(&format!("session {}", payload.session()));
    0
}

fn failing_leg(_args: &[String]) -> i32 {
    crate::hook::emit_line(r#"{"decision":"block","reason":"never relayed"}"#);
    1
}

fn panicking_leg(_args: &[String]) -> i32 {
    panic!("leg fault");
}

fn late_leg(_args: &[String]) -> i32 {
    std::thread::sleep(Duration::from_secs(3));
    crate::hook::emit_line(r#"{"decision":"block","reason":"too late"}"#);
    0
}

/// AC2-HP: every leg runs once on the one payload, and each leg's notice
/// (JSON context and plain text alike) joins in table order.
#[test]
fn prompt_front_joins_every_leg_notice() {
    let legs = vec![
        Leg::new("context", 5, context_leg),
        Leg::new("plain", 5, plain_leg),
        Leg::new("echo", 5, echo_session_leg),
    ];
    let out = run_legs(
        Event::UserPromptSubmit,
        legs,
        r#"{"session_id":"abc","prompt":"hi"}"#,
    );
    assert_eq!(
        out["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "from json\n\n## plain notice\n\nsession abc",
        "{out}"
    );
    assert!(out.get("decision").is_none(), "{out}");
}

/// AC2-ERR: a leg that answers nonzero, panics or runs past its timeout is
/// skipped, its output is dropped, and the others still answer.
#[test]
fn failed_and_late_legs_are_skipped() {
    let legs = vec![
        Leg::new("failing", 5, failing_leg),
        Leg::new("panicking", 5, panicking_leg),
        Leg::new("late", 1, late_leg),
        Leg::new("plain", 5, plain_leg),
    ];
    let started = Instant::now();
    let out = run_legs(Event::SessionStart, legs, "{}");
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "the late leg must not hold the answer past its timeout"
    );
    assert!(out.get("decision").is_none(), "{out}");
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "## plain notice"
    );
}

/// AC3-HP: every block reason joins into one Stop block at exit 0; plain
/// text and context never reach a Stop answer.
#[test]
fn stop_front_joins_every_block() {
    let outs = vec![
        r#"{"decision":"block","reason":"target: no PR yet"}"#.to_string(),
        "## Operator capture\n\nplain".to_string(),
        r#"{"decision":"block","reason":"context: 61% used"}"#.to_string(),
        "{}".to_string(),
    ];
    let out = merge(Event::Stop, &outs);
    assert_eq!(
        out,
        json!({"decision": "block", "reason": "target: no PR yet\n\ncontext: 61% used"})
    );
    assert_eq!(merge(Event::Stop, &["{}".to_string()]), json!({}));
}

/// AC3-ERR: a deployed fno-agent with no `front` entry answers exit 2; the
/// shim skips it and answers `{}`, so the stop allows.
#[test]
fn shim_skips_a_binary_without_the_front_entry() {
    let dir = tempfile::tempdir().unwrap();
    let old = crate::write_exec_stub(
        dir.path(),
        "fno-agent",
        "#!/bin/bash\necho 'fno-agent hook: unknown entry Some(\"front\")' >&2\nexit 2\n",
    );
    let shim = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hooks/hook-front.sh");
    let mut child = std::process::Command::new("/bin/bash")
        .arg(&shim)
        .arg("Stop")
        .env("FNO_AGENT_BIN", &old)
        .env_remove("FNO_AGENT_FRONT")
        .env("PATH", dir.path())
        .current_dir(dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(br#"{"hook_event_name":"Stop","session_id":"s"}"#)
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
}
