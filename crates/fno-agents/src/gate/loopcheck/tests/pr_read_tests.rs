use super::*;

/// The selector half of the target read, for the row-choice tests: the
/// production callers need both halves, the row-choice contract is about
/// the number alone.
fn row_selector(cwd: &std::path::Path, node: Option<&str>) -> Option<String> {
    pr_read::manifest_node_pr_target(cwd, node).map(|(selector, _)| selector)
}

/// AC5-EDGE: an unexpected gh state string maps to PrState::None
/// (fail-closed), never panics.
#[test]
fn pr_state_unknown_string_fails_closed() {
    assert_eq!(PrState::from_gh_str("DRAFT"), PrState::None);
    assert_eq!(PrState::from_gh_str(""), PrState::None);
    assert_eq!(PrState::from_gh_str("open"), PrState::None);
}

/// AC5-UI: as_str/render reproduce the exact legacy fingerprint vocabulary.
#[test]
fn enum_rendering_byte_identical_to_legacy_strings() {
    assert_eq!(PrState::Open.as_str(), "OPEN");
    assert_eq!(PrState::Merged.as_str(), "MERGED");
    assert_eq!(PrState::Closed.as_str(), "CLOSED");
    assert_eq!(PrState::None.as_str(), "none");
    assert_eq!(CiConclusion::Success.render(), "SUCCESS");
    assert_eq!(
        CiConclusion::Failure(Some("lint".into())).render(),
        "FAILURE:lint"
    );
    assert_eq!(CiConclusion::Failure(None).render(), "FAILURE");
    assert_eq!(CiConclusion::Pending.render(), "PENDING");
    assert_eq!(CiConclusion::Skipped.render(), "skipped");
    assert_eq!(CiConclusion::None.render(), "none");
}

