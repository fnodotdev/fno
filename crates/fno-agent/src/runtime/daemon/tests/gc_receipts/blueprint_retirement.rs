//! The blueprint retirement families: the probe-unread hold, the polled stop
//! confirmation's apply path, and the empty-planning-set basis. Split out of
//! `gc_receipts` (file budget); the shared fixtures resolve through it.

use super::*;
use super::{no_agents, quiet_transcript, stage_graph, staged_graph_home, uniform_ages};
use crate::gc_sweep::{self, GcSummary};

/// The classify-pass age: past the origin ruling's one idle hour, so the
/// row reads would-retire and the apply-time re-read is what holds it.
const CLASSIFY_AGE_S: i64 = crate::gc::SPAWN_IDLE_RETIRE_SECS + 100;

#[test]
fn a_reported_done_child_retires_after_grace_without_a_skill_or_node() {
    let (dir, home) = staged_graph_home();
    stage_graph(dir.path(), json!([]));
    state::update_registry(&home.registry_json(), |r| {
        let mut row = state::RegistryEntry::default();
        row.name = "finished-child".into();
        row.harness = Some("claude".into());
        row.harness_session_id = Some("finished-child-session".into());
        row.origin = Some("spawn".into());
        row.created_at = "2026-01-01T00:00:00Z".into();
        row.inside_leg = Some(state::InsideLegReport {
            state: state::InsideLegState::Done,
            ..Default::default()
        });
        r.entries.push(row);
    })
    .unwrap();
    let path = quiet_transcript(dir.path(), "child.jsonl", 1000);
    std::fs::write(&path, "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"screenshots saved\"}}\n").unwrap();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &crate::gc_sweep::read_graph_entries,
        &|_| Some(vec![path.clone()]),
        &uniform_ages(1000),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 1, "{summary:?}");
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
}

/// The team specimen (2026-09-14): a do-phase claude row whose transcript
/// ends mid-Edit with the badge still `working` and live. A fresh working
/// report is a turn plausibly in flight, so it blocks the tick retirement
/// even with an unanswered probe and an unchanged seq.
#[test]
fn a_live_working_report_blocks_the_tick_retirement() {
    let (_dir, home) = staged_probe_unread_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let now = chrono::Utc::now();
    crate::state::update_registry(&home.registry_json(), |r| {
        if let Some(row) = r.entries.iter_mut().find(|e| e.name == "worker-x-u1") {
            let leg = row.inside_leg.as_mut().unwrap();
            leg.state = state::InsideLegState::Working;
            leg.ttl_ms = Some(90_000);
            leg.received_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        }
    })
    .unwrap();
    let calls = std::cell::Cell::new(0u32);
    let age_seam = |entries: &[&state::RegistryEntry]| {
        let n = calls.get();
        calls.set(n + 1);
        entries
            .iter()
            .map(|e| {
                let age = if n == 0 { Some(CLASSIFY_AGE_S) } else { None };
                (crate::gc::row_handle(e), age)
            })
            .collect()
    };
    let summary = run_probe_unread_sweep(&home, &emitter, &age_seam, false, None);
    assert_eq!(summary.retired.len(), 0, "{:?}", summary.retired);
    let (id, detail) = summary
        .kept_probe_unread
        .iter()
        .find(|(id, _)| id == "worker-x-u1")
        .expect("the live working row holds");
    assert_eq!(id, "worker-x-u1");
    assert!(
        detail.contains("a live working report is on the row"),
        "{detail}"
    );
    assert!(summary.kept_active.is_empty(), "{:?}", summary.kept_active);
}

/// The AC1 fixture: one claude thread row (`pid` null is the
/// default), `inside_leg` present, staged on a done node so classification
/// answers would-retire on its quiet age.
fn staged_probe_unread_home() -> (tempfile::TempDir, AgentsHome) {
    let (dir, home) = staged_graph_home();
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-u1",
            "status": "done",
            "sessions": [{
                "phase": "execute",
                "harness": "claude",
                "session_id": "s-u1",
                "started_at": "2026-09-01T00:00:00Z",
                "ended_at": "2026-09-01T01:00:00Z",
            }],
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "worker-x-u1".into();
        e.short_id = "worker-x-u1".into();
        e.origin = Some("spawn".into());
        e.harness = Some("claude".into());
        e.harness_session_id = Some("s-u1".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        e.inside_leg = Some(state::InsideLegReport {
            state: state::InsideLegState::Done,
            seq: 4,
            reason: None,
            received_at: "2026-09-01T00:00:00Z".into(),
            ttl_ms: None,
            posture: None,
        });
        r.entries.push(e);
    })
    .unwrap();
    (dir, home)
}

/// The AC1 sweep shape: grace 900, apply mode, a stop seam that confirms,
/// and the age seam handed in per test (batch answers, single re-read
/// stages the timeout).
fn run_probe_unread_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    age_seam: &dyn Fn(&[&state::RegistryEntry]) -> std::collections::HashMap<String, Option<i64>>,
    dry_run: bool,
    surface_preview: Option<&dyn Fn(&state::RegistryEntry) -> bool>,
) -> GcSummary {
    gc_sweep::run_with_release(
        home,
        emitter,
        900,
        dry_run,
        7,
        &crate::gc_sweep::read_graph_entries,
        &|_| Some(vec![]),
        age_seam,
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::Removed,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
        false,
        None,
        surface_preview,
    )
}
