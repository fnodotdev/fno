//! The question-stamp arm: while a first-party operator question is open for
//! a registry row, the row reads Blocked everywhere.
//!
//! Philip's ruling (PR 346 item 2): Blocked comes from the SAME source the
//! sidebar's needs-input mark already reads - the needs projection's open
//! items - one source for glyph, toast and Dock badge. A pane-hosted agent
//! parks on an operator ask, its harness may never publish an inside-leg
//! report (measured: pane `claude` sessions wrote none all window), so the
//! leg-less row fell to liveness-only and no transition ever existed for the
//! app's diff. This arm closes that gap at the source: each pass folds the
//! open operator questions for a row's own cwd (`crate::needs::question_journals`
//! through `crate::attention::project`, the same stores the needs panel
//! reads), matches them to the row's `harness_session_id`, and writes
//! `question_rung` onto the row ONLY when the stamped set changes. The write
//! bumps the registry version, `agent.watch` serves the new doc, the mux
//! server's 1s reader derives the badge through
//! `crate::blocked_rung::question_rung_badge`, and the app's existing
//! Event::Layout diff fires the toast. A closed question drops the field on
//! the next pass, so the badge falls back without a TTL.
//!
//! Cadence and cost: 1s, stat-gated - the journal fold runs only when a
//! journal file's (mtime, len) moved, so an idle pass is three stats plus a
//! registry stat read.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::paths::AgentsHome;

pub const QUESTION_STAMP_INTERVAL_S: u64 = 1;

/// The registry field this arm writes. The projection
/// (`crate::blocked_rung::question_rung_badge`) reads the same name.
pub const RUNG_FIELD: &str = "question_rung";

pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    /// fnv1a of the last stamped set ("name:session" pairs, sorted); `None`
    /// until the first pass, which always writes (or confirms) the truth.
    last_set: Mutex<Option<u64>>,
    /// Journals' (mtime_nanos, len) digest from the last pass; `None` forces
    /// the first fold.
    last_journals: Mutex<Option<u64>>,
}

impl Default for Arm {
    fn default() -> Self {
        Self {
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            last_set: Mutex::new(None),
            last_journals: Mutex::new(None),
        }
    }
}

