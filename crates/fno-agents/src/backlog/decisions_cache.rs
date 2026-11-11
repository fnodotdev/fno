//! One cached decisions read per process, answering the merge gate's
//! repeated `fno backlog decisions` argv in process.
//!
//! Every spawned CLI re-read the whole decision store and, for a subject
//! that is no wellformed node id (`pr-hold:`, `merge-grant:`, waiver
//! shapes), also loaded the whole machine graph through `soft_entries`,
//! then read the store a second time for near misses. One decide pass
//! spawned six such reads and the decide+effect double pass repeated
//! them; under fleet contention each became a 60-80 s core, which is the load spike this module removes.
//! This module reads the store once per store version (mtime and size
//! keyed, so a store that changes on disk rebuilds on the next query)
//! and renders the payload shape the CLI stdout carries. The fields no
//! gate parser reads (`plan_rulings`, the standing-law block) stay
//! unset: `operator_law_rows`, `user_ruling_rows` and `held()` consume
//! the `decisions` array alone.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// The argv shapes the gate asks: `backlog decisions [subject]
/// [--lane L] [--state S] [--limit N] [--subject S] --json`. `None`
/// when `args` is not that shape: an unknown flag, an `--output` ask,
/// or a missing `--json` falls through to the real CLI, which renders
/// or refuses exactly as today.
struct Query<'a> {
    subject: Option<&'a str>,
    lane: Option<&'a str>,
    state: Option<&'a str>,
    limit: u32,
}

fn parse_query(args: &[String]) -> Option<Query<'_>> {
    if args.len() < 2
        || args.first().map(String::as_str) != Some("backlog")
        || args.get(1).map(String::as_str) != Some("decisions")
    {
        return None;
    }
    let Some(rest) = args.get(2..) else {
        return None;
    };
    // The machine-output ask rides the shared contract helpers, which
    // accept both CLI spellings; a human-shaped argv falls through to the
    // CLI, which renders or refuses exactly as today.
    if !crate::json_output::requested(args) {
        return None;
    }
    let mut subject: Option<&str> = None;
    let mut lane = None;
    let mut state = None;
    let mut limit = 0u32;
    let mut i = 0usize;
    while let Some(arg) = rest.get(i) {
        let arg = arg.as_str();
        if crate::json_output::is_flag(arg) {
            i += 1;
            continue;
        }
        match arg {
            "--lane" => {
                lane = rest.get(i + 1).map(String::as_str);
                i += 2;
            }
            "--state" => {
                state = rest.get(i + 1).map(String::as_str);
                i += 2;
            }
            "--limit" => {
                // A value that does not parse falls through to the CLI,
                // which refuses it with usage; answering unlimited here
                // would read milder than the command the caller named.
                match rest.get(i + 1).and_then(|v| v.parse().ok()) {
                    Some(parsed) => limit = parsed,
                    None => return None,
                }
                i += 2;
            }
            "--subject" => {
                // Deprecated alias form: its value is the subject.
                if subject.is_some() {
                    return None;
                }
                subject = rest.get(i + 1).map(String::as_str);
                i += 2;
            }
            _ if arg.starts_with("--") => return None,
            _ => {
                if subject.is_some() {
                    return None;
                }
                subject = Some(arg);
                i += 1;
            }
        }
    }
    Some(Query {
        subject,
        lane,
        state,
        limit,
    })
}

/// Store identity behind the cache: every file the store read touches,
/// validated by mtime and size per call. The WAL sidecars ride the key
/// because a WAL-mode store writes there first: the db file's own
/// mtime and size can hold still across an append, and the authority
/// recheck a long decide owes must see a decision that lands mid-process.
#[derive(Debug, PartialEq)]
struct StoreKey {
    graph: PathBuf,
    db_meta: Option<(SystemTime, u64)>,
    db_wal_meta: Option<(SystemTime, u64)>,
    db_shm_meta: Option<(SystemTime, u64)>,
    journal_meta: Option<(SystemTime, u64)>,
    store_meta: Option<(SystemTime, u64)>,
    store_wal_meta: Option<(SystemTime, u64)>,
}

