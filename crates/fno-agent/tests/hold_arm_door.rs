//! The `mail-hold --session <id> --on` door, driven at the integration
//! boundary. The mail scope caps new inline tests at zero (shrink-only),
//! so the new door's tests live here, outside the scope's path words, on
//! the pub surfaces the door itself reads and writes. The delivery-policy
//! contracts drive `gate` directly: the send lanes are its production
//! callers, and the point of the door file is that the hold policy is
//! reachable at the boundary.

use fno_agent::paths::AgentsHome;
use fno_agent::runtime::mail_hold::{gate, run_mail_hold};
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

fn seed_registry_row(name: &str, session: &str) {
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
    seed_registry_row("worker", SID);
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
    seed_registry_row("worker", SID);
    let code = run_mail_hold(&["--session".into(), OTHER.into(), "--on".into()]);
    assert_eq!(code, 3, "an unknown session refuses like --off");
    assert!(stored_hold(tmp.path(), OTHER).is_none());
}

/// The composer-interrupt replay (AC1-HP): the quiet-recovery nudge is
/// `control:` mail, and a live conversation hold now keeps it, so the
/// nudge queues instead of breaking into the chat. The arm here is the
/// bare door's default action - the exact call the mux input choke spawns
/// at Enter since the measured cause (the hook fires minutes late for a
/// queued prompt).
///
/// fixture-date-ok: 2026-11-19 names the incident this test replays; the date is never compared against now
#[test]
fn a_control_nudge_queues_under_a_live_conversation_hold() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    pin_hold_env(tmp.path());
    seed_registry_row("philip", SID);
    let code = run_mail_hold(&["--session".into(), SID.into()]);
    assert_eq!(code, 0, "the bare door arms the conversation hold");
    let nudge = "control: quiet 15 min on x-9, what blocks you? continue or emit <help>. [quiet-recovery:sid:work:nudge]";
    let v = gate(SID, Some(nudge), chrono::Utc::now());
    assert!(
        !v.deliver,
        "the nudge queues; it never breaks into the chat"
    );
    let receipt = v.receipt.as_deref().unwrap_or_default();
    assert!(
        receipt.contains("is in a conversation with the user"),
        "the receipt names the conversation, got {receipt:?}"
    );
    assert!(
        v.until.is_some(),
        "the hold carries its end so the parked line names when it runs"
    );
}

/// AC2-EDGE: a lead's `control:` hand-off order delivers live past a hold
/// the user set, and the receipt says so. The plain-body arm proves the
/// pass is the directive, not a broken gate.
#[test]
fn a_control_hand_off_order_passes_a_user_hold_live() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    pin_hold_env(tmp.path());
    seed_registry_row("worker", SID);
    let code = run_mail_hold(&["--session".into(), SID.into(), "--on".into()]);
    assert_eq!(code, 0, "--on arms the user's DND");
    let order = "control: take x-9: I am stepping down at term end.";
    let v = gate(SID, Some(order), chrono::Utc::now());
    assert!(
        v.deliver && v.pass == Some("control"),
        "the order delivers live"
    );
    let plain = "picking up the thread from the retro";
    let v = gate(SID, Some(plain), chrono::Utc::now());
    assert!(!v.deliver, "a plain body still queues behind the user DND");
    let receipt = v.receipt.as_deref().unwrap_or_default();
    assert!(
        receipt.contains("do-not-disturb"),
        "the receipt names the DND, got {receipt:?}"
    );
}

/// The own-send pass outranks the conversation hold: a session's own
/// `control:` self-send reaches itself even mid-chat (the dnd skill's
/// "you can always reach yourself"). The stamp is the deterministic half
/// of the identity resolver, read from the env the pane lanes stamp.
#[test]
fn an_own_control_self_send_passes_a_conversation_hold() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    pin_hold_env(tmp.path());
    seed_registry_row("philip", SID);
    let code = run_mail_hold(&["--session".into(), SID.into()]);
    assert_eq!(code, 0);
    std::env::set_var("FNO_HARNESS_NAME", "claude");
    std::env::set_var("FNO_HARNESS_SESSION_ID", SID);
    let v = gate(
        SID,
        Some("control: release the budget clock now"),
        chrono::Utc::now(),
    );
    assert!(v.deliver && v.pass == Some("own"), "own passes everything");
    // The negative arm needs the stamp gone: with it set, any body this
    // process sends is its own, prose included. Un-stamped, plain prose
    // from another sender queues.
    std::env::remove_var("FNO_HARNESS_NAME");
    std::env::remove_var("FNO_HARNESS_SESSION_ID");
    let v = gate(SID, Some("plain prose to myself"), chrono::Utc::now());
    assert!(
        !v.deliver,
        "without the stamp, another sender's prose queues mid-conversation"
    );
}
