//! The retirement sweep, reap-receipt gate and plan_reconcile test
//! families. Shared helpers (`tmp_home`, `ask_row`, `rentry`, `civil`, ...)
//! stay in the parent tests module and resolve through the glob.
use super::*;
use crate::daemon::claude_stop::{no_transport_row_gone, stop_claude_pid_confirmed};

use crate::gc_sweep::{self, GcSummary, GraphRead};
use crate::quiet_retire::no_live_worker;

// ── x-c672: the retirement sweep, keyed by the reverse join ─────────────

/// A transcript file untouched for `age_secs`, so the activity read answers
/// a positive quiet past the grace.
pub(super) fn quiet_transcript(
    dir: &std::path::Path,
    name: &str,
    age_secs: i64,
) -> std::path::PathBuf {
    use std::io::Write;
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{{}}").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mtime = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs((now - age_secs).max(0) as u64);
    f.set_modified(mtime).unwrap();
    path
}

/// The test age seam (x-54cf): the sweep reads ages through the injected
/// batch seam, and the fixtures answer from the SAME staged transcript files
/// the old stat read - the seam is what changed, not the fixture ages. `now`
/// is injected so a dry pass and its acting pass judge one clock and their
/// ages agree byte for byte; wall-clock call sites pass
/// `crate::daemon::now_epoch_secs()`.
pub(super) fn staged_ages_at(
    now: i64,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
) -> impl Fn(&[&state::RegistryEntry]) -> std::collections::HashMap<String, Option<i64>> + '_ {
    move |entries| {
        entries
            .iter()
            .map(|e| {
                let age = transcripts(e).and_then(|hits| {
                    hits.iter()
                        .filter_map(|p| {
                            let secs = std::fs::metadata(p).ok()?.modified().ok()?;
                            Some(secs.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
                        })
                        .max()
                });
                (
                    crate::gc::row_handle(e),
                    age.map(|newest| now.saturating_sub(newest)),
                )
            })
            .collect()
    }
}

/// The single-fixture age seam: one staged age answers every row the sweep
/// probes, the shape most call sites stage (`quiet_transcript(..., 2*3600)`).
pub(super) fn uniform_ages(
    age_s: i64,
) -> impl Fn(&[&state::RegistryEntry]) -> std::collections::HashMap<String, Option<i64>> {
    move |entries| {
        entries
            .iter()
            .map(|e| (crate::gc::row_handle(e), Some(age_s)))
            .collect()
    }
}

/// Build the injected graph seam from `(session, node, status)` triples and
/// `(session, node)` open-do pairs.
pub(super) fn graph_read(
    named: &[(&str, &str, &str)],
    open_do: &[(&str, &str)],
) -> Option<GraphRead> {
    let mut index: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    let mut statuses: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut pr_state: std::collections::HashMap<String, (Option<String>, usize, usize)> =
        std::collections::HashMap::new();
    for (sid, node, status) in named {
        index
            .entry(sid.to_ascii_lowercase())
            .or_default()
            .push((node.to_string(), status.to_string()));
        statuses.insert(node.to_string(), status.to_string());
        pr_state.insert(node.to_string(), (None, 0, 0));
    }
    let mut open: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for (slurp, node) in open_do {
        open.entry(slurp.to_ascii_lowercase())
            .or_default()
            .push(node.to_string());
    }
    Some(GraphRead {
        work_index: index.clone(),
        index,
        open_do: open,
        phases: std::collections::HashMap::new(),
        closed_planning: std::collections::HashMap::new(),
        plan_written: std::collections::HashMap::new(),
        statuses,
        pr_state,
        pr_number: std::collections::HashMap::default(),
        plan_paths: std::collections::HashMap::default(),
        delivery_merged_refs: std::collections::HashMap::default(),
        do_nodes: std::collections::HashMap::default(),
        pr_reads: std::collections::HashMap::default(),
    })
}

/// A retirement-shaped sweep for the dispatch-accounting tests: every row
/// named on a done node, its staged transcript quiet (grace 0), stop
/// confirmed, no tree.

pub(super) fn no_agents() -> crate::claude_roster::ClaudeAgentsSnapshot {
    crate::claude_roster::ClaudeAgentsSnapshot::unknown("test: no snapshot staged")
}

fn retire_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    named: &[(&str, &str, &str)],
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
) -> GcSummary {
    let graph = graph_read(named, &[]);
    gc_sweep::run(
        home,
        emitter,
        0,
        false,
        7,
        &move |_| graph.clone(),
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    )
}

/// The sweep against staged seams: graph as staged, transcripts as staged,
/// stop always confirmed, tree probes staged per name, prune recorded.
fn staged_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    graph: Option<GraphRead>,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    trees: &dyn Fn(&str) -> (Option<bool>, Option<bool>),
) -> GcSummary {
    gc_sweep::run(
        home,
        emitter,
        grace_secs,
        false,
        7,
        &move |_| graph.clone(),
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|e| trees(&e.name),
        &|e| Some(crate::daemon::PruneOutcome::Removed(e.cwd.clone())),
    )
}