/// The open-question facts one pass needs, pure so the fold is unit-testable
/// without files: which (row name, harness session id) pairs are open-asked.
pub fn desired_stamps(
    rows: &[serde_json::Value],
    journals_by_cwd: &std::collections::HashMap<String, String>,
    now: u64,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for row in rows {
        if row.get("exited").and_then(serde_json::Value::as_bool) == Some(true) {
            continue;
        }
        let Some(session) = row
            .get("harness_session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let cwd = row
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let Some(text) = journals_by_cwd.get(cwd) else {
            continue;
        };
        let open = crate::attention::project(text, &[], &[], now)
            .into_iter()
            .any(|item| {
                item.state == "open"
                    && item
                        .asker
                        .as_ref()
                        .and_then(|a| a.session_id.as_deref())
                        .is_some_and(|sid| sid == session)
            });
        if open {
            out.insert(format!("{cwd}\u{1f}{session}"));
        }
    }
    out
}

/// One pass, pure over its inputs so tests drive the write. Returns whether
/// the registry changed. `last` is the previously written set's digest.
pub fn apply_stamps(
    home: &AgentsHome,
    rows: &[serde_json::Value],
    journals_by_cwd: &std::collections::HashMap<String, String>,
    now: u64,
    last: &mut Option<u64>,
) -> bool {
    let desired = desired_stamps(rows, journals_by_cwd, now);
    let digest = {
        let mut h: u64 = 0xcbf29ce484222325;
        for pair in &desired {
            for byte in pair.as_bytes() {
                h ^= u64::from(*byte);
                h = h.wrapping_mul(0x100000001b3);
            }
        }
        h
    };
    if last.is_some_and(|prev| prev == digest) {
        return false;
    }
    let changed = crate::runtime::state::update_registry(&home.registry_json(), |r| {
        for entry in r.entries.iter_mut() {
            let session = entry.harness_session_id.as_deref().unwrap_or("");
            let cwd = entry.cwd.as_deref().unwrap_or("");
            let key = format!("{cwd}\u{1f}{session}");
            let wanted = desired.contains(&key) && !session.is_empty();
            let has = entry.extra.get(RUNG_FIELD).is_some_and(|v| !v.is_null());
            if wanted && !has {
                entry.extra.insert(
                    RUNG_FIELD.to_string(),
                    serde_json::json!({
                        "state": "open",
                        "rule": "operator_question",
                        "at": crate::daemon::now_rfc3339_like(),
                    }),
                );
            } else if !wanted && has {
                entry.extra.remove(RUNG_FIELD);
            }
        }
    })
    .is_ok();
    *last = Some(digest);
    changed
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    let interval = Duration::from_secs(QUESTION_STAMP_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let last_set = Arc::clone(&arm.last_set);
    let last_journals = Arc::clone(&arm.last_journals);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let fno_dir = home
            .root()
            .parent()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(".fno"));
        // Stat-gate: the fold costs file reads, so it runs only when a
        // journal moved. The registry read is a stat too (version).
        let mark = {
            let mut h: u64 = 0xcbf29ce484222325;
            let mut cwds: Vec<std::path::PathBuf> = Vec::new();
            let registry = crate::runtime::state::load_registry(&home.registry_json());
            let Ok(registry) = registry else {
                return;
            };
            for entry in &registry.entries {
                let cwd = entry.cwd.as_deref().unwrap_or("");
                let path = if cwd.is_empty() {
                    None
                } else {
                    Some(std::path::PathBuf::from(cwd))
                };
                cwds.push(
                    crate::needs::question_journals(
                        &fno_dir,
                        path.unwrap_or_else(|| fno_dir.clone()),
                    )
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                );
            }
            for path in cwds
                .iter()
                .chain(std::iter::once(&fno_dir.join("questions.jsonl")))
                .chain(std::iter::once(&fno_dir.join("events.jsonl")))
            {
                let Ok(meta) = std::fs::metadata(path) else {
                    continue;
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                for byte in [mtime, meta.len()].map(|v| v.to_le_bytes()) {
                    for b in byte {
                        h ^= u64::from(b);
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
            h
        };
        {
            let mut last = last_journals.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|prev| prev == mark) {
                return;
            }
            *last = Some(mark);
        }
        // Fold, per distinct cwd, only the question rows.
        let registry = crate::runtime::state::load_registry(&home.registry_json());
        let Ok(registry) = registry else {
            return;
        };
        let mut journals_by_cwd: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut cwds: std::collections::BTreeSet<String> = registry
            .entries
            .iter()
            .map(|e| e.cwd.as_deref().unwrap_or("").to_string())
            .collect();
        cwds.insert(String::new());
        for cwd in &cwds {
            let cwd_path = if cwd.is_empty() {
                None
            } else {
                Some(std::path::Path::new(cwd))
            };
            let mut text = String::new();
            for path in
                crate::needs::question_journals(&fno_dir, cwd_path.unwrap_or(fno_dir.as_path()))
            {
                if let Ok(content) = crate::event_store::journal_text_checked(
                    &path,
                    &crate::event_store::EventQuery::of_types(crate::needs::QUESTION_TYPES),
                ) {
                    text.push_str(&content);
                }
            }
            journals_by_cwd.insert(cwd.clone(), text);
        }
        let rows: Vec<serde_json::Value> = registry
            .entries
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or_default())
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut last = last_set.lock().unwrap_or_else(|e| e.into_inner()).take();
        let changed = apply_stamps(&home, &rows, &journals_by_cwd, now, &mut last);
        *last_set.lock().unwrap_or_else(|e| e.into_inner()) = last;
        if changed {
            let journal = crate::loop_runtime::Journal::new_raw(
                home.events_jsonl(),
                crate::daemon::global_events_path(&home),
            );
            let _ = journal.append(
                "question_stamp_applied",
                serde_json::json!({"changed": true}),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, session: &str, cwd: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "harness_session_id": session,
            "cwd": cwd,
            "exited": false,
        })
    }

    fn journal(asker: &str, qid: &str, close: bool) -> String {
        let mut out = format!(
            r#"{{"ts":"2027-01-15T08:00:00Z","type":"operator_question","data":{{"id":"{qid}","title":"t","asker":{{"session_id":"{asker}","harness":"claude"}}}}}}"#
        );
        if close {
            out.push_str(&format!(
                "\n{{\"ts\":\"2027-01-15T08:01:00Z\",\"type\":\"operator_question_closed\",\"data\":{{\"id\":\"{qid}\"}}}}"
            ));
        }
        out
    }

    #[test]
    fn an_open_question_stamps_and_a_close_drops_the_rung() {
        let rows = vec![row("demo", "uuid-1", "/w")];
        let mut journals = std::collections::HashMap::new();
        journals.insert("/w".to_string(), journal("uuid-1", "q-1", false));
        let mut last = None;
        assert!(apply_stamps(&stamp_home(), &rows, &journals, 1, &mut last));
        // Second identical pass: no write.
        assert!(!apply_stamps(&stamp_home(), &rows, &journals, 2, &mut last));
        // The question closes: the set drops, the write lands.
        journals.insert("/w".to_string(), journal("uuid-1", "q-1", true));
        assert!(apply_stamps(&stamp_home(), &rows, &journals, 3, &mut last));
    }

    #[test]
    fn an_asker_mismatch_and_an_exited_row_never_stamp() {
        let rows = vec![
            row("demo", "uuid-1", "/w"),
            serde_json::json!({
                "name": "gone",
                "harness_session_id": "uuid-2",
                "cwd": "/w",
                "exited": true,
            }),
        ];
        let mut journals = std::collections::HashMap::new();
        journals.insert("/w".to_string(), journal("uuid-9", "q-1", false));
        let mut last = None;
        assert!(!apply_stamps(&stamp_home(), &rows, &journals, 1, &mut last));
        // Empty desired set: no registry write ever fires.
        journals.insert("/w".to_string(), String::new());
        assert!(!apply_stamps(&stamp_home(), &rows, &journals, 2, &mut last));
    }

    /// A throwaway home for the write path; the fold itself is pure.
    fn stamp_home() -> AgentsHome {
        use std::sync::atomic::{AtomicU32, Ordering};
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let tail = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = std::path::PathBuf::from(format!("/tmp/fnosbqs{}_{n}_{tail}", std::process::id()));
        let home = AgentsHome::at(&p);
        home.ensure_root().unwrap();
        std::fs::write(
            home.registry_json(),
            r#"{"schema_version": 6, "agents": []}"#,
        )
        .unwrap();
        home
    }
}
