//! Daily filing of repeating machine refusals from the correction journal.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::event_store::{query_events, EventQuery, EventRow};
use crate::refusal_surface_owner::refusal_surface;

const DAILY: Duration = Duration::from_secs(86_400);
const WINDOW_MS: i64 = 14 * 86_400_000;
const PREFIX: &str = "refusal-fold:";

#[derive(Debug)]
struct Pair {
    command: String,
    signature: String,
    bucket: String,
    rows: usize,
    sessions: usize,
    skills: Vec<String>,
    row_ts: Vec<i64>,
    reasons: Vec<(i64, String)>,
    latest_ts: i64,
    seen: BTreeSet<String>,
}

fn origin_key(command: &str, signature: &str) -> String {
    let hash = Sha256::digest(format!("{command}\n{signature}").as_bytes());
    format!("{PREFIX}{}", format!("{hash:x}")[..12].to_string())
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn group_pairs<'a>(
    events: impl IntoIterator<Item = &'a EventRow>,
    now: SystemTime,
    own_session: Option<&str>,
) -> Vec<Pair> {
    let now_ms = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let mut pairs = BTreeMap::<(String, String), Pair>::new();
    for event in events {
        if event.r#type != "agent_corrected"
            || event.ts_ms < now_ms - WINDOW_MS
            || event.ts_ms > now_ms
        {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(&event.line) else {
            continue;
        };
        let Some(data) = row.get("data") else {
            continue;
        };
        let text = |key: &str| data.get(key).and_then(Value::as_str).unwrap_or_default();
        if text("corrected_by") != "machine"
            || text("kind") != "refused"
            || text("refusal_bucket") == "timeout"
            || text("command").trim().is_empty()
            || own_session.is_some_and(|s| s == text("session_id"))
        {
            continue;
        }
        let key = (text("command").to_string(), text("signature").to_string());
        let pair = pairs.entry(key.clone()).or_insert_with(|| Pair {
            command: key.0,
            signature: key.1,
            bucket: String::new(),
            rows: 0,
            sessions: 0,
            skills: Vec::new(),
            row_ts: Vec::new(),
            reasons: Vec::new(),
            latest_ts: i64::MIN,
            seen: BTreeSet::new(),
        });
        if event.ts_ms >= pair.latest_ts {
            pair.latest_ts = event.ts_ms;
            pair.bucket = text("refusal_bucket").to_string();
        }
        pair.rows += 1;
        let session = text("session_id");
        if !session.is_empty() {
            pair.seen.insert(session.to_string());
        }
        pair.sessions = pair.seen.len();
        pair.skills.push(text("active_skill").to_string());
        pair.row_ts.push(event.ts_ms);
        pair.reasons.push((event.ts_ms, text("reason").to_string()));
    }
    pairs
        .into_values()
        .filter(|p| p.rows >= 3 && p.sessions >= 2)
        .collect()
}

fn gate_file(surface: &str, repo: Option<&Path>) -> Option<String> {
    let tag = surface.strip_prefix("gate:")?;
    let mut cmd = Command::new("rg");
    cmd.args(["-l", "-F", "--", &format!("[fno {tag}]"), "hooks", "crates"])
        .stdin(Stdio::null());
    if let Some(repo) = repo {
        cmd.current_dir(repo);
    }
    let found = crate::bounded_cmd::output_with_timeout_result(cmd, 30)
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    Some(found.unwrap_or_else(|| "unresolved".to_string()))
}

fn file_nodes(
    fno_bin: &Path,
    repo: Option<&Path>,
    graph: &Path,
    events: &[EventRow],
    now: SystemTime,
    own_session: Option<&str>,
) -> Result<usize, String> {
    let cwd = repo
        .map(Path::to_path_buf)
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|e| e.to_string())?;
    if crate::agents_config::config_lookup(&cwd, &["friction", "refusal_fold", "file_nodes"])
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return Ok(0);
    }
    let nodes = crate::evals_arm::origin_key_rows(graph, PREFIX)?;
    let mut held = BTreeSet::new();
    let mut completed = BTreeMap::<String, i64>::new();
    for node in nodes {
        let Some(key) = node.get("origin_evidence").and_then(Value::as_str) else {
            continue;
        };
        if node.get("status").and_then(Value::as_str) != Some("done") {
            held.insert(key.to_string());
            continue;
        }
        // An unmeasured completion cannot prove any refusal is newer.
        match node
            .get("completed_at")
            .and_then(Value::as_str)
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        {
            Some(ts) => {
                let slot = completed.entry(key.to_string()).or_insert(i64::MIN);
                *slot = (*slot).max(ts.timestamp_millis());
            }
            None => {
                held.insert(key.to_string());
            }
        }
    }
    let eligible = events.iter().filter(|event| {
        let Ok(row) = serde_json::from_str::<Value>(&event.line) else {
            return false;
        };
        let Some(data) = row.get("data") else {
            return false;
        };
        let field = |name: &str| data.get(name).and_then(Value::as_str).unwrap_or_default();
        let key = origin_key(field("command"), field("signature"));
        !held.contains(&key) && completed.get(&key).map_or(true, |ts| event.ts_ms > *ts)
    });
    let mut pairs = group_pairs(eligible, now, own_session);
    pairs.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then(b.rows.cmp(&a.rows))
            .then(a.command.cmp(&b.command))
            .then(a.signature.cmp(&b.signature))
    });
    let mut filed = 0;
    let mut attempted = 0;
    for mut pair in pairs {
        let surface = refusal_surface(
            Some(&pair.bucket),
            Some(&pair.command),
            &pair.signature,
            &pair.skills,
        );
        if surface == "report-only" {
            continue;
        }
        if attempted == 5 {
            break;
        }
        attempted += 1;
        let key = origin_key(&pair.command, &pair.signature);
        let title = cut(
            &format!(
                "Refused {}x in {} sessions: {}: {}",
                pair.rows,
                pair.sessions,
                pair.command,
                cut(&pair.signature, 120)
            ),
            190,
        );
        pair.reasons.reverse();
        pair.reasons.sort_by(|a, b| b.0.cmp(&a.0));
        let skills: BTreeSet<_> = pair
            .skills
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        let mut details = format!(
            "surface: {surface}\nbucket: {}\nrows: {}\nsessions: {}\n",
            pair.bucket, pair.rows, pair.sessions
        );
        for (i, (_, reason)) in pair.reasons.iter().take(3).enumerate() {
            let label = if i == 0 {
                "reason (latest)"
            } else {
                "reason (sample)"
            };
            details.push_str(&format!("{label}: {}\n", cut(reason, 280)));
        }
        details.push_str(&format!(
            "active skills seen: {}\nread: fno doctor event corrections --by refusal",
            skills.into_iter().collect::<Vec<_>>().join(", ")
        ));
        if let Some(file) = gate_file(&surface, repo) {
            details.push_str(&format!("\ngate file: {file}"));
        }
        let mut cmd = Command::new(fno_bin);
        if let Some(repo) = repo {
            cmd.current_dir(repo);
        }
        cmd.args([
            "backlog",
            "idea",
            &title,
            "--project",
            "fno",
            "--type",
            "bug",
            "--difficulty",
            "low",
            "--source-kind",
            "from_observation",
            "--origin-evidence",
            &key,
            "--separate",
            "-J",
            "--details",
            &details,
        ])
        .stdin(Stdio::null());
        let out = crate::bounded_cmd::output_with_timeout_result(cmd, 30)
            .map_err(|e| format!("refusal fold idea {key}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "refusal fold idea {key}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        filed += 1;
    }
    Ok(filed)
}

fn pass(journal: &Path, now: SystemTime) -> Result<usize, String> {
    let now_ms = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let events = query_events(
        journal,
        &EventQuery {
            types: vec!["agent_corrected".to_string()],
            since_ms: Some(now_ms - WINDOW_MS),
            until_ms: Some(now_ms),
            ..Default::default()
        },
    )?;
    let repo = std::env::var_os("FNO_FOLD_REPO").map(PathBuf::from);
    let graph = crate::backlog::settings::graph_path_at(repo.as_deref());
    let own = std::env::var("FNO_AGENT_SELF").ok();
    file_nodes(
        Path::new("fno"),
        repo.as_deref(),
        &graph,
        &events,
        now,
        own.as_deref(),
    )
}

#[derive(Default)]
pub struct Arm {
    last: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    {
        let mut last = arm.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < DAILY) || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let Some(state) = home.root().parent().map(Path::to_path_buf) else {
            return;
        };
        if let Err(e) = pass(&state.join("events.jsonl"), SystemTime::now()) {
            eprintln!("refusal fold: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    // The fixtures index Values and slice row lists directly; the slicing
    // ratchet would red every fixture edit, so the module holds the allow.
    #![allow(clippy::indexing_slicing)]

    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    const GOOD_TS: &str = "2026-11-09T12:00:00Z";
    const COMMAND: &str = "fno agents mail send";

    fn test_now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_794_232_800)
    }

    fn row(id: usize, command: &str, signature: &str, bucket: &str) -> Value {
        json!({"ts": GOOD_TS, "type": "agent_corrected", "source": "test",
            "data": {"correction_id": format!("corr:{id}"), "corrected_by": "machine",
                "kind": "refused", "rule_version": "v1", "session_id": format!("session-{}", id % 2),
                "command": command, "signature": signature, "refusal_bucket": bucket,
                "reason": format!("reason {id}"), "active_skill": "fno:mail"}})
    }

    fn events(tmp: &TempDir, rows: &[Value]) -> Vec<EventRow> {
        let journal = tmp.path().join("events.jsonl");
        for (i, row) in rows.iter().enumerate() {
            crate::event_store::append_envelope(
                &journal,
                &row.to_string(),
                Some(&format!("row-{i}")),
            )
            .unwrap();
        }
        query_events(
            &journal,
            &EventQuery {
                types: vec!["agent_corrected".into()],
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let bin = crate::write_exec_stub(tmp.path(), "fno-bin.sh",
            "#!/bin/sh\n[ \"$1/$2\" = backlog/idea ] || exit 2\nprintf '%s\\n' CALL >> idea.log\nprintf '%s\\n' \"$@\" >> idea.log\n");
        let graph = tmp.path().join("graph.json");
        crate::graph_store::seed_rows(&graph, &[]).unwrap();
        (tmp, bin, graph)
    }

    fn node(key: &str, status: &str, completed: Option<&str>) -> Value {
        json!({"id": "x-existing", "title": "Existing refusal", "type": "bug",
            "status": status, "priority": "p2", "origin_evidence": key, "completed_at": completed})
    }

    fn calls(tmp: &TempDir) -> usize {
        std::fs::read_to_string(tmp.path().join("idea.log"))
            .unwrap_or_default()
            .lines()
            .filter(|s| *s == "CALL")
            .count()
    }

    #[test]
    fn repeating_pair_files_once_and_the_surface_owner_is_byte_identical() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            std::fs::read(root.join("../fno/src/refusal_surface_core.rs")).unwrap(),
            std::fs::read(root.join("src/eval/refusal_surface_owner.rs")).unwrap()
        );
        // The tag parse pins on a real current fno-tagged line (law_match's
        // caution line), and the buckets the surface rule names by string
        // stay in the table a sibling plan may rename.
        assert_eq!(
            crate::refusal_rate::refusal_signature("[fno law] caution: the flag is gone"),
            "hook:law"
        );
        for named in [
            "usage_error",
            "style_lint",
            "fno_guard",
            "gate_refusal",
            "timeout",
        ] {
            assert!(
                crate::refusal_rate::REFUSAL_BUCKETS
                    .iter()
                    .any(|(name, _)| *name == named),
                "bucket {named} left the table the surface rule names"
            );
        }
        let (tmp, bin, graph) = fixture();
        let rows: Vec<_> = (0..3)
            .map(|i| row(i, COMMAND, "rule n (style)", "style_lint"))
            .collect();
        let events = events(&tmp, &rows);
        let key = origin_key(COMMAND, "rule n (style)");
        assert!(key.starts_with(PREFIX));
        assert_eq!(key.len(), PREFIX.len() + 12);
        assert_eq!(
            file_nodes(&bin, Some(tmp.path()), &graph, &events, test_now(), None).unwrap(),
            1
        );
        assert_eq!(calls(&tmp), 1);
        let log = std::fs::read_to_string(tmp.path().join("idea.log")).unwrap();
        assert!(
            log.contains(&format!("--origin-evidence\n{key}\n")),
            "{log}"
        );
        assert!(log.contains("surface: help:fno agents mail send"), "{log}");
        assert!(log.contains("reason (latest): reason 2") && log.contains("reason (sample):"));
        assert!(log.contains("--project\nfno\n") && log.contains("--separate\n-J\n"));
        crate::graph_store::seed_rows(&graph, &[node(&key, "idea", None)]).unwrap();
        assert_eq!(
            file_nodes(&bin, Some(tmp.path()), &graph, &events, test_now(), None).unwrap(),
            0
        );
        assert_eq!(calls(&tmp), 1);
    }

    #[test]
    fn completed_timeout_cap_and_report_only_pairs_file_zero_zero_five_and_zero() {
        let (tmp, bin, graph) = fixture();
        let rows: Vec<_> = (0..3)
            .map(|i| row(i, COMMAND, "done", "style_lint"))
            .collect();
        let stored = events(&tmp, &rows);
        crate::graph_store::seed_rows(
            &graph,
            &[node(
                &origin_key(COMMAND, "done"),
                "done",
                Some("2026-11-09T13:00:00Z"),
            )],
        )
        .unwrap();
        assert_eq!(
            file_nodes(&bin, Some(tmp.path()), &graph, &stored, test_now(), None).unwrap(),
            0
        );
        assert_eq!(calls(&tmp), 0);

        let (tmp, _, _) = fixture();
        let rows: Vec<_> = (0..3)
            .map(|i| row(i, COMMAND, "timeout", "timeout"))
            .collect();
        assert!(group_pairs(&events(&tmp, &rows), test_now(), None).is_empty());

        let (tmp, bin, graph) = fixture();
        let rows: Vec<_> = (0..18)
            .map(|i| row(i, COMMAND, &format!("signature-{}", i / 3), "usage_error"))
            .collect();
        assert_eq!(
            file_nodes(
                &bin,
                Some(tmp.path()),
                &graph,
                &events(&tmp, &rows),
                test_now(),
                None
            )
            .unwrap(),
            5
        );
        assert_eq!(calls(&tmp), 5);

        let (tmp, bin, graph) = fixture();
        let rows: Vec<_> = (0..3)
            .map(|i| row(i, "gh pr view", "usage", "usage_error"))
            .collect();
        assert_eq!(
            file_nodes(
                &bin,
                Some(tmp.path()),
                &graph,
                &events(&tmp, &rows),
                test_now(),
                None
            )
            .unwrap(),
            0
        );
        assert!(!tmp.path().join("idea.log").exists());
    }

    #[test]
    fn only_new_cross_session_rows_rearm_done_nodes_and_every_other_status_holds() {
        let (tmp, bin, graph) = fixture();
        let mut rows: Vec<_> = (0..6)
            .map(|i| row(i, COMMAND, "repeat", "style_lint"))
            .collect();
        for row in &mut rows[3..] {
            row["ts"] = json!("2026-11-09T13:30:00Z");
        }
        let stored = events(&tmp, &rows);
        let key = origin_key(COMMAND, "repeat");
        for status in ["deferred", "superseded", "in_progress"] {
            let mut existing = node(&key, status, None);
            existing["defer_reason"] = json!("wont_do");
            crate::graph_store::seed_rows(&graph, &[existing]).unwrap();
            assert_eq!(
                file_nodes(&bin, Some(tmp.path()), &graph, &stored, test_now(), None).unwrap(),
                0,
                "{status}"
            );
        }
        crate::graph_store::seed_rows(&graph, &[node(&key, "done", Some("2026-11-09T13:00:00Z"))])
            .unwrap();
        assert_eq!(
            file_nodes(
                &bin,
                Some(tmp.path()),
                &graph,
                &stored[..5],
                test_now(),
                None
            )
            .unwrap(),
            0,
            "two new rows cannot use old rows to qualify"
        );
        assert_eq!(
            file_nodes(&bin, Some(tmp.path()), &graph, &stored, test_now(), None).unwrap(),
            1
        );
        let log = std::fs::read_to_string(tmp.path().join("idea.log")).unwrap();
        assert!(log.contains("rows: 3\nsessions: 2"), "{log}");
        assert!(group_pairs(&stored, test_now(), Some("session-0")).is_empty());
        let group = group_pairs(&stored, test_now(), None).remove(0);
        assert_eq!(group.row_ts.len(), 6);
        assert_eq!(group.skills.len(), 6);

        let (tmp, _, _) = fixture();
        let mut excluded: Vec<_> = (0..15)
            .map(|i| row(i, COMMAND, &format!("excluded-{}", i / 3), "style_lint"))
            .collect();
        for row in &mut excluded[..3] {
            row["data"]["corrected_by"] = json!("user");
        }
        for row in &mut excluded[3..6] {
            row["data"]["kind"] = json!("correct");
        }
        for row in &mut excluded[6..9] {
            row["data"].as_object_mut().unwrap().remove("command");
        }
        for row in &mut excluded[9..12] {
            row["ts"] = json!("2026-10-01T12:00:00Z");
        }
        for row in &mut excluded[12..] {
            row["data"]["session_id"] = json!("single");
        }
        assert!(group_pairs(&events(&tmp, &excluded), test_now(), None).is_empty());
    }
}
