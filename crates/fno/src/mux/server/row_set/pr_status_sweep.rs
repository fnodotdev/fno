//! The serve's bounded pr-status sweep. The row assembly notes each PR
//! row's (cwd, pr) key; this task refreshes those keys through the shared
//! `fno do pr status` door at the door's own TTL, at most
//! [`READS_PER_TICK`] reads per [`TICK`], run sequentially, so the daemon
//! adds no gh path the fleet's coalescing cache does not already gate
//! (crates/fno-agent/src/gate/gh_poller.rs is the one-caller-per-account
//! contract this rides; the door's flock cache dedupes across callers).
//! Facts are fail-open: a failed read keeps the previous fact and moves
//! only the attempt stamp, and the row assembly renders whatever the map
//! holds, so one bad read never blanks a row.
//!
//! The stdout contract is one JSON line (`fno do pr status` prints its
//! human summary and warns on stderr); the parse reads stdout only and
//! takes the first line that parses, so order noise degrades to no fact,
//! never to a wrong one.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::proto::{PrFailingCheck, PrStatusFact};

/// How often the sweep wakes. The door's cache TTL is 60s; two reads per
/// 15s tick refresh a dozen rows inside one TTL without burst.
const TICK: Duration = Duration::from_secs(15);
/// A fact older than this is due (the door's own TTL is 60s; refresh just
/// inside it so the row mark never rides a fact the door would have
/// replaced).
const REFRESH_AFTER_S: u64 = 55;
/// A failed read may not retry sooner than this, so a downed gh or an
/// unauthed repo costs at most one attempt per door TTL.
const RETRY_EVERY: Duration = Duration::from_secs(60);
/// Reads per tick: sequential, spawn-blocking, off the core loop.
const READS_PER_TICK: usize = 2;
/// One door read's hard bound; a hung read drops its tick slot, never the
/// loop.
const DOOR_BOUND: Duration = Duration::from_secs(30);
/// The sheet's cap on named failing checks (the plan's Changes 1).
const FAILING_CAP: usize = 3;
/// A fact text field's hard cap; the door already truncates, this bounds
/// the wire against a future door that does not.
const TEXT_CAP: usize = 240;
/// The fact map's hard bound; a fleet never nears it, the bound keeps a
/// runaway key set from growing without end.
const FACTS_CAP: usize = 512;

type Key = (String, u64);

fn facts() -> &'static Mutex<HashMap<Key, PrStatusFact>> {
    static FACTS: OnceLock<Mutex<HashMap<Key, PrStatusFact>>> = OnceLock::new();
    FACTS.get_or_init(Default::default)
}

fn wanted() -> &'static Mutex<HashSet<Key>> {
    static WANTED: OnceLock<Mutex<HashSet<Key>>> = OnceLock::new();
    WANTED.get_or_init(Default::default)
}

fn attempts() -> &'static Mutex<HashMap<Key, Instant>> {
    static ATTEMPTS: OnceLock<Mutex<HashMap<Key, Instant>>> = OnceLock::new();
    ATTEMPTS.get_or_init(Default::default)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The row assembly's one door into the sweep: notes the key as wanted and
/// returns the fact the map currently holds (`None` = no fact yet, which
/// renders as no mark).
pub(super) fn row_fact(cwd: &str, pr: Option<u64>) -> Option<PrStatusFact> {
    let pr = pr?;
    let key = (cwd.to_string(), pr);
    if let Ok(mut set) = wanted().lock() {
        set.insert(key.clone());
    }
    facts().lock().ok().and_then(|f| f.get(&key).cloned())
}

/// The sweep task: wakes every [`TICK`], refreshes the due keys.
pub(crate) fn spawn() {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        loop {
            tick.tick().await;
            sweep_once().await;
        }
    });
}

async fn sweep_once() {
    let due = {
        let wanted = wanted()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let facts = facts()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let attempts = attempts()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        due_keys(&wanted, &facts, &attempts, now_secs())
    };
    for key in due {
        let fact = read_door(&key.0, key.1).await;
        apply_read(key, fact);
    }
    gc_facts();
    if let (Ok(wanted), Ok(mut attempts)) = (wanted().lock(), attempts().lock()) {
        gc_attempts(&wanted, &mut attempts);
    }
}

/// Which keys this tick refreshes: missing facts first, then the stales,
/// never a key a too-recent attempt already covered, capped at
/// [`READS_PER_TICK`]. Free so the gate math tests without a door.
fn due_keys(
    wanted: &HashSet<Key>,
    facts: &HashMap<Key, PrStatusFact>,
    attempts: &HashMap<Key, Instant>,
    now: u64,
) -> Vec<Key> {
    let mut due: Vec<(Key, u64)> = wanted
        .iter()
        .filter_map(|key| {
            if let Some(at) = attempts.get(key) {
                if at.elapsed() < RETRY_EVERY {
                    return None;
                }
            }
            let age = match facts.get(key) {
                Some(fact) => now.saturating_sub(fact.measured_at),
                None => u64::MAX,
            };
            (age >= REFRESH_AFTER_S).then(|| (key.clone(), age))
        })
        .collect();
    // Oldest (or missing) first, so the cap drops the freshest need, never
    // the stalest fact.
    due.sort_by_key(|(_, age)| std::cmp::Reverse(*age));
    due.truncate(READS_PER_TICK);
    due.into_iter().map(|(key, _)| key).collect()
}

