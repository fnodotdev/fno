//! `fno agent mail sent` answers in process: with no `fno-agent` binary on
//! the path, the front door still prints the view fno-agent's own leaf froze
//! in `tests/golden/mail_sent/claimed_by_cursor`, field for field.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const HANDLE: &str = "abcd1234";

fn iso_ago(seconds_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(seconds_ago))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn row(id: &str) -> String {
    json!({
        "v": 1, "id": id, "ts": iso_ago(7200), "thread": format!("th-{id}"),
        "from": HANDLE, "to": "worker2", "kind": "send", "body": "hello",
    })
    .to_string()
}

#[test]
fn front_door_lists_sent_mail_by_cursor_without_fno_agent() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("fno-mail-sent-door-{}", std::process::id()));
    let bus = root.join("bus");
    std::fs::create_dir_all(bus.join("cursors")).expect("bus dir");
    std::fs::write(
        bus.join("messages.jsonl"),
        format!("{}\n{}\n", row("m-old2"), row("m-old3")),
    )
    .expect("bus log");
    std::fs::write(
        bus.join("cursors").join("worker2.json"),
        json!({"last_seen_id": "m-old2", "ts": iso_ago(0)}).to_string(),
    )
    .expect("cursor");

    let out = Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(["agent", "mail", "sent", "--from-name", HANDLE])
        .env("HOME", &root)
        .env("FNO_STATE_DIR", &root)
        .env("FNO_BUS_DIR", &bus)
        .env("FNO_AGENT_BIN", root.join("no-such-fno-agent"))
        .env_remove("FNO_AGENT_SELF")
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|child| child.wait_with_output())
        .expect("fno agent mail sent runs");
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut got: Value = serde_json::from_slice(&out.stdout).expect("json rows");
    for r in got.as_array_mut().expect("array") {
        r["ts"] = json!("TS");
    }
    let golden = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../fno-agent/tests/golden/mail_sent/claimed_by_cursor.out"),
    )
    .expect("golden");
    let want: Value = serde_json::from_str(&golden).expect("golden json");
    assert_eq!(got, want);
}
