use super::*;

// 2026-09-09T15:46:29Z, the do-row fixtures' start.
const START_MS: i64 = 1_788_968_789_000;
const CLAUDE_SID: &str = "9f06a492-1111-4222-8333-444455556666";

fn node(id: &str, extra: Value) -> Value {
    let mut n = json!({"id": id, "title": id, "status": "ready"});
    for (k, v) in extra.as_object().unwrap() {
        n[k] = v.clone();
    }
    n
}

fn do_node(id: &str, extra: Value) -> Value {
    let mut n = node(
        id,
        json!({"status": "in_progress", "cwd": "/some/worktree", "sessions": [
        {"phase": "execute", "harness": "claude", "session_id": CLAUDE_SID,
         "started_at": "2026-09-09T15:46:29Z"}]}),
    );
    for (k, v) in extra.as_object().unwrap() {
        n[k] = v.clone();
    }
    n
}

#[test]
fn an_unread_roster_reaps_nothing() {
    let entries = [do_node("x-deg01", json!({}))];
    let blind =
        || -> Result<BTreeMap<String, Vec<String>>, String> { Err("probe timed out".into()) };
    let now = START_MS + 40 * 3_600_000;
    let err = abandoned_do_rows(&entries, &BTreeSet::new(), 24, &blind, now, true).unwrap_err();
    assert_eq!(err, "roster unread (probe timed out)");
    let held = abandoned_do_rows(&entries, &BTreeSet::new(), 24, &blind, now, false).unwrap();
    assert_eq!(
        (held[0].verdict, held[0].reason.as_str()),
        ("held", "roster unread (probe timed out)")
    );
}

// ---------------------------------------------------------------------
// The whole pass over a seeded store
// ---------------------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    graph: PathBuf,
}

impl Fixture {
    fn new(rows: &[Value]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        crate::graph_store::seed_rows(&graph, rows).unwrap();
        Fixture { dir, graph }
    }

    fn rows(&self) -> Vec<Value> {
        crate::graph_store::read_rows(&self.graph).unwrap()
    }

    fn row(&self, id: &str) -> Value {
        self.rows()
            .into_iter()
            .find(|r| entry_id(r) == Some(id))
            .unwrap()
    }

    fn history(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

struct Run<'a> {
    settings: Settings,
    workspaces: HashMap<String, String>,
    claims: &'a dyn Fn() -> Result<BTreeSet<String>, String>,
    events: Vec<Value>,
    slugs: HashMap<String, String>,
    analyze: validity::Analyzer<'a>,
}

fn no_claims() -> Result<BTreeSet<String>, String> {
    Ok(BTreeSet::new())
}

fn keep_all(
    packets: &[validity::EvidencePacket],
    _: Option<f64>,
) -> Result<BTreeMap<String, Value>, String> {
    Ok(packets
        .iter()
        .map(|p| {
            (
                p.node_id.clone(),
                json!({"classification": "keep", "confidence": 0.8, "rationale": "stub"}),
            )
        })
        .collect())
}

impl Default for Run<'_> {
    fn default() -> Self {
        Run {
            settings: Settings::default(),
            workspaces: HashMap::new(),
            claims: &no_claims,
            events: Vec::new(),
            slugs: HashMap::new(),
            analyze: &keep_all,
        }
    }
}

impl Run<'_> {
    fn go(&self, fx: &Fixture, args: &[&str]) -> (i32, Out) {
        let tail: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let flags = parse_flags(&tail).unwrap();
        let events = || self.events.clone();
        let slug = |cwd: Option<&str>| cwd.and_then(|c| self.slugs.get(c).cloned());
        let roster = || -> Result<BTreeMap<String, Vec<String>>, String> { Ok(BTreeMap::new()) };
        let no_exists = |_: &Value| -> Option<validity::PathProbe> { None };
        let no_search = |_: &str| -> Option<usize> { None };
        let no_retro = |_: &Value| -> Vec<(String, String)> { Vec::new() };
        let seams = Seams {
            graph: fx.graph.clone(),
            settings: self.settings.clone(),
            workspaces: self.workspaces.clone(),
            worktrees_base: None,
            claims: self.claims,
            events: &events,
            slug: &slug,
            roster: &roster,
            history: Some(fx.dir.path().join("history.jsonl")),
            deck_dir: Some(fx.dir.path().join("validity-decks")),
            history_dir: Some(fx.dir.path().join("history")),
            vision_grade: &|_, _| None::<(String, String)>,
            sweep: SweepSeams {
                exists_for: &no_exists,
                search: &no_search,
                retro: &no_retro,
                analyze: self.analyze,
            },
        };
        let mut out = Out::captured();
        let code = run_pass(flags, &seams, &mut out);
        (code, out)
    }
}