/// A clean-and-merged worktree the callback could not remove reads as a
/// prune ATTEMPT, not a prune SUCCESS: `Kept` never populates `pruned`, and
/// `None` (no linked worktree) reads as its own reason. Neither ever
/// duplicates into the other bucket.
#[test]
fn a_prune_that_did_not_confirm_removal_is_never_reported_pruned() {
    let home = tmp_home("gc-prune-outcome");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "p.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut p = ask_row("row-p", None);
        p.short_id = "rowp".into();
        p.harness_session_id = Some("sess-p".into());
        p.origin = Some("spawn".into());
        p.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path()).unwrap();
        std::fs::write(wt.path().join(".git"), "gitdir: /x/worktrees/p\n").unwrap();
        p.cwd = wt.keep().to_string_lossy().into_owned();
        r.entries.push(p);
    })
    .unwrap();
    let graph = graph_read(&[("sess-p", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| {
            Some(crate::daemon::PruneOutcome::Kept(
                "git worktree remove failed: simulated".into(),
            ))
        },
    );
    assert_eq!(
        summary.retired,
        vec![(
            "rowp".to_string(),
            "every named node done: N1 (via sessions; merge_status: N1:unrecorded)".to_string()
        )]
    );
    assert!(summary.pruned.is_empty(), "{:?}", summary.pruned);
    assert_eq!(
        summary.prune_failed,
        vec![(
            "rowp".to_string(),
            "git worktree remove failed: simulated".to_string()
        )]
    );
}

/// A shared worktree cwd: one row retires, the other is still live on it -
/// the tree survives and the retiring row names the row still holding it.
#[test]
fn a_shared_worktree_survives_while_the_other_row_is_live() {
    let home = tmp_home("gc-shared-tree-one-live");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "x.jsonl", 2 * 3600);
    let wt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(wt.path()).unwrap();
    std::fs::write(wt.path().join(".git"), "gitdir: /x/worktrees/shared\n").unwrap();
    let shared_cwd = wt.keep().to_string_lossy().into_owned();
    state::update_registry(&home.registry_json(), |r| {
        let mut x = ask_row("row-x", None);
        x.short_id = "rowx".into();
        x.harness_session_id = Some("sess-x".into());
        x.origin = Some("spawn".into());
        x.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        x.cwd = shared_cwd.clone();
        r.entries.push(x);
        // row-y stays: named on an open node, still live on the same tree.
        let mut y = ask_row("row-y", None);
        y.short_id = "rowy".into();
        y.harness_session_id = Some("sess-y".into());
        y.origin = Some("spawn".into());
        y.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        y.cwd = shared_cwd.clone();
        r.entries.push(y);
    })
    .unwrap();
    let graph = graph_read(
        &[("sess-x", "N1", "done"), ("sess-y", "N2", "in_review")],
        &[],
    );
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        // The ruling's hour splits the pair: the done row is past it, the
        // live row sits inside it, so only the done row retires.
        &|entries| {
            entries
                .iter()
                .map(|e| {
                    let age = if e.short_id == "rowy" { 1800 } else { 2 * 3600 };
                    (crate::gc::row_handle(e), Some(age))
                })
                .collect()
        },
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| {
            Some(crate::daemon::PruneOutcome::Removed(
                "must not be called".into(),
            ))
        },
    );
    assert_eq!(
        summary.retired,
        vec![(
            "rowx".to_string(),
            "every named node done: N1 (via sessions; merge_status: N1:unrecorded)".to_string()
        )]
    );
    assert!(summary.pruned.is_empty(), "{:?}", summary.pruned);
    assert_eq!(
        summary.kept_shared_tree,
        vec![("rowx".to_string(), "rowy".to_string())]
    );
}