fn file_meta(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn wal_metas(db: &Path) -> (Option<(SystemTime, u64)>, Option<(SystemTime, u64)>) {
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = db.as_os_str().to_os_string();
    shm.push("-shm");
    (file_meta(Path::new(&wal)), file_meta(Path::new(&shm)))
}

fn store_key(graph: &Path, jsonl: &Path) -> StoreKey {
    let db = crate::backlog::database_path(graph);
    let (db_wal_meta, db_shm_meta) = wal_metas(&db);
    let store = crate::event_store::store_path(jsonl);
    let (store_wal_meta, _) = wal_metas(&store);
    StoreKey {
        graph: graph.to_path_buf(),
        db_meta: file_meta(&db),
        db_wal_meta,
        db_shm_meta,
        journal_meta: file_meta(jsonl),
        store_meta: file_meta(&store),
        store_wal_meta,
    }
}

struct CachedStore {
    key: StoreKey,
    rows: Arc<Vec<Value>>,
    damaged: usize,
}

fn cached_store() -> &'static Mutex<Option<CachedStore>> {
    static CACHE: OnceLock<Mutex<Option<CachedStore>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// One store read's outcome, typed rather than a string error: the rows
/// with their damaged count, or the reason the store could not be read.
enum StoreRead {
    Rows(Arc<Vec<Value>>, usize),
    Unreadable(String),
}

/// The store rows for this process, admitted and damaged-counted, read
/// once per store version. A store that changed on disk rebuilds here,
/// so a long-lived gate process never answers from stale rows.
fn store_rows() -> StoreRead {
    let graph = crate::graph_get::default_graph_path();
    let jsonl = crate::decision_index::default_state_path("decisions.jsonl");
    let key = store_key(&graph, &jsonl);
    let Ok(mut guard) = cached_store().lock() else {
        return StoreRead::Unreadable("the store cache lock poisoned".to_string());
    };
    if let Some(hit) = guard.as_ref() {
        if hit.key == key {
            return StoreRead::Rows(hit.rows.clone(), hit.damaged);
        }
    }
    match crate::decision_index::read_store_rows(&graph, &jsonl) {
        Ok((rows, damaged)) => {
            let rows = Arc::new(rows);
            *guard = Some(CachedStore {
                key,
                rows: rows.clone(),
                damaged,
            });
            StoreRead::Rows(rows, damaged)
        }
        Err(reason) => StoreRead::Unreadable(reason),
    }
}

/// The graph entries a subject query needs. A scheme-shaped subject
/// (`pr-hold:`, `merge-grant:`, waiver) is no node spelling, so the
/// resolution tiers cannot answer it and the graph read the CLI pays
/// is pure waste; node ids keep the `read_one` fast path and every
/// other spelling keeps the CLI's soft read.
fn entries_for(subject: Option<&str>, lane: Option<&str>) -> Option<Vec<Value>> {
    let subject = subject?;
    let subject = subject.trim();
    if !crate::backlog::node_ref::is_wellformed_node_id(subject)
        && subject.contains(':')
        && lane.is_some_and(|lane| lane != "coord")
    {
        return None;
    }
    super::decisions_cli::soft_entries(subject, lane)
}

/// One cached answer: the payload bytes the CLI stdout would carry, or
/// the unreadable-store line the CLI would print at exit 1. Typed rather
/// than a string error (docs/rust-style.md R6).
pub(crate) enum Answer {
    Bytes(Vec<u8>),
    Unreadable(String),
}

/// The cached answer for one decisions argv, or `None` when `args` is
/// not a cacheable decisions query (the caller spawns the CLI).
pub(crate) fn payload(args: &[String]) -> Option<Answer> {
    let query = parse_query(args)?;
    Some(render(&query))
}

fn render(query: &Query) -> Answer {
    match store_rows() {
        StoreRead::Rows(rows, damaged) => render_rows(&rows, damaged, query),
        StoreRead::Unreadable(reason) => Answer::Unreadable(reason),
    }
}

/// The pure render over given rows: the store seam tests inject against,
/// and the shape the CLI stdout carries for the fields the gate reads.
fn render_rows(rows: &[Value], damaged: usize, query: &Query) -> Answer {
    let mut entries = entries_for(query.subject, query.lane);
    let listing = super::decisions_cli::list_core_over(
        rows.to_vec(),
        damaged,
        &mut entries,
        query.subject,
        query.lane,
        query.state,
        false,
    );
    let uncapped = listing.rows.len();
    let mut decisions = listing.rows;
    if query.limit > 0 && decisions.len() > query.limit as usize {
        decisions.truncate(query.limit as usize);
    }
    let truncated = decisions.len() < uncapped;
    // The near-miss scan reads the cached rows, never a second store
    // read: the rows are the same set the CLI's second read would
    // return within one process lifetime.
    let near = query
        .subject
        .map(|subject| super::decisions_cli::near_miss_subjects(subject, rows, entries.as_deref()))
        .unwrap_or_default();
    let payload = json!({
        "subject": listing.label,
        "decisions": decisions,
        "total": uncapped,
        "truncated": truncated,
        "damaged": listing.damaged,
        "matched_by": Value::Null,
        "near_misses": near
            .iter()
            .map(|(subject, count)| json!({"subject": subject, "count": count}))
            .collect::<Vec<Value>>(),
    });
    match serde_json::to_vec(&payload) {
        Ok(bytes) => Answer::Bytes(bytes),
        Err(reason) => Answer::Unreadable(reason.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_gate_argv_shapes() {
        let held = argv(&[
            "backlog",
            "decisions",
            "pr-hold:o/r#7",
            "--lane",
            "law",
            "--state",
            "live",
            "--json",
        ]);
        let q = parse_query(&held).expect("gate shape parses");
        assert_eq!(q.subject, Some("pr-hold:o/r#7"));
        assert_eq!(q.lane, Some("law"));
        assert_eq!(q.state, Some("live"));
        assert_eq!(q.limit, 0);

        let lane_only = argv(&[
            "backlog",
            "decisions",
            "--lane",
            "law",
            "--state",
            "live",
            "--limit",
            "0",
            "--json",
        ]);
        let q = parse_query(&lane_only).expect("lane listing parses");
        assert_eq!(q.subject, None);
        assert_eq!(q.limit, 0);

        let dash_j = argv(&["backlog", "decisions", "x", "-J"]);
        let q = parse_query(&dash_j).expect("the machine-output twin spelling parses");
        assert_eq!(q.subject, Some("x"));
    }

    #[test]
    fn non_decisions_or_human_argv_answers_none() {
        assert!(parse_query(&argv(&["doctor", "test"])).is_none());
        assert!(
            parse_query(&argv(&["backlog", "decisions", "x", "--lane", "law"])).is_none(),
            "no --json falls through to the CLI"
        );
        assert!(
            parse_query(&argv(&[
                "backlog",
                "decisions",
                "x",
                "--json",
                "--output",
                "f.json"
            ]))
            .is_none(),
            "an --output ask renders through the CLI"
        );
        assert!(
            parse_query(&argv(&["backlog", "decisions", "x", "--wat", "--json"])).is_none(),
            "an unknown flag refuses at the CLI"
        );
    }

    #[test]
    fn scheme_subjects_skip_the_graph_read_but_node_ids_keep_it() {
        // The branch is structural: scheme-shaped subjects answer with no
        // entries, and the function delegates everything else. Assert the
        // skip arm directly through a colon subject on the law lane.
        let entries = entries_for(Some("pr-hold:o/r#7"), Some("law"));
        assert_eq!(
            entries, None,
            "scheme subject on the law lane skips the read"
        );
        // A colon-free non-id subject keeps the CLI's soft read (which may
        // load the graph); only the shape decision is asserted here, so run
        // it only when a graph exists.
        if crate::graph_get::default_graph_path().exists() {
            let entries = entries_for(Some("no-such-subject-shape"), Some("law"));
            assert!(entries.is_some(), "colon-free subject keeps the soft read");
        }
    }

    #[test]
    fn store_key_changes_when_a_store_file_moves() {
        let dir = std::env::temp_dir().join(format!("x51c4-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let graph = dir.join("graph.db");
        std::fs::write(&graph, b"one").unwrap();
        let jsonl = dir.join("decisions.jsonl");
        let before = store_key(&graph, &jsonl);
        std::fs::write(&graph, b"one-two-three").unwrap();
        let after = store_key(&graph, &jsonl);
        assert_ne!(before, after, "a rewritten store file invalidates the key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_render_parity_with_the_full_read() {
        // A wide row, a retraction, and a superseder: the lifecycle
        // derivation must answer identically through the cached render and
        // the full read over the same rows.
        let rows = vec![
            json!({
                "_event_type": "operator_decision",
                "decision_id": "d-00000001",
                "subject": "pr-hold:o/r#7",
                "decision": "hold",
                "authority_source": "operator",
                "scope": "global",
                "ts": "2026-11-10T00:00:01Z",
                "text": "veto this merge",
            }),
            json!({
                "_event_type": "decision_retracted",
                "target_decision_id": "d-00000001",
                "retraction_id": "r-00000001",
                "ts": "2026-11-10T00:00:02Z",
                "reason": "released",
            }),
            json!({
                "_event_type": "operator_decision",
                "decision_id": "d-00000002",
                "subject": "pr-hold:o/r#7",
                "decision": "hold",
                "authority_source": "operator",
                "scope": "global",
                "supersedes": "d-00000001",
                "ts": "2026-11-10T00:00:03Z",
                "text": "hold again",
            }),
            json!({
                "_event_type": "operator_decision",
                "decision_id": "d-00000003",
                "subject": "unrelated-subject",
                "decision": "note",
                "authority_source": "operator",
                "scope": "global",
                "ts": "2026-11-10T00:00:04Z",
            }),
        ];
        let held = argv(&[
            "backlog",
            "decisions",
            "pr-hold:o/r#7",
            "--lane",
            "law",
            "--state",
            "live",
            "--json",
        ]);
        let query = parse_query(&held).unwrap();
        let Answer::Bytes(cached) = render_rows(&rows, 0, &query) else {
            panic!("the fixture's readable rows render as bytes");
        };
        #[derive(serde::Deserialize)]
        struct RenderedDecisions {
            decisions: Vec<Value>,
        }
        let rendered: RenderedDecisions = serde_json::from_slice(&cached).unwrap();
        let mut no_entries: Option<Vec<Value>> = None;
        let full = crate::backlog::decisions_cli::list_core_over(
            rows.clone(),
            0,
            &mut no_entries,
            Some("pr-hold:o/r#7"),
            Some("law"),
            Some("live"),
            false,
        );
        assert_eq!(
            rendered.decisions, full.rows,
            "the cached render answers the same rows the full read derives"
        );
    }
}