#[test]
fn apply_prunes_rescopes_backfills_and_skips_claimed_nodes() {
    let fx = Fixture::new(&[
        node(
            "ab-keep01",
            json!({"cwd": "/home/u/code/fno", "project": "fno"}),
        ),
        node("ab-leak01", json!({"cwd": "/tmp/pytest-of-x/pytest-1/p"})),
        node("ab-leak02", json!({"cwd": "/tmp/pytest-of-x/pytest-9/p"})),
        node(
            "ab-scope01",
            json!({"project": "fno", "cwd": "/home/u/code/fno/wt/x", "blocked_by": ["ab-leak01"]}),
        ),
        node("ab-url0001", json!({"cwd": "/repo/a", "pr_number": 7})),
        node(
            "ab-url0003",
            json!({"cwd": "/repo/a", "pr_number": 7, "pr_url": "https://github.com/other/repo/pull/7"}),
        ),
    ]);
    let claims = || -> Result<BTreeSet<String>, String> { Ok(["ab-leak02".to_string()].into()) };
    let run = Run {
        workspaces: [("fno".to_string(), "/home/u/code/fno".to_string())].into(),
        claims: &claims,
        slugs: [("/repo/a".to_string(), "o/a".to_string())].into(),
        ..Run::default()
    };
    let (code, out) = run.go(&fx, &["--apply", "--no-validity"]);
    assert_eq!(code, 0, "{}", out.stderr);
    let ids: BTreeSet<String> = fx
        .rows()
        .iter()
        .filter_map(|r| entry_id(r).map(str::to_string))
        .collect();
    assert!(!ids.contains("ab-leak01") && ids.contains("ab-leak02"));
    let scoped = fx.row("ab-scope01");
    assert_eq!(scoped["cwd"], "/home/u/code/fno");
    assert!(
        scoped["blocked_by"].as_array().is_none_or(Vec::is_empty),
        "dangling ref cleaned"
    );
    assert_eq!(
        fx.row("ab-url0001")["pr_url"],
        "https://github.com/o/a/pull/7"
    );
    assert_eq!(
        fx.row("ab-url0003")["pr_url"],
        "https://github.com/other/repo/pull/7"
    );
    assert!(out
        .stdout
        .contains("pr-url written 1 of 1 | pr-url unresolvable 0"));
    assert!(out.stdout.contains("re-scoped 1 | pruned 1 |"));
    assert!(out.stdout.contains("skipped-claimed 1"));
    let report = fx.history().last().unwrap()["report"].clone();
    assert_eq!(
        (report["applied"].clone(), report["complete"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(report["incomplete_leg"], Value::Null);

    // Idempotent: the second pass finds nothing left to do.
    let (_, again) = run.go(&fx, &["--apply", "--json", "--no-validity"]);
    let payload: Value = serde_json::from_str(&again.stdout).unwrap();
    assert_eq!(payload["rescope"]["candidates"], json!([]));
    assert_eq!(payload["pr_url_backfill"]["applied"], json!([]));
}

#[test]
fn apply_refuses_when_claim_state_is_unreadable() {
    let fx = Fixture::new(&[node(
        "ab-leak03",
        json!({"cwd": "/tmp/pytest-of-x/pytest-9/p"}),
    )]);
    let blind = || -> Result<BTreeSet<String>, String> { Err("claims unavailable".into()) };
    let run = Run {
        claims: &blind,
        ..Run::default()
    };
    let (code, out) = run.go(&fx, &["--apply"]);
    assert_eq!(code, 1);
    assert!(out.stderr.contains("live claim state is unavailable"));
    assert_eq!(fx.rows().len(), 1, "a refused apply mutates nothing");
    // A report degrades to no claims rather than refusing.
    assert_eq!(run.go(&fx, &["--no-validity"]).0, 0);
}

#[test]
fn repo_moved_rewrites_every_row_and_prints_a_dry_count() {
    let fx = Fixture::new(&[
        node(
            "ab-mov0001",
            json!({"pr_number": 9, "pr_url": "https://github.com/fnodotdev/fno/pull/9"}),
        ),
        // A closed row: the number-reuse case the pass may not skip.
        node(
            "ab-mov0002",
            json!({"status": "done", "completed_at": "2026-11-01T00:00:00Z",
                   "pr_number": 12, "pr_url": "https://github.com/fnodotdev/fno/pull/12"}),
        ),
        node(
            "ab-mov0003",
            json!({"pr_number": 3, "pr_url": "https://github.com/fnodotdev/fno/pull/3",
                   "additional_prs": [
                       {"number": 44, "url": "https://github.com/fnodotdev/fno/pull/44", "history": true},
                       {"number": 45, "url": "https://github.com/other/repo/pull/45"}]}),
        ),
        // A url-less row whose cwd resolves to the moved repo: filled.
        node("ab-mov0004", json!({"pr_number": 4, "cwd": "/repo/a"})),
        // A url-less row of an unmoved repo: left for the regular backfill.
        node(
            "ab-mov0005",
            json!({"pr_number": 5, "cwd": "/repo/unknown"}),
        ),
        // A row of a repo the spec does not name.
        node(
            "ab-mov0006",
            json!({"pr_number": 7, "pr_url": "https://github.com/other/repo/pull/7"}),
        ),
    ]);
    let run = Run {
        slugs: [("/repo/a".to_string(), "fnodotdev/fno".to_string())].into(),
        ..Run::default()
    };

    // Dry run: the count prints, nothing writes.
    let (code, out) = run.go(
        &fx,
        &[
            "--repo-moved",
            "fnodotdev/fno=fnodotdev/fno-workshop",
            "--no-validity",
        ],
    );
    assert_eq!(code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("repo-moved: 5"), "{}", out.stdout);
    assert!(out.stdout.contains("dry run"), "{}", out.stdout);
    assert_eq!(
        fx.row("ab-mov0001").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno/pull/9"),
        "a dry run writes nothing"
    );

    // Apply: open, closed, additional (history-marked included) and the
    // resolvable url-less rows move; foreign rows stay.
    let (code, out) = run.go(
        &fx,
        &[
            "--apply",
            "--repo-moved",
            "fnodotdev/fno=fnodotdev/fno-workshop",
            "--no-validity",
        ],
    );
    assert_eq!(code, 0, "{}", out.stderr);
    assert_eq!(
        fx.row("ab-mov0001").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/9")
    );
    assert_eq!(
        fx.row("ab-mov0002").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/12")
    );
    let moved3 = fx.row("ab-mov0003");
    assert_eq!(
        moved3.get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/3")
    );
    let extras = moved3
        .get("additional_prs")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(
        extras
            .first()
            .and_then(|e| e.get("url"))
            .and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/44")
    );
    assert_eq!(
        extras.first().and_then(|e| e.get("history")),
        Some(&json!(true)),
        "record-keeping flags survive"
    );
    assert_eq!(
        extras
            .get(1)
            .and_then(|e| e.get("url"))
            .and_then(Value::as_str),
        Some("https://github.com/other/repo/pull/45")
    );
    assert_eq!(
        fx.row("ab-mov0004").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/4")
    );
    let untouched = fx.row("ab-mov0005");
    assert_eq!(
        untouched.get("pr_url"),
        Some(&Value::Null),
        "unresolvable cwd stays url-less"
    );
    assert_eq!(
        fx.row("ab-mov0006").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/other/repo/pull/7")
    );
    assert!(out.stdout.contains("repo-moved: 5"), "{}", out.stdout);
    assert!(!out.stdout.contains("dry run"), "{}", out.stdout);

    // Idempotent: the second pass finds nothing left to move.
    let (_, again) = run.go(
        &fx,
        &[
            "--apply",
            "--repo-moved",
            "fnodotdev/fno=fnodotdev/fno-workshop",
            "--json",
        ],
    );
    let payload: Value = serde_json::from_str(&again.stdout).unwrap();
    let moved = payload.get("repo_moved").expect("repo_moved block");
    assert_eq!(moved.get("applied"), Some(&json!([])));
    assert_eq!(moved.get("candidates"), Some(&json!(0)));
}

#[test]
fn repo_moved_refuses_a_malformed_spec() {
    let fx = Fixture::new(&[node("ab-mov0007", json!({}))]);
    let tail: Vec<String> = ["--repo-moved", "junk"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let err = parse_flags(&tail).unwrap_err();
    assert_eq!(err, 2);
    assert_eq!(fx.rows().len(), 1);
}

#[test]
fn repo_moved_floor_keeps_rows_below_min_number() {
    let fx = Fixture::new(&[
        // New-repo rows: the numbers a cutover must leave alone.
        node(
            "ab-mov1001",
            json!({"pr_number": 4, "pr_url": "https://github.com/fnodotdev/fno/pull/4"}),
        ),
        node(
            "ab-mov1002",
            json!({"pr_number": 26, "pr_url": "https://github.com/fnodotdev/fno/pull/26"}),
        ),
        // Old-repo rows: at or above the floor, they move.
        node(
            "ab-mov1003",
            json!({"pr_number": 4038, "pr_url": "https://github.com/fnodotdev/fno/pull/4038"}),
        ),
        node(
            "ab-mov1004",
            json!({"pr_url": "https://github.com/fnodotdev/fno/pull/4800", "pr_number": 4800,
                   "additional_prs": [
                       {"number": 4789, "url": "https://github.com/fnodotdev/fno/pull/4789", "history": true},
                       {"number": 4, "url": "https://github.com/fnodotdev/fno/pull/4"}]}),
        ),
        // Url-less fills respect the floor in both directions.
        node("ab-mov1005", json!({"pr_number": 9, "cwd": "/repo/a"})),
        node("ab-mov1006", json!({"pr_number": 4801, "cwd": "/repo/a"})),
    ]);
    let run = Run {
        slugs: [("/repo/a".to_string(), "fnodotdev/fno".to_string())].into(),
        ..Run::default()
    };

    // Dry run: the count and payload name the floor, nothing writes.
    let (code, out) = run.go(
        &fx,
        &[
            "--repo-moved",
            "fnodotdev/fno=fnodotdev/fno-workshop",
            "--min-number",
            "27",
            "--json",
            "--no-validity",
        ],
    );
    assert_eq!(code, 0, "{}", out.stderr);
    let payload: Value = serde_json::from_str(&out.stdout).unwrap();
    let moved = payload.get("repo_moved").expect("repo_moved block");
    assert_eq!(moved.get("min_number"), Some(&json!(27)));
    assert_eq!(moved.get("candidates"), Some(&json!(4)));
    assert_eq!(
        fx.row("ab-mov1003").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno/pull/4038"),
        "a dry run writes nothing"
    );

    // Apply: rows at or above the floor move in every leg; the new-repo
    // rows stay.
    let (code, out) = run.go(
        &fx,
        &[
            "--apply",
            "--repo-moved",
            "fnodotdev/fno=fnodotdev/fno-workshop",
            "--min-number",
            "27",
            "--no-validity",
        ],
    );
    assert_eq!(code, 0, "{}", out.stderr);
    assert_eq!(
        fx.row("ab-mov1001").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno/pull/4"),
        "a row under the floor stays"
    );
    assert_eq!(
        fx.row("ab-mov1002").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno/pull/26"),
        "a row under the floor stays"
    );
    assert_eq!(
        fx.row("ab-mov1003").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/4038")
    );
    let moved4 = fx.row("ab-mov1004");
    assert_eq!(
        moved4.get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/4800"),
        "a primary url at or above the floor moves"
    );
    let extras = moved4
        .get("additional_prs")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(
        extras
            .first()
            .and_then(|e| e.get("url"))
            .and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/4789"),
        "an additional row at or above the floor moves"
    );
    assert_eq!(
        extras
            .get(1)
            .and_then(|e| e.get("url"))
            .and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno/pull/4"),
        "an additional row under the floor stays"
    );
    assert_eq!(
        fx.row("ab-mov1005").get("pr_url").and_then(Value::as_str),
        None,
        "a url-less fill under the floor stays url-less"
    );
    assert_eq!(
        fx.row("ab-mov1006").get("pr_url").and_then(Value::as_str),
        Some("https://github.com/fnodotdev/fno-workshop/pull/4801"),
        "a url-less fill at or above the floor fills"
    );
    assert!(out.stdout.contains("floor 27"), "{}", out.stdout);
}

#[test]
fn repo_moved_floor_needs_repo_moved_and_a_positive_number() {
    for tail in [
        vec!["--min-number", "27"],
        vec!["--min-number", "abc"],
        vec!["--min-number", "0"],
        vec!["--min-number"],
    ] {
        let tail: Vec<String> = tail.into_iter().map(str::to_string).collect();
        assert_eq!(parse_flags(&tail).unwrap_err(), 2, "{tail:?}");
    }
    let ok: Vec<String> = ["--repo-moved", "a/b=c/d", "--min-number", "27"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let flags = parse_flags(&ok).unwrap();
    assert_eq!(flags.min_number, Some(27));
}