/// A parent row named as `spawned_by_session` by a live CHILD child (a
/// joiner-named row) is never retired, and no active-surface removal ever
/// reaches it: `surface_removal` panics if the sweep calls it.
#[test]
fn a_parent_with_a_live_descendant_is_kept_and_never_touched() {
    let home = tmp_home("gc-lineage-live-child");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "parent.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut parent = ask_row("row-parent", None);
        parent.short_id = "rowparent".into();
        parent.harness_session_id = Some("sess-parent".into());
        parent.origin = Some("spawn".into());
        r.entries.push(parent);
        let mut child = ask_row("jn-t-row-child", None);
        child.short_id = "rowchild".into();
        child.harness_session_id = Some("sess-child".into());
        child.spawned_by_session = Some("sess-parent".into());
        child.status = crate::AgentStatus::Busy;
        r.entries.push(child);
    })
    .unwrap();
    let graph = graph_read(&[("sess-parent", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| panic!("active-surface removal must never run on a held parent"),
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert_eq!(
        summary.kept_live_descendants,
        vec![("rowparent".to_string(), "rowchild".to_string())]
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "row-parent"));
}

/// AC4-ERR, both arms: an unreadable graph keeps every row (never a
/// retirement on a failed read), and a stop that does not confirm keeps
/// the row under `stop_refused`.
#[test]
fn ac4_err_graph_unreadable_and_stop_refusal_keep_every_row() {
    let home = tmp_home("gc-ac4-err");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |r| {
        for name in ["row-a", "row-b"] {
            let mut e = ask_row(name, None);
            e.harness_session_id = Some(format!("sess-{name}"));
            e.origin = Some("spawn".into());
            // The verdict door reads the stored status first; these scenarios vary
            // the vendor word, so the row itself must be fno-undecided.
            e.status = crate::AgentStatus::Idle;
            r.entries.push(e);
        }
    })
    .unwrap();

    // Graph unreadable: every row keeps under that one reason.
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &|_| None,
        &|_| None,
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty());
    assert_eq!(
        summary.kept_graph_unreadable,
        vec!["row-a".to_string(), "row-b".to_string()]
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(reg.entries.len(), 2);

    // Stop refused: the row earned its retirement but the process could
    // not be confirmed stopped, so it stays for the next tick.
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "q.jsonl", 2 * 3600);
    let graph = graph_read(&[("sess-row-a", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|e| e.name != "row-a",
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    // row-b names no node and sits idle past the hour, so it retires under
    // the origin ruling; row-a's refused stop keeps it.
    assert_eq!(
        summary
            .retired
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["row-b"]
    );
    assert_eq!(
        summary
            .stop_refused
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        vec!["row-a".to_string()]
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "row-a"));
}

// ── x-b150: the reap receipt gate ────────────────────────────────────────

/// x-70e1 AC6: the active-surface removal is a REAL effect of the sweep. A
/// typed outcome that does not confirm (Failed) HOLDS the row for retry, and
/// a confirming outcome is recorded on the staged receipt as an
/// `active-surface` effect - a partial retirement is never readable as a
/// full one.
#[test]
fn a_failed_surface_removal_holds_the_row_and_a_confirmation_is_recorded() {
    use crate::daemon::CascadeOutcome;

    let home = tmp_home("gc-surface-hold");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let quiet = quiet_transcript(&dir, "q.jsonl", 2 * 3600);
    let graph = graph_read(&[("s-hold", "N1", "done")], &[]);
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "holdw".into();
        e.short_id = "holdw".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-hold".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();

    // First pass: the surface removal FAILS. The row holds; nothing drops.
    let refusing =
        |_e: &state::RegistryEntry| CascadeOutcome::Failed("claude rm timed out".to_string());
    let q1 = quiet.clone();
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![q1.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &refusing,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert!(
        summary
            .stop_refused
            .iter()
            .any(|(id, reason)| id == "holdw" && reason.contains("active-surface")),
        "{:?}",
        summary.stop_refused
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .any(|e| e.name == "holdw"),
        "a refused surface removal holds the row"
    );

    // Second pass: the removal confirms. The row retires and the receipt on
    // disk carries the active-surface effect record. The staged values are
    // re-cloned for the second closure set.
    let confirming = |e: &state::RegistryEntry| {
        let _ = e;
        CascadeOutcome::Removed
    };
    let graph = graph_read(&[("s-hold", "N1", "done")], &[]);
    let quiet2 = quiet.clone();
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet2.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &confirming,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 1, "{:?}", summary.retired);
    let receipts: Vec<String> = std::fs::read_dir(home.root().join("reap-receipts"))
        .unwrap()
        .flatten()
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    assert!(!receipts.is_empty(), "a receipt was staged");
    let body = std::fs::read_to_string(&receipts[0]).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let effects = value["effects"].as_array().expect("effects recorded");
    let by_op: std::collections::BTreeMap<String, String> = effects
        .iter()
        .map(|e| {
            (
                e["op"].as_str().unwrap_or_default().to_string(),
                e["outcome"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        by_op.get("native-stop").map(String::as_str),
        Some("confirmed-removed"),
        "the confirmed stop is named before the surface removal: {by_op:?}"
    );
    assert_eq!(
        by_op.get("active-surface").map(String::as_str),
        Some("confirmed-removed"),
        "the native active-surface outcome must be named: {by_op:?}"
    );
    assert!(
        by_op.contains_key("resume-evidence"),
        "the resumability evidence op must be present: {by_op:?}"
    );
}

/// x-5aef AC3-EDGE: a row whose receipt cannot be built refuses BEFORE any
/// effect fires - no stop seam call, no surface seam call, no receipt on
/// disk, row kept under kept_no_receipt.
#[test]
fn a_row_without_a_buildable_receipt_refuses_before_any_effect() {
    use crate::daemon::CascadeOutcome;

    let home = tmp_home("gc-no-receipt-refuses-first");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let quiet = quiet_transcript(&dir, "q.jsonl", 2 * 3600);
    let graph = graph_read(&[("s-norc", "N1", "done")], &[]);
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "norcw".into();
        e.short_id = "norcw".into();
        e.origin = Some("spawn".into());
        // A harness with no capability row builds no receipt: the Unknown
        // case in build_reap_receipt's gate.
        e.harness = Some("harness-no-contract".into());
        e.harness_session_id = Some("s-norc".into());
        e.created_at = "2026-01-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let stop_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let surface_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let stop_for_seam = std::rc::Rc::clone(&stop_calls);
    let surface_for_seam = std::rc::Rc::clone(&surface_calls);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &move |_| {
            stop_for_seam.set(stop_for_seam.get() + 1);
            true
        },
        &move |_| {
            surface_for_seam.set(surface_for_seam.get() + 1);
            CascadeOutcome::Removed
        },
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert_eq!(
        stop_calls.get(),
        0,
        "no effect may fire when the receipt cannot be built"
    );
    assert_eq!(
        surface_calls.get(),
        0,
        "no effect may fire when the receipt cannot be built"
    );
    assert!(
        summary.kept_no_receipt.iter().any(|(id, _)| id == "norcw"),
        "{:?}",
        summary.kept_no_receipt
    );
    assert!(
        !home.root().join("reap-receipts").exists()
            || std::fs::read_dir(home.root().join("reap-receipts"))
                .map(|entries| entries.count() == 0)
                .unwrap_or(true),
        "no receipt may land for a row the gate refused before staging"
    );
}

/// Durability ordering, enforced not narrated: a receipt that cannot be
/// written holds its row in the registry. The receipts dir is pre-created
/// as a FILE, so the persist write fails and the sweep keeps the row.
#[test]
fn a_row_reaps_only_after_its_receipt_is_durable() {
    let (dir, home) = staged_graph_home();
    stage_graph(dir.path(), json!([]));
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    std::fs::write(home.root().join("reap-receipts"), b"not a directory").unwrap();
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "undeliv.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut e = ask_row("undeliverable", None);
        e.short_id = "undeliv".into();
        e.harness = Some("footnote".into());
        e.origin = Some("spawn".into());
        r.entries.push(e);
    })
    .unwrap();

    let graph = graph_read(&[("undeliverable-sess", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|_| panic!("receipt write failure must precede process stop"),
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );

    assert!(summary.retired.is_empty());
    assert!(
        summary
            .kept_no_receipt
            .iter()
            .any(|(id, reason)| id == "undeliv" && reason.contains("receipt did not persist")),
        "the persist failure must be surfaced: {:?}",
        summary.kept_no_receipt
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "undeliverable"));
}

#[test]
fn gc_sweep_restores_row_when_termination_evidence_is_unknown() {
    let sandbox = tmp_home("gc-unknown-termination");
    let home = AgentsHome::at(sandbox.root().join("agents"));
    home.ensure_root().unwrap();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let repo = home.root().join("repo");
    std::fs::create_dir_all(repo.join(".fno")).unwrap();
    assert!(crate::git_test_helpers::git_init(&repo));
    std::fs::write(
        repo.join(".fno/target-state.md"),
        "---\nfno_id: reused-run\ninput: x-other\nplan_path: \"\"\n---\n",
    )
    .unwrap();
    state::update_registry(&home.registry_json(), |registry| {
        let mut row = bg_claude_row("target-x-a35a-route-atomicity", "dead0001");
        row.status = AgentStatus::Exited;
        row.cwd = repo.to_string_lossy().into_owned();
        row.exited_at = Some("2020-01-01T00:00:00Z".into());
        row.log_path = Some(stale_log(&repo));
        // A real bg worker carries its session identity, so the receipt
        // gate stages a record and the row reaches the dispatch path this
        // test exercises.
        row.harness_session_id = Some("019cdead-0000-7000-8000-000000000001".into());
        registry.entries.push(row);
    })
    .unwrap();

    let summary = retire_sweep(
        &home,
        &emitter,
        &[("019cdead-0000-7000-8000-000000000001", "x-other", "done")],
        &|e| {
            e.log_path
                .as_deref()
                .map(|p| vec![std::path::PathBuf::from(p)])
        },
    );

    assert!(summary.retired.is_empty());
    let registry = state::load_registry(&home.registry_json()).unwrap();
    assert!(registry
        .entries
        .iter()
        .any(|row| row.name == "target-x-a35a-route-atomicity"));
    let events = read_events(&home);
    assert!(events.iter().any(|event| {
        event["type"] == "daemon_recovery_error"
            && event["data"]["op"] == "observe_dead_dispatch_termination"
    }));
}

#[tokio::test]
async fn lifecycle_name_resolution_never_falls_back_on_ambiguity() {
    let mut named = rentry("deadbeef", AgentStatus::Live, None);
    named.short_id = "transport-a".into();
    named.harness_session_id = Some("aaaaaaaa-1111-2222-3333-444455556666".into());
    let mut short = rentry("other", AgentStatus::Live, None);
    short.short_id = "deadbeef".into();
    short.harness_session_id = Some("bbbbbbbb-1111-2222-3333-000000000002".into());
    let reg = crate::state::Registry {
        schema_version: crate::state::REGISTRY_SCHEMA_VERSION,
        entries: vec![named, short],
        ..Default::default()
    };

    let error = entry_for_lifecycle(
        &reg,
        "deadbeef",
        std::path::Path::new("/nonexistent/registry.json"),
        false,
    )
    .await
    .expect_err("ambiguous token must not fall back to the matching row name");

    assert!(error.contains("ambiguous across 2 agents"));
}

// ---- x-cd31: idle-exit reads live workers, not registry emptiness ------

#[test]
fn idle_exit_fires_on_terminal_rows_with_no_live_worker() {
    // The exact defect shape: a registry of TERMINAL rows (the roster an
    // established machine always has) with no live socket and no live pid
    // must let the daemon exit. Registry emptiness never held here.
    let home = short_home("idle-terminal");
    home.ensure_root().unwrap();
    state::update_registry(&home.registry_json(), |r| {
        r.entries
            .push(ask_row("done-1", Some("2020-01-01T00:00:00Z")));
        r.entries
            .push(ask_row("done-2", Some("2020-01-01T00:00:00Z")));
    })
    .unwrap();
    assert!(
        no_live_worker(&home),
        "terminal rows with dead pids and no sockets must not pin the daemon"
    );
    // The fail-safe side: an unreadable registry is an absence with two
    // explanations, and the daemon must stay resident rather than exit on
    // a transient read failure (the old code exited: unwrap_or(true)).
    std::fs::remove_dir_all(home.root()).ok();
    let home = short_home("idle-unreadable");
    home.ensure_root().unwrap();
    crate::registry_store::seed_raw(&home.registry_json(), "not json at all{");
    assert!(
        !no_live_worker(&home),
        "an unreadable registry must not license an idle exit"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test]
async fn stop_claude_pid_kills_a_real_child_and_spares_a_recycled_pid() {
    struct OwnedChild {
        process: std::sync::Arc<std::sync::Mutex<std::process::Child>>,
        reaper: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            {
                let mut child = self.process.lock().unwrap_or_else(|e| e.into_inner());
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Some(reaper) = self.reaper.take() {
                let _ = reaper.join();
            }
        }
    }
    let mut entry = ask_row("orphan", None);
    assert!(
        !stop_claude_pid_confirmed(&entry).await,
        "no pid -> nothing to stop"
    );
    // The already-exited arm reads only pid evidence here.
    entry.harness_session_id = None;
    assert!(
        !no_transport_row_gone(&entry),
        "no pid and no session -> no proof of death"
    );
    let child = std::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn owned sleeper");
    let pid = child.id();
    let process = std::sync::Arc::new(std::sync::Mutex::new(child));
    let mut guard = OwnedChild {
        process,
        reaper: None,
    };
    let reaping = std::sync::Arc::clone(&guard.process);
    guard.reaper = Some(std::thread::spawn(move || loop {
        if reaping
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait()
            .expect("reap owned sleeper")
            .is_some()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }));
    let start = process_start_time(pid).expect("owned child has a readable start token");
    let is_alive = || {
        guard
            .process
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait()
            .expect("observe owned child")
            .is_none()
    };
    assert!(is_alive(), "positive control: sleeper is running");
    entry.pid = Some(pid);
    entry.pid_start_time = None;
    assert!(
        !stop_claude_pid_confirmed(&entry).await,
        "no start token -> refuse"
    );
    assert!(is_alive(), "a refused row must not be signalled");
    entry.pid_start_time = Some(start.wrapping_add(1));
    assert!(
        !stop_claude_pid_confirmed(&entry).await,
        "recycled pid -> refuse"
    );
    assert!(is_alive(), "an unrelated incarnation must not be signalled");
    entry.pid_start_time = Some(start);
    assert!(
        !no_transport_row_gone(&entry),
        "an owned live pid is not gone"
    );
    assert!(
        stop_claude_pid_confirmed(&entry).await,
        "owned live pid -> stopped"
    );
    assert!(
        !is_alive(),
        "owned child is reaped, not merely reported gone"
    );
    assert!(
        !stop_claude_pid_confirmed(&entry).await,
        "a dead pid is not newly stopped"
    );
    assert!(
        no_transport_row_gone(&entry),
        "a reaped pid reads gone, so stop answers already exited"
    );
}

#[test]
fn recovery_does_not_quarantine_a_temp_held_by_an_active_writer() {
    let home = tmp_home("recover-active-temp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |_| {}).unwrap();
    let temp = home.root().join(".registry.json.tmp.active");
    std::fs::write(&temp, b"partial").unwrap();
    let lock_path = std::path::PathBuf::from(format!("{}.lock", home.registry_json().display()));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    lock.lock().unwrap();

    let found = crate::quarantine::quarantine_interrupted_write_temps(&home, &emitter);

    assert!(found.is_empty());
    assert!(temp.exists(), "active writer temp must remain in place");
}

// --- plan_reconcile (US6.9): tri-state, status-aware transitions, budget ---

// ── x-8739: the sweep settles a stale open do row on a settled node ──
// The settle test family, moved out of daemon.rs (shrink-only) and out of
// its own module: same-module helpers resolve directly.

/// A home whose graph path (`home.root().parent()/graph.json`) lands INSIDE
/// the test's tmpdir; `tmp_home` would make it the shared temp dir instead.
pub(super) fn staged_graph_home() -> (tempfile::TempDir, AgentsHome) {
    let dir = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(dir.path().join("agents"));
    home.ensure_root().unwrap();
    crate::paths::pin_test_claims_root(dir.path());
    (dir, home)
}

/// Seed graph.db for tests that address the stable graph path anchor.
pub(super) fn stage_graph(dir: &std::path::Path, entries: Value) {
    crate::graph_store::seed_rows(
        &dir.join("graph.json"),
        entries.as_array().expect("entry rows"),
    )
    .unwrap();
}

/// The settled-node shape: done, GitHub-confirmed merged, no additional PR.
pub(super) fn done_node(id: &str, merge_status: Value, aprs: Value, sessions: Vec<Value>) -> Value {
    json!({
        "id": id,
        "status": "done",
        "completed_at": "2026-09-01T00:00:00Z",
        "merge_status": merge_status,
        "additional_prs": aprs,
        "sessions": sessions,
    })
}

/// One open do row.
pub(super) fn open_do_row(harness: &str, sid: &str) -> Value {
    json!({
        "phase": "execute",
        "harness": harness,
        "session_id": sid,
        "started_at": "2026-09-01T01:00:00Z",
    })
}

/// The shipped shell's order against a REAL graph file: settle first, then
/// the row pass over the production graph read. Only the transcript/stop/
/// tree seams are staged (gc::gc_sweep resolves those from live stores a
/// tmp home does not have); the settle -> run -> read ordering under test is
/// the wiring gc.rs ships.
fn settle_then_run(
    home: &AgentsHome,
    emitter: &EventEmitter,
    dry_run: bool,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    agents: crate::claude_roster::ClaudeAgentsSnapshot,
) -> GcSummary {
    if dry_run {
        let planned = gc_sweep::plan_stale_do_rows(home);
        let mut summary = gc_sweep::run(
            home,
            emitter,
            0,
            true,
            0,
            &|h| {
                gc_sweep::read_graph_entries(h).map(|g| gc_sweep::without_settled(g, &planned, &[]))
            },
            transcripts,
            &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
            &|_| true,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &move || agents.clone(),
            &|_| (None, None),
            &|_| None,
        );
        summary.settled_do_rows = planned
            .into_iter()
            .map(|row| (row.node, row.harness, row.session_id))
            .collect();
        summary
    } else {
        let mut read = |_path: &str, _cwd: &str| -> Option<gc_sweep::PrState> {
            panic!("no existing settle_then_run test may reach GitHub");
        };
        let (settled, refused) = gc_sweep::settle_stale_do_rows_with(home, &mut read);
        let mut summary = gc_sweep::run(
            home,
            emitter,
            0,
            false,
            7,
            &gc_sweep::read_graph_entries,
            transcripts,
            &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
            &|_| true,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &move || agents.clone(),
            &|_| (None, None),
            &|_| None,
        );
        summary.settled_do_rows = settled
            .into_iter()
            .map(|row| (row.node, row.harness, row.session_id))
            .collect();
        summary.settle_refused = refused;
        summary
    }
}

/// A registry row named on `sid`, spawned by fno, transcript-probeable.
fn spawn_row(reg: &mut state::Registry, name: &str, sid: &str) {
    let mut e = ask_row(name, None);
    e.harness = Some("claude".into());
    e.harness_session_id = Some(sid.into());
    e.origin = Some("spawn".into());
    // The verdict door reads the stored status first; these scenarios vary
    // the vendor word, so the row itself must be fno-undecided.
    e.status = crate::AgentStatus::Idle;
    reg.entries.push(e);
}

/// AC4-EDGE: the rehearsal names the settle and touches nothing - the graph
/// store version is unchanged, the row still open, and the row pass
/// reads it as would-retire.
#[test]
fn a_dry_run_settles_nothing_on_disk() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "d.jsonl", 2 * 3600);
    stage_graph(
        dir.path(),
        json!([done_node(
            "N4",
            json!("merged"),
            json!([]),
            vec![open_do_row("claude", "sess-d")],
        )]),
    );
    state::update_registry(&home.registry_json(), |r| {
        spawn_row(r, "row-d", "sess-d");
    })
    .unwrap();
    let before = crate::backlog::version(&dir.path().join("graph.json")).unwrap();

    let summary = settle_then_run(
        &home,
        &emitter,
        true,
        &move |_| Some(vec![quiet.clone()]),
        crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
            crate::claude_roster::ClaudeAgentRow::new("sess-d", Some("done")),
        ]),
    );

    assert_eq!(
        summary.settled_do_rows,
        vec![("N4".into(), "claude".into(), "sess-d".into())]
    );
    assert!(summary.kept_open_do_row.is_empty());
    let after = crate::backlog::version(&dir.path().join("graph.json")).unwrap();
    assert_eq!(before, after, "a dry run changed the store");
    let rows = crate::graph_store::read_rows(&dir.path().join("graph.json")).unwrap();
    assert!(rows[0]["sessions"][0].get("ended_at").is_none());
    assert!(
        !summary.dry_run_unverified.is_empty(),
        "the row stays on the retirement path, its remaining gate named: {summary:?}"
    );
}

/// x-5aef AC5, hard version (the codex P1 on PR 1637): an open do row that
/// opened after the decision must hold the row BEFORE the effects fire. The
/// decision seam answers done-and-quiet; the real graph carries an open do
/// row naming this session. The stop and surface seams COUNT their calls,
/// proving the effects never fired; the row keeps under kept_open_do_row
/// with the node named. A held session whose process was already stopped is
/// not held at all - it is dead.
#[test]
fn a_graph_obligation_opened_after_the_decision_holds_before_the_effects() {
    use crate::daemon::CascadeOutcome;

    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    // The real graph: an open do row for this session on x-new - the
    // obligation that "opens after the decision".
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-new",
            "status": "ready",
            "sessions": [open_do_row("codex", "s-commit")],
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "commitw".into();
        e.short_id = "commitw".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-commit".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let store = home.root().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let quiet = quiet_transcript(&store, "q.jsonl", 2 * 3600);
    let stop_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let surface_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    // AC1-HP: BOTH modes re-read the graph at staging and both keep
    // the row. One sweep runner, one counter pair, two modes.
    let run_once = |dry: bool,
                    stop_c: std::rc::Rc<std::cell::Cell<usize>>,
                    surf_c: std::rc::Rc<std::cell::Cell<usize>>|
     -> GcSummary {
        // The decision seam: done, no open do row - the stale evidence the
        // staging re-read must contradict.
        let graph = graph_read(&[("s-commit", "N1", "done")], &[]);
        let q = quiet.clone();
        gc_sweep::run(
            &home,
            &emitter,
            900,
            dry,
            7,
            &move |_| graph.clone(),
            &move |_| Some(vec![q.clone()]),
            &uniform_ages(2 * 3600),
            &move |_| {
                stop_c.set(stop_c.get() + 1);
                true
            },
            &move |_| {
                surf_c.set(surf_c.get() + 1);
                CascadeOutcome::Removed
            },
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_| (Some(true), Some(true)),
            &|_| None,
        )
    };
    let dry = run_once(
        true,
        std::rc::Rc::clone(&stop_calls),
        std::rc::Rc::clone(&surface_calls),
    );
    let acting = run_once(
        false,
        std::rc::Rc::clone(&stop_calls),
        std::rc::Rc::clone(&surface_calls),
    );
    for (mode, summary) in [("dry", dry), ("apply", acting)] {
        assert!(summary.retired.is_empty(), "{mode}: {:?}", summary.retired);
        assert!(
            summary
                .kept_open_do_row
                .iter()
                .any(|(id, node)| id == "commitw" && node == "x-new"),
            "{mode}: {:?}",
            summary.kept_open_do_row
        );
    }
    assert_eq!(
        stop_calls.get(),
        0,
        "the stop must never fire in either mode"
    );
    assert_eq!(
        surface_calls.get(),
        0,
        "the surface removal must never fire in either mode"
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .any(|e| e.name == "commitw"),
        "the row survives"
    );
}

// ── positive death evidence in the reaper ─────────────────────────────────

/// A sweep with the agents snapshot and stop seam staged separately, so a
/// test can prove WHICH evidence confirmed (or refused) the stop.
#[allow(clippy::too_many_arguments)]
pub(super) fn evidence_sweep(
    sweep_home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    dry_run: bool,
    graph: Option<GraphRead>,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    agents: crate::claude_roster::ClaudeAgentsSnapshot,
    stop: &dyn Fn(&state::RegistryEntry) -> bool,
) -> GcSummary {
    gc_sweep::run(
        sweep_home,
        emitter,
        grace_secs,
        dry_run,
        7,
        &move |_| graph.clone(),
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        stop,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &move || agents.clone(),
        &|_| (None, None),
        &|_| None,
    )
}

pub(super) fn claude_worker_row(name: &str, short: &str) -> state::RegistryEntry {
    let mut row = ask_row(name, None);
    row.short_id = short.into();
    row.harness = Some("claude".into());
    row.harness_session_id = Some(format!("{short}-1111-2222-3333-444444444444"));
    row.origin = Some("spawn".into());
    // The verdict door reads the stored status first (Exited would decide
    // the row fno-Finished before the vendor word these scenarios vary);
    // a live worker row is non-terminal.
    row.status = crate::AgentStatus::Idle;
    row
}

#[test]
fn blocked_roster_state_is_not_death_evidence() {
    let home = tmp_home("gc-evidence-blocked");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "a.jsonl", 7200);
    state::update_registry(&home.registry_json(), |registry| {
        registry
            .entries
            .push(claude_worker_row("row-blocked", "eeee5555"));
    })
    .unwrap();
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("eeee5555", Some("blocked")),
    ]);
    let summary = evidence_sweep(
        &home,
        &emitter,
        0,
        false,
        graph_read(
            &[("eeee5555-1111-2222-3333-444444444444", "N1", "done")],
            &[],
        ),
        &|_| Some(vec![quiet.clone()]),
        agents,
        &|_| false,
    );
    assert!(summary.retired.is_empty());
    assert!(summary
        .stop_refused
        .iter()
        .any(|(id, why)| id == "eeee5555" && why.contains("no death evidence")));
    std::fs::remove_dir_all(home.root()).ok();
}

