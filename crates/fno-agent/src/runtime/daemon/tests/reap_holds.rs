//! The x-e3cc test families: the hold clock on every reaper hold, the
//! release command's rulings and refusals. Same-module helpers resolve through
//! the parent glob; gc_receipts' fixtures are `pub(super)`.

use super::gc_receipts::*;
use super::*;
use crate::gc_sweep::{self, GcSummary, GraphRead};
use std::collections::HashMap;

// ── x-e3cc: every hold carries an age, a basis and an escalation ─────────

fn find_hold<'a>(summary: &'a GcSummary, id: &str) -> &'a crate::gc_sweep::Hold {
    summary
        .holds
        .iter()
        .find(|h| h.id == id)
        .unwrap_or_else(|| panic!("no hold for {id} in {:?}", summary.holds))
}

fn assert_hold_line(summary: &GcSummary, dry_run: bool, id: &str, needle: &str) {
    let text = crate::reap_render::render_reap(summary, false, dry_run);
    let line = text
        .lines()
        .find(|l| l.contains(&format!("kept {id} (")) || l.contains(&format!("held {id} (")))
        .unwrap_or_else(|| panic!("no hold line for {id} in:\n{text}"));
    assert!(line.contains(needle), "line for {id}: {line}");
}

/// AC2-HP: rows whose sources disagree, whose transcript is unresolved or
/// that carry an open do row hold nothing now. The one hold left is the
/// claude row a dry run cannot promise a stop for, and its line carries
/// an age and a basis.
#[test]
fn ac2_hp_every_hold_line_carries_an_age_and_basis() {
    let home = tmp_home("gc-ac2-hp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
    // Past the grace, inside the 30-minute done-node window, so the
    // work-state keeps still hold.
    let recent = quiet_transcript(transcripts.path(), "recent.jsonl", 1500);
    let stamp = |ago_s: i64| {
        (chrono::Utc::now() - chrono::Duration::seconds(ago_s))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    state::update_registry(&home.registry_json(), |r| {
        // sources disagree: sessions names x-aaaa, the registry field x-cccc.
        let mut conf = ask_row("conf-row", None);
        conf.short_id = "confrow".into();
        conf.node = Some("x-cccc".into());
        conf.harness_session_id = Some("sess-conf".into());
        conf.origin = Some("spawn".into());
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        conf.status = crate::AgentStatus::Idle;
        r.entries.push(conf);
        // transcript unresolved: the age seam answers nothing for this row.
        let mut tu = ask_row("tu-row", None);
        tu.short_id = "turow".into();
        tu.harness_session_id = Some("sess-tu".into());
        tu.origin = Some("spawn".into());
        tu.created_at = stamp(1500);
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        tu.status = crate::AgentStatus::Idle;
        r.entries.push(tu);
        // open do row on a done node.
        let mut odr = ask_row("odr-row", None);
        odr.short_id = "odrrow".into();
        odr.harness_session_id = Some("sess-odr".into());
        odr.origin = Some("spawn".into());
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        odr.status = crate::AgentStatus::Idle;
        r.entries.push(odr);
        // needs live stop: a claude row a dry run cannot promise a stop for.
        let nls = claude_worker_row("nls-row", "nlsrow");
        r.entries.push(nls);
    })
    .unwrap();

    let summary = evidence_sweep(
        &home,
        &emitter,
        900,
        true,
        graph_read(
            &[
                ("sess-conf", "x-aaaa", "done"),
                ("x-cccc", "x-cccc", "done"),
                ("sess-odr", "N1", "done"),
                ("sess-tu", "N2", "done"),
                ("nlsrow-1111-2222-3333-444444444444", "N3", "done"),
            ],
            &[("sess-odr", "N1")],
        ),
        &|e| match e.harness_session_id.as_deref() {
            Some("sess-tu") => None,
            Some("sess-conf") | Some("sess-odr") => Some(vec![recent.clone()]),
            _ => Some(vec![quiet.clone()]),
        },
        no_agents(),
        &|_| false,
    );

    let ids: Vec<&str> = summary.holds.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["nlsrow"], "holds: {:?}", summary.holds);
    let h = find_hold(&summary, "nlsrow");
    assert_eq!(h.reason, "needs live stop", "{h:?}");
    assert!(h.age_s.is_some(), "{h:?}");
    assert_eq!(h.age_basis, "transcript quiet", "{h:?}");
    assert_hold_line(&summary, true, "nlsrow", "[held ");
}

// ── x-e3cc change 3: the release command applies a ruling through the
// sweep's own door ────────────────────────────────────────────────────────

/// AC3-EDGE, the changed hold: a release whose captured hold no longer
/// matches the row's current hold keeps the row and names the change.
#[test]
fn ac3_edge_a_changed_hold_refuses_the_release() {
    let home = tmp_home("gc-ac3-changed");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("chg-row", None);
        row.short_id = "chgrow".into();
        row.harness_session_id = Some("sess-chg".into());
        row.origin = Some("spawn".into());
        // Inside the 30-minute done-node window, so the row holds as
        // transcript unresolved.
        row.created_at = (chrono::Utc::now() - chrono::Duration::seconds(1500))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        r.entries.push(row);
    })
    .unwrap();

    let graph = graph_read(&[("sess-chg", "N1", "done")], &[]);
    let release = gc_sweep::Release {
        handle: "chgrow".to_string(),
        reason: "sources disagree".to_string(),
        detail: "sessions x-aaaa vs registry x-cccc".to_string(),
    };
    let summary = gc_sweep::run_with_release(
        &home,
        &emitter,
        900,
        true,
        7,
        &move |_| graph.clone(),
        &|_| None,
        &staged_ages_at(crate::daemon::now_epoch_secs(), &|_| None),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
        false,
        Some(&release),
        None,
    );

    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert!(
        summary
            .release_refused
            .iter()
            .any(|line| line.contains("release refused: hold changed from sources disagree")),
        "{:?}",
        summary.release_refused
    );
    let text = crate::reap_render::render_reap(&summary, false, true);
    assert!(
        text.contains("release refused: hold changed"),
        "text: {text}"
    );
}