/// Land one read: a fact replaces, a failure moves only the attempt stamp.
/// Free so the fail-open rule tests without a door.
fn apply_read(key: Key, fact: Option<PrStatusFact>) {
    if let Ok(mut map) = attempts().lock() {
        map.insert(key.clone(), Instant::now());
    }
    if let Some(fact) = fact {
        if let Ok(mut map) = facts().lock() {
            map.insert(key, fact);
        }
    }
}

/// Drop attempt stamps whose key left the wanted set, so the stamp map
/// tracks the live rows instead of every key the daemon ever saw. Free so
/// the bound tests without a door.
fn gc_attempts(wanted: &HashSet<Key>, attempts: &mut HashMap<Key, Instant>) {
    attempts.retain(|k, _| wanted.contains(k));
}

/// Keep the map bounded: when over [`FACTS_CAP`], drop the stalest facts.
fn gc_facts() {
    let Ok(mut map) = facts().lock() else { return };
    while map.len() > FACTS_CAP {
        let stalest = map
            .iter()
            .min_by_key(|(_, fact)| fact.measured_at)
            .map(|(k, _)| k.clone());
        match stalest {
            Some(k) => {
                map.remove(&k);
            }
            None => break,
        }
    }
}

/// The door's JSON line, the fields this projection reads (typed once,
/// docs/rust-style.md R1; a drifted or missing field reads as its default,
/// never as an error).
#[derive(Deserialize, Default)]
struct DoorLine {
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    settled: bool,
    #[serde(default)]
    pr_state: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    checks: Option<DoorChecks>,
    #[serde(default)]
    failures: Option<Vec<DoorFailure>>,
    #[serde(default)]
    review_coverage: Option<DoorReview>,
    #[serde(default)]
    rounds_used: Option<u32>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Clone, Deserialize, Default)]
struct DoorChecks {
    #[serde(default)]
    pass: Option<u64>,
    #[serde(default)]
    fail: Option<u64>,
    #[serde(default)]
    pending: Option<u64>,
}

#[derive(Clone, Deserialize, Default)]
struct DoorFailure {
    #[serde(default)]
    check: String,
    #[serde(default)]
    step: String,
    #[serde(default)]
    first_error: String,
}

#[derive(Deserialize, Default)]
struct DoorReview {
    #[serde(default)]
    review_state: Option<String>,
}

async fn read_door(cwd: &str, pr: u64) -> Option<PrStatusFact> {
    let exe = std::env::current_exe().ok()?;
    let out = tokio::time::timeout(
        DOOR_BOUND,
        tokio::process::Command::new(exe)
            .args(["do", "pr", "status", &pr.to_string()])
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| serde_json::from_str::<DoorLine>(l).ok())?;
    let now = now_secs();
    Some(fact_from(&line, now))
}

/// The projection: one parsed door line becomes the wire fact. Free so the
/// mapping tests without a door.
fn fact_from(line: &DoorLine, now: u64) -> PrStatusFact {
    let checks = line.checks.clone().unwrap_or_default();
    let failing = line
        .failures
        .clone()
        .unwrap_or_default()
        .into_iter()
        .take(FAILING_CAP)
        .map(|f| PrFailingCheck {
            name: cut(f.check),
            step: cut(f.step),
            first_error: cut(f.first_error),
        })
        .collect();
    PrStatusFact {
        verdict: cut(line.verdict.clone()),
        pass: checks.pass.unwrap_or(0).min(u32::MAX as u64) as u32,
        fail: checks.fail.unwrap_or(0).min(u32::MAX as u64) as u32,
        pending: checks.pending.unwrap_or(0).min(u32::MAX as u64) as u32,
        failing,
        review_state: line
            .review_coverage
            .as_ref()
            .and_then(|r| r.review_state.clone())
            .map(cut),
        review_rounds: line.rounds_used,
        title: line.title.clone().map(cut),
        settled: line.settled,
        pr_state: line.pr_state.clone().map(cut),
        draft: line.draft,
        mergeable: line.mergeable.clone().map(cut),
        measured_at: now,
    }
}

