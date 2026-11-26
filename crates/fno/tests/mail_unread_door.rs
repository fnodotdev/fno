//! `fno agent mail unread` answers in process: with no `fno-agent` binary on
//! the path, the front door still prints the listing fno-agent's own leaf
//! froze in `tests/golden/mail_unread`, byte for byte.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BUS_LOG: &str = r#"{"id":"m1","thread":"t1","from":"lead","to":"worker","kind":"send","body":"before cursor","ts":"2026-10-12T09:00:00Z"}
{"id":"m2","thread":"t1","from":"lead","to":"worker","kind":"send","body":"at cursor","ts":"2026-10-12T09:05:00Z"}
{"id":"m3","thread":"t1","from":"lead","to":"other","kind":"send","body":"not mine","ts":"2026-10-12T09:06:00Z"}
{"id":"m4","thread":"t1","from":"lead","to":"worker","kind":"withdraw","body":"","ts":"2026-10-12T09:07:00Z","meta":{"withdraws":"m5"}}
{"id":"m5","thread":"t1","from":"lead","to":"worker","kind":"send","body":"retracted","ts":"2026-10-12T09:08:00Z"}
{"id":"m6","thread":"t1","from":"lead","to":"worker","kind":"send","body":"staged twin","ts":"2026-10-12T09:09:00Z"}
{"id":"m6","thread":"t1","from":"lead","to":"worker","kind":"send","delivery":"hosted","body":"hosted receipt","ts":"2026-10-12T09:10:00Z"}
{"id":"m7","thread":"t1","from":"lead","to":"worker","kind":"send","delivery":"typed","body":"typed delivery","ts":"2026-10-12T09:11:00Z"}
{"id":"m8","from":"lead","to":"worker","kind":"send","body":"fresh café"}
{"id":"m9","thread":"t1","from":"lead","to":"worker","kind":"send","body":"first copy wins","ts":"2026-10-12T09:12:00Z"}
{"id":"m9","thread":"t1","from":"lead","to":"worker","kind":"send","body":"second copy dropped","ts":"2026-10-12T09:13:00Z"}
"#;

#[test]
fn front_door_lists_unread_after_the_cursor_without_fno_agent() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("fno-mail-unread-door-{}", std::process::id()));
    let bus = root.join("bus");
    std::fs::create_dir_all(bus.join("cursors")).expect("bus dir");
    std::fs::write(bus.join("messages.jsonl"), BUS_LOG).expect("bus log");
    std::fs::write(
        bus.join("cursors").join("worker.json"),
        "{\"last_seen_id\": \"m2\", \"ts\": \"2026-10-12T09:14:00Z\"}\n",
    )
    .expect("cursor");

    let out = Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(["agent", "mail", "unread", "--name", "worker"])
        .env("HOME", &root)
        .env("FNO_STATE_DIR", &root)
        .env("FNO_BUS_DIR", &bus)
        .env("FNO_AGENT_BIN", root.join("no-such-fno-agent"))
        .env_remove("FNO_AGENT_SELF")
        .current_dir(&root)
        .stdin(Stdio::null())
        .output()
        .expect("fno agent mail unread runs");
    let _ = std::fs::remove_dir_all(&root);

    let golden = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../fno-agent/tests/golden/mail_unread/unread_after_cursor.out"),
    )
    .expect("golden");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), golden);
}
