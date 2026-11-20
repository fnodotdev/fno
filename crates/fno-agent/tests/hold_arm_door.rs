//! The `mail-hold --session <id> --on` door, driven at the integration
//! boundary. The mail scope caps new inline tests at zero (shrink-only),
//! so the new door's tests live here, outside the scope's path words, on
//! the pub surfaces the door itself reads and writes.

use fno_agent::paths::AgentsHome;
use fno_agent::runtime::mail_hold::run_mail_hold;
use fno_agent::runtime::state_layout::place_dir;
use fno_mail::hold::{HoldStore, SetBy};
use fno_mail::session::SessionId;
use std::sync::Mutex;

/// One test file is one process: the env pinning below is process-global,
/// so the tests serialize on this lock instead of racing each other.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The hold fixture's env, mirrored from mail_hold.rs `with_hold_env`:
/// the registry, hold store and chats under one tempdir, the global
/// settings pinned away so `mail.hold.max_minutes` reads its default,
/// and the timer seam pointed at `true` so no detached timer escapes.
fn pin_hold_env(home: &std::path::Path) {
    std::env::set_var("FNO_AGENT_HOME", home);
    std::env::set_var("FNO_HOME", home);
    std::env::set_var("FNO_STATE_DIR", home);
    std::env::set_var("FNO_BIN", "true");
    std::env::set_var("FNO_AGENT_TIMER_EXE", "true");
    std::env::set_var(
        "FNO_GLOBAL_SETTINGS_PATH",
        home.join("no-global-settings.toml"),
    );
    std::env::remove_var("FNO_AGENT_ANSWER_MINUTES");
}

fn seed_registry_row(home: &std::path::Path, name: &str, session: &str) {
    let row = serde_json::json!({
        "name": name, "status": "live", "cwd": "/repo", "harness": "claude",
        "harness_session_id": session,
        "created_at": "2026-09-26T00:00:00Z",
    });
    let doc = serde_json::json!({"schema_version": 1, "agents": [row]});
    fno_agent::runtime::registry_store::seed_raw(
        AgentsHome::shared_registry_json(),
        doc.to_string(),
    );
}

fn stored_hold(home: &std::path::Path, session: &str) -> Option<fno_mail::hold::Hold> {
    let dir = place_dir(home, "mail-hold").unwrap_or_else(|| home.join("mail-hold"));
    HoldStore::new(dir).get(&SessionId::parse(session).unwrap())
}

const SID: &str = "cccccccc-1111-2222-3333-444455556666";
const OTHER: &str = "dddddddd-1111-2222-3333-444455556666";

#[test]
fn the_arm_door_writes_a_user_hold_at_the_ceiling() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    pin_hold_env(tmp.path());
    seed_registry_row(tmp.path(), "worker", SID);
    let code = run_mail_hold(&["--session".into(), SID.into(), "--on".into()]);
    assert_eq!(code, 0, "--on exits 0 on a known session");
    let hold = stored_hold(tmp.path(), SID).expect("the arm wrote a hold");
    assert_eq!(
        hold.set_by(),
        SetBy::User,
        "the arm is the user's, never the conversation's"
    );
    let minutes = (hold.until() - chrono::Utc::now()).num_minutes();
    assert!(
        (468..=481).contains(&minutes),
        "the ceiling is the 480 minute default, read {minutes}m"
    );
}

#[test]
fn the_arm_door_refuses_an_unknown_session_and_writes_nothing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    pin_hold_env(tmp.path());
    seed_registry_row(tmp.path(), "worker", SID);
    let code = run_mail_hold(&["--session".into(), OTHER.into(), "--on".into()]);
    assert_eq!(code, 3, "an unknown session refuses like --off");
    assert!(stored_hold(tmp.path(), OTHER).is_none());
}