/// The manifest node's primary PR pins the terminal read's selector, and
/// every unreadable shape falls open to the branch-resolved read (None) -
/// the branch read is the only correct answer before the PR exists at all.
#[test]
fn manifest_node_selector_reads_the_primary_pr_and_fails_open() {
    let _guard = crate::claims::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::env::set_var("FNO_STATE_DIR", dir.path().join("state"));
    let graph = crate::graph_get::graph_path_for(&cwd);
    std::fs::create_dir_all(graph.parent().expect("graph path always has a parent")).unwrap();
    std::fs::write(&graph, b"{\"entries\": []}").unwrap();
    let connection = crate::backlog::open(&graph).unwrap();
    crate::backlog::save_aggregate(
        &connection,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-sel", "slug": "sel", "title": "T", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 930,
            "pr_url": "https://github.com/o/r/pull/930",
        }))
        .unwrap(),
    )
    .unwrap();
    // A second node carrying no PR: the branch-resolved read stays.
    crate::backlog::save_aggregate(
        &connection,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-bare", "slug": "bare", "title": "B", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(row_selector(&cwd, Some("x-sel")).as_deref(), Some("930"));
    // The target carries the chosen row's url-derived owner/repo: a node
    // whose PR lives in another repo reads THAT repo, never the ambient
    // origin's same-number PR. A row without a url (or with a non-GitHub
    // one) answers None and keeps the ambient origin.
    let (selector, repo) = pr_read::manifest_node_pr_target(&cwd, Some("x-sel")).unwrap();
    assert_eq!(selector, "930");
    assert_eq!(repo.as_deref(), Some("o/r"));
    assert_eq!(pr_read::manifest_node_pr_target(&cwd, Some("x-bare")), None);
    assert_eq!(row_selector(&cwd, Some("x-bare")), None);
    // An unknown node and a missing manifest node both fail open.
    assert_eq!(row_selector(&cwd, Some("x-absent")), None);
    assert_eq!(row_selector(&cwd, None), None);

    // A node whose attached rows disagree: a merged-stamped primary with a
    // later attached row picks the later PR, and an all-terminal node picks
    // the newest number. attach writes merge_status None, so an attached row
    // always outranks a stamped dead one. The second store lives under its
    // own state root, so the function under test reads it via the same
    // FNO_STATE_DIR door as the first.
    std::env::set_var("FNO_STATE_DIR", dir.path().join("state2"));
    let graph2 = crate::graph_get::graph_path_for(&cwd);
    std::fs::create_dir_all(graph2.parent().expect("graph path always has a parent")).unwrap();
    std::fs::write(&graph2, b"{\"entries\": []}").unwrap();
    let conn2 = crate::backlog::open(&graph2).unwrap();
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-multi", "slug": "multi", "title": "M", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 900,
            "pr_url": "https://github.com/o/r/pull/900",
            "merge_status": "MERGED",
            "additional_prs": [
                {"number": 940, "url": "https://github.com/o/r/pull/940"},
                {"number": 930, "url": "https://github.com/o/r/pull/930", "merge_status": "MERGED"},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-reopen", "slug": "reopen", "title": "R", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 900,
            "pr_url": "https://github.com/o/r/pull/900",
            "merge_status": "MERGED",
            "additional_prs": [
                {"number": 930, "url": "https://github.com/o/r/pull/930", "merge_status": "MERGED"},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    // The store's own stamps are lowercase (additional_prs attach and the
    // api reconcile writers): a lowercase-stamped primary must lose to the
    // live row exactly as the uppercase spelling does.
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-lower", "slug": "lower", "title": "L", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 900,
            "pr_url": "https://github.com/o/r/pull/900",
            "merge_status": "merged",
            "additional_prs": [
                {"number": 950, "url": "https://github.com/o/r/pull/950"},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-reolder", "slug": "reolder", "title": "O", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 960,
            "pr_url": "https://github.com/o/r/pull/960",
            "merge_status": "merged",
            "additional_prs": [
                {"number": 930, "url": "https://github.com/o/r/pull/930"},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    // An explicitly marked history row sinks whatever its number: state,
    // not number order, decides for rows the reopen demotion wrote.
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-marked", "slug": "marked", "title": "K", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 900,
            "pr_url": "https://github.com/o/r/pull/900",
            "merge_status": "merged",
            "additional_prs": [
                {"number": 970, "url": "https://github.com/o/r/pull/970", "history": true},
            ],
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(row_selector(&cwd, Some("x-multi")).as_deref(), Some("940"));
    // A reopened node's history row: the older unstamped row predates the
    // stamp, so the merged current pr_number pins the gate.
    assert_eq!(row_selector(&cwd, Some("x-reolder")).as_deref(), Some("960"));
    // A marked history row sinks even newer than the stamp: state beats
    // number order.
    assert_eq!(row_selector(&cwd, Some("x-marked")).as_deref(), Some("900"));
    assert_eq!(row_selector(&cwd, Some("x-reopen")).as_deref(), Some("930"));
    assert_eq!(row_selector(&cwd, Some("x-lower")).as_deref(), Some("950"));
    std::env::remove_var("FNO_STATE_DIR");
}

/// A replacement PR binds only through its own `Fixes` line, open or
/// merged, never the closed PR it replaced or one that merely mentions the
/// node.
#[test]
fn live_fixes_pr_picks_the_newest_open_or_merged_closure() {
    let found = serde_json::json!({"items": [
        {"number": 900, "state": "closed", "pull_request": {"merged_at": null}, "body": "Fixes x-aaaa"},
        {"number": 940, "state": "closed", "pull_request": {"merged_at": "2026-10-21T00:00:00Z"}, "body": "body\n\nFixes x-aaaa"},
        {"number": 950, "state": "open", "pull_request": {"merged_at": null}, "body": "mentions x-aaaa\n\nFixes x-bbbb"},
        {"number": 930, "state": "open", "pull_request": {"merged_at": null}, "body": "Fixes x-aaaa x-bbbb"},
    ]});
    assert_eq!(
        pr_read::pick_live_fixes_pr(&found, "x-aaaa", 900),
        Some(940)
    );
    assert_eq!(
        pr_read::pick_live_fixes_pr(&found, "x-aaaa", 940),
        Some(930)
    );
    assert_eq!(pr_read::pick_live_fixes_pr(&found, "x-cccc", 900), None);
    assert_eq!(
        pr_read::pick_live_fixes_pr(&serde_json::json!({}), "x-aaaa", 900),
        None
    );
}

/// The gate pins every PR read to the node PR row's own repo: a node whose
/// pr_url names repo B, read from a checkout whose origin is repo A, asks
/// for repo B on the terminal read and the checks read - never the ambient
/// origin's same-number PR (the 2026-11-11 cutover shape that blocked the
/// promise against an old merged PR 21).
#[test]
fn the_gate_pins_every_pr_read_to_the_node_pr_url_repo() {
    let _guard = crate::claims::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let oid = "a".repeat(40);
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$@" >> "$RECORD_CALLS"
case "$*" in
  *pr*view*)
    echo '{{"state":"OPEN","number":7,"headRefName":"feature","headRefOid":"{oid}","mergeable":"MERGEABLE","mergeStateStatus":"clean","baseRefName":"main","author":{{"login":"jj"}},"url":"https://github.com/b/r/pull/7"}}'
    exit 0 ;;
  *pr*checks*)
    echo '[]'
    exit 0 ;;
esac
echo 'unexpected gh call' >&2
exit 1
"#
    );
    let gh = crate::write_exec_stub(dir.path(), "gh-pin", &script);
    std::env::set_var("RECORD_CALLS", &calls);
    let events = dir.path().join("events.jsonl");
    let global = dir.path().join("global.jsonl");
    std::fs::write(&events, "").unwrap();
    std::fs::write(&global, "").unwrap();
    let head = "b".repeat(40);
    let info = pr_read::read_pr_info(
        gh.to_str().unwrap(),
        "git",
        dir.path(),
        false,
        true, // no_external: the review reads stay out; the pin is the point
        &[],
        &[],
        false,
        &[],
        &[],
        &[],
        &head,
        &events,
        &global,
        "a/r",
        None,
        Some("7"),
        None,
        true,
        2,
        100,
        None,
        &[],
        Some("b/r"),
    )
    .unwrap_or_else(|_| panic!("the pinned read answers"));
    std::env::remove_var("RECORD_CALLS");
    assert_eq!(info.number, 7);
    let recorded = std::fs::read_to_string(&calls).unwrap();
    let mut pr_view_pinned = false;
    let mut checks_pinned = false;
    for row in recorded.lines() {
        if row.contains("pr view") {
            assert!(
                row.contains("--repo b/r"),
                "the terminal read pins the node repo: {row}"
            );
            pr_view_pinned = true;
        }
        if row.contains("pr checks") {
            assert!(
                row.contains("--repo b/r"),
                "the checks read pins the node repo: {row}"
            );
            checks_pinned = true;
        }
        assert!(
            !row.contains("--repo a/r"),
            "the ambient origin never pins a read: {row}"
        );
    }
    assert!(
        pr_view_pinned,
        "the terminal read ran with the pin: {recorded}"
    );
    assert!(checks_pinned, "the checks read ran: {recorded}");
}
