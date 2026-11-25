use super::*;

/// AC5-EDGE: an unexpected gh state string maps to PrState::None
/// (fail-closed), never panics.
#[test]
fn pr_state_unknown_string_fails_closed() {
    assert_eq!(PrState::from_gh_str("DRAFT"), PrState::None);
    assert_eq!(PrState::from_gh_str(""), PrState::None);
    assert_eq!(PrState::from_gh_str("open"), PrState::None);
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
    let loopcheck_script = format!(
        r#"#!/bin/sh
printf '%s\n' "LOOPCHECK $*" >> "$RECORD_CALLS"
case "$*" in
  *pr*view*)
    echo '{{"state":"OPEN","number":7,"headRefName":"feature","headRefOid":"{oid}","mergeable":"MERGEABLE","mergeStateStatus":"clean","baseRefName":"main","author":{{"login":"jj"}},"url":"https://github.com/b/r/pull/7"}}'
    exit 0 ;;
  *pr*checks*)
    echo '[]'
    exit 0 ;;
  *pulls/7/comments*)
    echo '[{{"body":"![high] fix this"}}]'
    exit 0 ;;
  *pulls/7/commits*)
    echo '[{{"commit":{{"committer":{{"date":"2026-01-02T03:04:05Z"}}}}}}]'
    exit 0 ;;
esac
echo "unexpected loopcheck call: $*" >&2
exit 1
"#
    );
    let coverage_script = r#"#!/bin/sh
printf '%s\n' "COVERAGE $*" >> "$RECORD_CALLS"
case "$*" in
  *pr*view*)
    echo '{"reviews":[],"comments":[],"headRefOid":"c0ffee","baseRefName":"main"}'
    exit 0 ;;