// ── the open-PR keep through the sweep ──────────────────────────────────────

/// A stopped claude spawn row whose session has a do row on an in_review
/// node carrying pr_number 1943 (merge_status unrecorded): the sweep keeps
/// the row under `open pr`, projects a hold with a clock, and exposes the
/// nudge-ladder row. A peer without a do row on the node changes nothing.
#[test]
fn ac1_hp_open_pr_keep_survives_a_terminal_state_through_the_sweep() {
    let home = tmp_home("gc-open-pr");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    // Past the grace, so the keep asks the PR; inside the idle hour, so the
    // keep still holds.
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 1800);
    state::update_registry(&home.registry_json(), |r| {
        let mut row = claude_worker_row("pr-row", "cccc9999");
        row.origin = Some("spawn".into());
        r.entries.push(row);
    })
    .unwrap();
    let sid = "cccc9999-1111-2222-3333-444444444444";
    let graph = Some(GraphRead {
        index: HashMap::from([(
            sid.to_string(),
            vec![("x-node".to_string(), "in_review".to_string())],
        )]),
        work_index: HashMap::from([(
            sid.to_string(),
            vec![("x-node".to_string(), "in_review".to_string())],
        )]),
        statuses: HashMap::from([("x-node".to_string(), "in_review".to_string())]),
        pr_state: HashMap::from([("x-node".to_string(), (None, 0, 0))]),
        pr_number: HashMap::from([("x-node".to_string(), Some(1943))]),
        do_nodes: HashMap::from([(
            sid.to_string(),
            std::collections::HashSet::from(["x-node".to_string()]),
        )]),
        // The row is quiet past the grace, so the keep asks the PR: stage
        // the open answer the sweep must read (no test touches the network).
        pr_reads: HashMap::from([("/tmp".to_string(), 1943u64)])
            .into_iter()
            .map(|(cwd, pr)| ((cwd, pr), Some(true)))
            .collect(),
        ..Default::default()
    });
    // The roster reads stopped: the exact state that reaped seven rows on
    // 2026-09-13 before the keep existed.
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("cccc9999", Some("stopped")),
    ]);
    let summary = evidence_sweep(
        &home,
        &emitter,
        900,
        false,
        graph,
        &|_| Some(vec![quiet.clone()]),
        agents,
        &|_| true,
    );
    // The row handle is the short id when one is recorded.
    assert_eq!(
        summary.kept_open_pr,
        vec![("cccc9999".to_string(), "x-node".to_string())],
        "kept buckets: {summary:?}"
    );
    let hold = find_hold(&summary, "cccc9999");
    assert_eq!(hold.reason, "open pr");
    assert_eq!(hold.detail, "x-node #1943");
    assert_eq!(summary.open_pr_rows.len(), 1, "{:?}", summary.open_pr_rows);
    let ladder_row = &summary.open_pr_rows[0];
    assert_eq!(ladder_row.node, "x-node");
    assert_eq!(ladder_row.pr, Some(1943));
    assert_eq!(ladder_row.session_id, sid);
    // A stopped roster state reads as not live: the ladder's resume arm.
    assert!(!ladder_row.live);
    assert!(!ladder_row.busy, "a stopped row is never mid-turn");
    assert_eq!(summary.retired, vec![], "nothing retires");
    std::fs::remove_dir_all(home.root()).ok();
}

// ── the dead-worker keep (dead open work) through the sweep ────────────────

// ── the open-PR keep asks the PR ─────────────────────────────────────────

// ── an adopted row keeps only while there is a session to own it ────────

// ── a row that resolved no node releases on its own done report ─────────

// ── the receipt names a registry it cannot write ────────────────────────