fn cut(text: String) -> String {
    text.chars().take(TEXT_CAP).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real door line, read 2026-11-24, trimmed to the
    /// fields this module reads.
    fn red_line() -> DoorLine {
        serde_json::from_str(
            r#"{"pr":"566","verdict":"red","settled":false,"pr_state":"OPEN","draft":false,
               "mergeable":"MERGEABLE",
               "checks":{"pass":32,"fail":2,"pending":17},
               "failures":[{"check":"Python static correctness","step":"Test declarations are shrink-only","first_error":"test-delta: scope G3 nets +1"},
                           {"check":"ci-required","step":"wait for every run","first_error":"ci-required: FAILED"}],
               "review_coverage":{"coverage":"covered","review_state":"reviewed","reviewed_count":1},
               "rounds_used":2,"title":"the mux board fetch stays inside its deadline"}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_red_line_maps_verdict_counts_failing_and_review() {
        let fact = fact_from(&red_line(), 1000);
        assert_eq!(fact.verdict, "red");
        assert_eq!((fact.pass, fact.fail, fact.pending), (32, 2, 17));
        assert_eq!(fact.failing.len(), 2);
        let first = fact.failing.first().unwrap();
        assert_eq!(first.name, "Python static correctness");
        assert_eq!(first.step, "Test declarations are shrink-only");
        assert_eq!(fact.review_state.as_deref(), Some("reviewed"));
        assert_eq!(fact.review_rounds, Some(2));
        assert_eq!(
            fact.title.as_deref(),
            Some("the mux board fetch stays inside its deadline")
        );
        assert!(!fact.settled);
        assert_eq!(fact.pr_state.as_deref(), Some("OPEN"));
        assert_eq!(fact.mergeable.as_deref(), Some("MERGEABLE"));
        assert!(!fact.draft);
        assert_eq!(fact.measured_at, 1000);
    }

    #[test]
    fn a_line_without_review_coverage_reads_none_and_still_parses() {
        let mut line = red_line();
        line.review_coverage = None;
        let fact = fact_from(&line, 1);
        assert_eq!(fact.review_state, None);
        assert_eq!(fact.verdict, "red");
    }

    #[test]
    fn failing_rows_cap_at_three() {
        let mut line = red_line();
        line.failures = Some((0..5).map(|_| DoorFailure::default()).collect());
        assert_eq!(fact_from(&line, 1).failing.len(), 3);
    }

    #[test]
    fn due_keys_orders_missing_first_then_stale_and_caps() {
        let wanted: HashSet<Key> = ["a", "b", "c"].iter().map(|c| (c.to_string(), 1)).collect();
        let mut facts = HashMap::new();
        facts.insert(
            ("a".to_string(), 1),
            PrStatusFact {
                measured_at: 100,
                ..Default::default()
            },
        );
        facts.insert(
            ("b".to_string(), 1),
            PrStatusFact {
                measured_at: 9_000,
                ..Default::default()
            },
        );
        let now = 10_000;
        let due = due_keys(&wanted, &facts, &HashMap::new(), now);
        // "c" has no fact (age MAX), "a" is stale, "b" is fresh; the cap is
        // 2, so the fresh fact waits.
        assert_eq!(due.len(), 2);
        assert_eq!(due.first(), Some(&("c".to_string(), 1)));
        assert_eq!(due.get(1), Some(&("a".to_string(), 1)));
    }

    #[test]
    fn a_recent_attempt_defers_the_key_even_when_the_fact_is_stale() {
        let wanted: HashSet<Key> = [("a".to_string(), 1)].into_iter().collect();
        let mut facts = HashMap::new();
        facts.insert(
            ("a".to_string(), 1),
            PrStatusFact {
                measured_at: 0,
                ..Default::default()
            },
        );
        let mut attempts = HashMap::new();
        attempts.insert(("a".to_string(), 1), Instant::now());
        assert!(due_keys(&wanted, &facts, &attempts, now_secs()).is_empty());
    }

    #[test]
    fn a_failed_read_keeps_the_previous_fact_and_stamps_the_attempt() {
        let key = ("a".to_string(), 1);
        let old = PrStatusFact {
            verdict: "green".into(),
            measured_at: 1,
            ..Default::default()
        };
        apply_read(key.clone(), Some(old));
        apply_read(key.clone(), None);
        let map = facts().lock().unwrap();
        assert_eq!(map.get(&key).map(|f| f.verdict.as_str()), Some("green"));
        assert!(attempts().lock().unwrap().contains_key(&key));
    }

    #[test]
    fn an_attempt_stamp_dies_when_its_key_leaves_the_wanted_set() {
        let keep = ("a".to_string(), 1);
        let gone = ("b".to_string(), 2);
        let mut attempts = HashMap::new();
        attempts.insert(keep.clone(), Instant::now());
        attempts.insert(gone.clone(), Instant::now());
        let wanted: HashSet<Key> = [keep.clone()].into_iter().collect();
        gc_attempts(&wanted, &mut attempts);
        assert!(attempts.contains_key(&keep));
        assert!(!attempts.contains_key(&gone));
    }
}