esac
echo "unexpected coverage call: $*" >&2
exit 1
"#
    .to_string();
    let gh = crate::write_exec_stub(dir.path(), "fno-gh-loopcheck", &loopcheck_script);
    let _coverage = crate::write_exec_stub(dir.path(), "fno-gh-coverage", &coverage_script);
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
        false, // no_external=false: the review reads run, and the pin holds on them too
        &["chatgpt-codex-connector".to_string()], // a required login: the else arm runs (Read 3 + 4)
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
        false,
        2,
        100,
        None,
        &[],
        Some("b/r"),
    )
    .unwrap_or_else(|_| panic!("every read answers"));
    std::env::remove_var("RECORD_CALLS");
    assert_eq!(info.number, 7);
    let recorded = std::fs::read_to_string(&calls).unwrap();
    let mut pr_view_pinned = false;
    let mut checks_pinned = false;
    for row in recorded.lines() {
        if row.contains("LOOPCHECK pr view") {
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
    // The folded transport contract: the ordinary fire's review reads ride
    // the REST transports, never the loopcheck broker. The reviews read is
    // answered by the coverage shim (the promise-fire transport) and the
    // commits read rides the raw commits endpoint pinned to the node repo,
    // so a drained GraphQL bucket cannot park a fire whose merge question
    // these reads answer.
    assert!(
        recorded.contains("COVERAGE pr view 7 --repo b/r --json reviews,comments"),
        "the reviews read rode the coverage shim: {recorded}"
    );
    for row in recorded.lines() {
        assert!(
            !(row.starts_with("LOOPCHECK") && row.contains("reviews")),
            "the loopcheck shim never served a reviews read: {row}"
        );
        assert!(
            !(row.starts_with("LOOPCHECK") && row.contains("--json commits")),
            "the commits read rides the api endpoint, not pr view: {row}"
        );
    }
    assert!(
        recorded.contains("LOOPCHECK api repos/b/r/pulls/7/commits"),
        "the commits read rode the REST endpoint pinned to the node repo: {recorded}"
    );
    // The pin is per-number (the pure half, `pr_repo_from_rows`): among the
    // session node's PR rows, the row naming the asked number pins its repo.
    // A node whose only numbered row is its cross-repo `additional_prs`
    // docs PR must not pin a different operand number to that repo: the
    // status read for PR 485 asked footnote-sh/fno-web for PR 485 (no such
    // PR there, gh 404) and ready never turned true, so the merge door
    // stalled. The split docs PR itself still reads in its own repo, from
    // its url.
    let extras_only: Vec<(i64, Option<&str>, bool, Option<&str>)> = vec![(
        268,
        Some("docs"),
        false,
        Some("https://github.com/footnote-sh/fno-web/pull/268"),
    )];
    assert_eq!(
        pr_read::pr_repo_from_rows(&extras_only, 268).as_deref(),
        Some("footnote-sh/fno-web"),
        "the split docs PR reads in its own repo, from its url"
    );
    assert_eq!(
        pr_read::pr_repo_from_rows(&extras_only, 485),
        None,
        "a number the node never recorded keeps the ambient origin"
    );
    // A split node: primary in this repo, docs PR in another.
    let split: Vec<(i64, Option<&str>, bool, Option<&str>)> = vec![
        (
            485,
            Some("open"),
            false,
            Some("https://github.com/fnodotdev/fno/pull/485"),
        ),
        (
            268,
            None,
            false,
            Some("https://github.com/footnote-sh/fno-web/pull/268"),
        ),
    ];
    assert_eq!(
        pr_read::pr_repo_from_rows(&split, 485).as_deref(),
        Some("fnodotdev/fno")
    );
    assert_eq!(
        pr_read::pr_repo_from_rows(&split, 268).as_deref(),
        Some("footnote-sh/fno-web")
    );
    // A bare-number row (no url) never pins.
    let bare: Vec<(i64, Option<&str>, bool, Option<&str>)> = vec![(7, None, false, None)];
    assert_eq!(pr_read::pr_repo_from_rows(&bare, 7), None);
    // One number recorded in two repos: refuse to the ambient origin.
    let clash: Vec<(i64, Option<&str>, bool, Option<&str>)> = vec![
        (9, None, false, Some("https://github.com/a/r/pull/9")),
        (9, None, false, Some("https://github.com/b/r/pull/9")),
    ];
    assert_eq!(pr_read::pr_repo_from_rows(&clash, 9), None);
}

/// The unmeasured fold: a green checks read over a head whose
/// Actions runs listing names no completed run reads PENDING, not SUCCESS -
/// PR 82 merged ready with all 7 head runs queued and the loop said
/// DonePRGreen for a head CI never measured.
#[test]
fn a_green_read_over_an_unmeasured_head_folds_to_pending() {
    let _guard = crate::claims::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let oid = "a".repeat(40);
    let runs: Vec<String> = (0..7)
        .map(|i| {
            format!(r#"{{"id": {i}, "name": "w{i}", "status": "queued", "conclusion": null}}"#)
        })
        .collect();
    let runs_j = runs.join(",");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$RECORD_CALLS"
case "$*" in
  *pr*view*)
    echo '{{"state":"OPEN","number":7,"headRefName":"feature","headRefOid":"{oid}","mergeable":"MERGEABLE","mergeStateStatus":"clean","baseRefName":"main","author":{{"login":"jj"}},"url":"https://github.com/b/r/pull/7"}}'
    exit 0 ;;
  *pr*checks*)
    echo '[{{"name":"rust-ci","bucket":"pass"}}]'
    exit 0 ;;
  *actions/runs?head_sha*)
    echo '{{"total_count": 7, "workflow_runs": [{runs_j}]}}'
    exit 0 ;;
esac
echo 'unexpected gh call' >&2
exit 1
"#
    );
    let gh = crate::write_exec_stub(dir.path(), "gh-fold", &script);
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
        true, // no_external: the review reads stay out; the fold is the point
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
        false,
        2,
        100,
        None,
        &[],
        Some("b/r"),
    )
    .unwrap_or_else(|_| panic!("the fold read answers"));
    std::env::remove_var("RECORD_CALLS");
    assert!(
        info.ci_unmeasured
            .as_deref()
            .unwrap_or("")
            .contains("7 runs queued"),
        "the fold names the unmeasured head: {:?}",
        info.ci_unmeasured
    );
    assert!(
        matches!(info.ci_conclusion, CiConclusion::Pending),
        "a green read over an unmeasured head holds: {:?}",
        info.ci_conclusion
    );
}

// The pin is per-number: the asserts live inside
// `the_gate_pins_every_pr_read_to_the_node_pr_url_repo`, whose harness they
// extend - the G3 scope is shrink-only, so the per-number contract shares
// that declaration instead of adding one.
