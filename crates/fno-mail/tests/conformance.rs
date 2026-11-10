//! Pins SPEC.md sections through the public API of the fno-mail crate.
//! Moved from crates/fno-agents/tests/mail_conformance.rs; the door half stayed there.

// ---------------------------------------------------------------------------
// The public API (SPEC.md sections: envelope, threads, addresses,
// states, log, lanes)
// ---------------------------------------------------------------------------

use fno_mail::address::Address;
use fno_mail::envelope::{is_readable, tag, thread_root, Envelope, ENVELOPE_VERSION};
use fno_mail::lane::{LaneKind, Target, LADDER};
use fno_mail::log::{read_bus_segments_reported, resolve_log_path};
use fno_mail::state::{newest_state, DeliveryState};
use serde_json::json;

/// Envelope: field order is byte order, an empty tag is absent, `v` leads.
#[test]
fn envelope_bytes_follow_the_field_order() {
    let mut env = Envelope::new(
        "fmail-aaaaaaaaaaaa",
        "2026-11-03T10:00:00Z",
        "fmail-aaaaaaaaaaaa",
        "lead",
        "worker",
        "send",
        "hello",
    );
    env.from_harness = tag(Some("claude"));
    env.in_reply_to = tag(Some(""));
    env.subject = tag(Some("S"));
    env.word_count = Some(1);
    env.meta.insert("owner".into(), json!("lead"));
    assert_eq!(ENVELOPE_VERSION, 1);
    assert_eq!(
        serde_json::to_string(&env.into_value()).unwrap(),
        r#"{"v":1,"id":"fmail-aaaaaaaaaaaa","ts":"2026-11-03T10:00:00Z","thread":"fmail-aaaaaaaaaaaa","from":"lead","to":"worker","kind":"send","from_harness":"claude","subject":"S","word_count":1,"meta":{"owner":"lead"},"body":"hello"}"#
    );
}

/// Envelope and log: a row with no `v` reads as 1; a newer `v` and a
/// malformed line each skip with a warning that names the segment and line.
#[test]
fn the_reader_skips_what_it_cannot_read_and_names_where() {
    assert!(is_readable(&json!({"id": "a"})));
    assert!(is_readable(&json!({"v": 1})));
    assert!(!is_readable(&json!({"v": 2})));
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("messages.jsonl");
    std::fs::write(
        &live,
        "{\"id\":\"a\",\"v\":1}\nnot json\n{\"id\":\"b\",\"v\":2}\n{\"id\":\"c\"}\n",
    )
    .unwrap();
    let (rows, warnings) = read_bus_segments_reported(&live);
    let ids: Vec<&str> = rows.iter().map(|r| r.row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a", "c"]);
    assert_eq!(rows[1].line, 4);
    assert_eq!(
        warnings,
        [
            "bus log: skipping malformed line messages.jsonl:2 (JSONDecodeError)",
            "bus log: skipping row messages.jsonl:3 (v=2 is newer than supported v=1)",
        ]
    );
    // The ladder: FNO_BUS_DIR, then config, then the table's bus folder.
    let default_log = std::path::PathBuf::from("/r/db/mail/bus/messages.jsonl");
    let cfg = std::path::Path::new("/cfg");
    assert_eq!(
        resolve_log_path(&default_log, Some("/e"), Some(cfg)),
        std::path::PathBuf::from("/e/messages.jsonl")
    );
    assert_eq!(
        resolve_log_path(&default_log, None, Some(cfg)),
        std::path::PathBuf::from("/cfg/messages.jsonl")
    );
    assert_eq!(resolve_log_path(&default_log, None, None), default_log);
}

/// Threads: the root is one level. A reply to a reply carries the same root.
#[test]
fn a_reply_to_a_reply_keeps_the_first_root() {
    let a = "fmail-aaaaaaaaaaaa";
    let b_thread = thread_root(None, a);
    assert_eq!(b_thread, a);
    let c_thread = thread_root(Some(&b_thread), "fmail-bbbbbbbbbbbb");
    assert_eq!(c_thread, a);
}

/// Addresses: a handle, `name@host` with the host normalized, and two `@` refuse.
#[test]
fn address_forms_parse_to_the_table() {
    assert_eq!(
        Address::parse_target("quill").unwrap(),
        Address::Handle("quill".into())
    );
    assert_eq!(
        Address::parse_target("quill@mac2.local").unwrap(),
        Address::Remote {
            name: "quill".into(),
            host: "mac2".into()
        }
    );
    let err = Address::parse_target("a@b@c").unwrap_err().to_string();
    assert!(err.contains("the form is <name>@<host>, one @"), "{err}");
}

/// States: an acceptance is never lowered; with no state row a legacy
/// `delivery` field and a `landed` row each imply one.
#[test]
fn the_state_fold_never_lowers_an_acceptance() {
    let msg = json!({"id": "m1", "kind": "send", "delivery": "typed"});
    assert_eq!(newest_state(&[msg.clone()], "m1"), DeliveryState::Injected);
    let state = |s: &DeliveryState| {
        let mut meta = s.to_json();
        meta["of"] = json!("m1");
        json!({"id": "s", "kind": "state", "meta": meta})
    };
    let accepted = DeliveryState::accepted(false, Default::default());
    let rows = vec![
        msg,
        state(&accepted),
        state(&DeliveryState::failed("io-error")),
    ];
    assert_eq!(newest_state(&rows, "m1"), accepted);
    assert_eq!(
        newest_state(&[json!({"id": "m2", "kind": "send"})], "m2"),
        DeliveryState::Queued
    );
}

/// Lanes: the first lane in the ladder that serves the target carries the send.
#[test]
fn the_lane_ladder_picks_one_lane_in_order() {
    let pick = |t: &Target| LADDER.iter().copied().find(|k| k.applies(t));
    let base = Target {
        harness: "claude".into(),
        session_id: Some("s1".into()),
        ..Target::default()
    };
    assert_eq!(pick(&base), Some(LaneKind::ClaudeSwitchboard));
    let pane = Target {
        has_mux: true,
        codex_thread: true,
        ..base.clone()
    };
    assert_eq!(pick(&pane), Some(LaneKind::Pane));
    let codex = Target {
        harness: "codex".into(),
        codex_thread: true,
        ..base.clone()
    };
    assert_eq!(pick(&codex), Some(LaneKind::CodexThread));
    let keeper = Target {
        keeper_thread: true,
        ..base.clone()
    };
    assert_eq!(pick(&keeper), Some(LaneKind::Keeper));
    // A keeper row with no session id falls through to the harness lanes.
    let keeper_no_session = Target {
        session_id: None,
        ..keeper
    };
    assert_eq!(pick(&keeper_no_session), Some(LaneKind::ClaudeSwitchboard));
    let acp = Target {
        harness: "gemini".into(),
        ..base
    };
    assert_eq!(pick(&acp), Some(LaneKind::DaemonDeliver));
}