// == x-a33f: the stage ends a row's process the way rm does ==============

// ── the live-claim keep ──────────────────────────────────────

/// A live claim naming the session keeps a quiet, done-node row the sweep
/// would otherwise retire. The 2026-09-25 registry sweeps retired thread
/// workers whose claims were live; the claim fact now rides the row and
/// the grace gate keeps it past quiet.
#[test]
fn a_live_claim_keeps_a_quiet_row_the_sweep_would_retire() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    struct ClaimsRootGuard(Option<std::ffi::OsString>);
    impl Drop for ClaimsRootGuard {
        fn drop(&mut self) {
            if let Some(root) = self.0.take() {
                std::env::set_var("FNO_CLAIMS_ROOT", root);
            } else {
                std::env::remove_var("FNO_CLAIMS_ROOT");
            }
        }
    }
    let _claims_root_guard = ClaimsRootGuard(std::env::var_os("FNO_CLAIMS_ROOT"));
    let mut failures = Vec::new();
    for (case, claim_sid, status, row_sid, open_do, scope, expected) in [
        (
            "direct session",
            Some("s-main"),
            "in_progress",
            "s-main",
            true,
            "none",
            "live",
        ),
        (
            "legacy open execution",
            None,
            "in_progress",
            "s-main",
            true,
            "none",
            "live",
        ),
        ("done node", None, "done", "s-main", true, "none", "retired"),
        (
            "different session",
            Some("s-other"),
            "in_progress",
            "s-main",
            true,
            "none",
            "retired",
        ),
        (
            "child without execution",
            None,
            "in_progress",
            "s-child",
            true,
            "none",
            "retired",
        ),
        (
            "different node",
            None,
            "in_progress",
            "s-main",
            true,
            "different-node",
            "retired",
        ),
        (
            "empty cwd",
            None,
            "in_progress",
            "s-main",
            false,
            "empty",
            "retired",
        ),
        (
            "relative cwd",
            None,
            "in_progress",
            "s-main",
            false,
            "relative",
            "retired",
        ),
        (
            "absent local store",
            None,
            "in_progress",
            "s-main",
            false,
            "absent",
            "retired",
        ),
        (
            "closed execution",
            None,
            "in_progress",
            "s-main",
            false,
            "none",
            "retired",
        ),
        (
            "unrelated local scope",
            Some("s-main"),
            "in_progress",
            "s-main",
            true,
            "unrelated",
            "live",
        ),
        (
            "required local scope",
            Some("s-other"),
            "in_progress",
            "s-main",
            true,
            "required",
            "unread",
        ),
        (
            "global scope",
            None,
            "in_progress",
            "s-main",
            true,
            "global",
            "unread",
        ),
    ] {
        let (dir, home) = staged_graph_home();
        std::env::set_var("FNO_CLAIMS_ROOT", dir.path());
        let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
        let quiet = quiet_transcript(home.root(), "q.jsonl", 7200);
        let mut execution = json!({"phase":"execute", "harness":"claude", "session_id":"s-main", "started_at":"2026-09-01T00:00:00Z"});
        if !open_do {
            execution
                .as_object_mut()
                .unwrap()
                .insert("ended_at".into(), json!("2026-09-01T01:00:00Z"));
        }
        stage_graph(
            dir.path(),
            json!([{"id":"N1", "status":status, "sessions":[execution]}]),
        );
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let bad = dir.path().join("bad");
        std::fs::write(&bad, "regular file, not a claim root").unwrap();
        let mut row = ask_row("row-claim", None);
        row.short_id = "t-claim".into();
        row.harness = Some("claude".into());
        row.harness_session_id = Some(row_sid.into());
        row.node = Some("N1".into());
        row.origin = Some("spawn".into());
        row.cwd = match scope {
            "required" => bad.clone(),
            "empty" => PathBuf::new(),
            "relative" => PathBuf::from("relative-project"),
            "absent" => dir.path().join("not-created"),
            _ => work.clone(),
        }
        .to_string_lossy()
        .into_owned();
        let mut rows = vec![row];
        if scope == "unrelated" || scope == "global" {
            let mut other = ask_row("row-other", None);
            other.harness = Some("claude".into());
            other.origin = Some("spawn".into());
            other.node = Some("N1".into());
            other.cwd = if scope == "unrelated" { &bad } else { &work }
                .to_string_lossy()
                .into_owned();
            rows.push(other);
        }
        state::update_registry(&home.registry_json(), |r| r.entries.extend(rows)).unwrap();
        if scope == "global" {
            std::env::set_var("FNO_CLAIMS_ROOT", &bad);
        } else {
            let rec = crate::claims::ClaimRecord {
                schema_version: 1,
                key: if scope == "different-node" {
                    "node:N2"
                } else {
                    "node:N1"
                }
                .into(),
                holder: "opaque-holder".into(),
                acquired_at: crate::claims::now_ms(),
                pid: Some(std::process::id() as i32),
                host: crate::claims::hostname(),
                pid_unavailable: false,
                expires_at: None,
                reason: Some("resume reserve".into()),
                harness: claim_sid.map(|_| "claude".into()),
                session_id: claim_sid.map(str::to_string),
                pid_provenance: None,
                machine_id: None,
                metadata: Default::default(),
            };
            crate::claim_store::seed_at_path(
                &crate::claims::claim_path(&rec.key, Some(dir.path())).unwrap(),
                &rec,
            );
        }
        let summary = gc_sweep::run(
            &home,
            &emitter,
            900,
            false,
            7,
            &gc_sweep::read_graph_entries,
            &|_| Some(vec![quiet.clone()]),
            &uniform_ages(7200),
            &|_| true,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_| (None, None),
            &|_| None,
        );
        let live = summary
            .kept_live_claim
            .iter()
            .any(|(id, _)| id == "t-claim");
        let unread = summary
            .kept_other
            .iter()
            .any(|(id, reason)| id == "t-claim" && reason == "claims unreadable");
        let retired = summary.retired.iter().any(|(id, _)| id == "t-claim");
        let outcome = match expected {
            "live" => live,
            "unread" => unread,
            _ => retired,
        };
        if !outcome {
            failures.push(format!("{case}: expected {expected}, got {summary:?}"));
        }
        if scope == "unrelated" || scope == "global" {
            if !summary
                .kept_other
                .iter()
                .any(|(id, reason)| id == "row-other" && reason == "claims unreadable")
            {
                failures.push(format!(
                    "{case}: required unread scope did not protect row-other: {summary:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The blueprint retirement families, split by the file budget; the
/// fixtures above are the shared seams.
#[path = "gc_receipts/blueprint_retirement.rs"]
mod blueprint_retirement;

/// The additional-PR settle families (tasks 1.1 and 1.2): the three graph
/// rules, the one-GitHub-read stamp, and the pass that applies it.
#[path = "gc_receipts/additional_pr_settle.rs"]
mod additional_pr_settle;

/// The retirement-removes-the-session families: the production active-surface
/// seam runs for real against a fake `claude` on PATH.
#[path = "gc_receipts/retire_removes_session.rs"]
mod retire_removes_session;

/// The spawn-edge kind families (a PEER handoff never holds its spawner),
/// split by the file budget; the fixtures above are the shared seams.
#[path = "gc_receipts/lineage_kind.rs"]
mod lineage_kind;

/// The open-PR reap guard families: keep + resume for the driver with no
/// termination, alert for the reap that goes through on an open-PR node.
#[path = "gc_receipts/open_pr_guard.rs"]
mod open_pr_guard;
