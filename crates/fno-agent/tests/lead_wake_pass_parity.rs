//! parity-stage: characterization
//! parity-oracle: fno.pr_watch._lead_wake.run_lead_wake
//!
//! Characterization for the daemon lead-wake arm's refill-respawn leg. The
//! goldens under tests/golden/lead_wake_pass/ were captured on 2026-10-27
//! from the Python pr-watch phase over the injected-fn fixture shapes of
//! cli/tests/unit/test_lead_wake_phase.py, before the phase was deleted.
//! Capture mode refuses: a golden can only be captured while the old leg
//! runs.
//!
//! The temp root folds to `<ROOT>` and the space slug to `<ROOTSLUG>`. The
//! pass runs in-process against the same seams the Python tests injected:
//! truth, unread mail, the answered records, the answer projection, the
//! graph rows, the scope read, dispatch, ask, emit, the clock and the tick
//! budget. The registry, manifests and sidecars are real files, so the
//! ledger ops (`admit-wake` under the manifest lock) run for real.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use common::golden_dir;
use fno_agent::lead_wake_triggers::{
    run_with, ScopeRead, Seams, TriggerConfig, TriggerOutcome, TruthAnswer,
};
use serde_json::{json, Value};

mod common;

/// 2026-08-29T12:00:00Z, the fixtures' fixed clock.
const NOW: i64 = 1788004800;
const NOW_ISO: &str = "2026-08-29T12:00:00Z";
const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const BIRTH: &str = "2026-08-28T12:00:00Z";
const CURSOR_SEED: &str = "2026-08-29T10:00:00Z";
const PROMPT: &str =
    r#"Answer to your question q-ab12cd34 "what does the operator want for epic-x?": ship it."#;
const ANSWER_CURSOR: &str = r#"{"ts":"2026-08-29T11:00:00Z","id":"q-ab12cd34","seq":2}"#;

/// Serializes the env-mutating fixtures. The lib's `claims::test_env_lock`
/// is #[cfg(test)]-gated, so an integration binary carries its own.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores the pins the leg's `space_dir` resolution reads through,
/// holding the shared env lock so a parallel test's pin swap cannot flip
/// the resolution mid-read (the lead_wake tests' guard shape).
struct PinGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl PinGuard {
    fn take(base: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = ["FNO_SPACES_DIR", fno_agent::paths::HOME_ENV]
            .iter()
            .map(|var| (*var, std::env::var_os(var)))
            .collect();
        std::env::set_var("FNO_SPACES_DIR", base.join("spaces"));
        std::env::set_var(fno_agent::paths::HOME_ENV, base.join("home"));
        Self { _lock: lock, saved }
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        for (var, saved) in &self.saved {
            match saved {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }
}

/// One fixture world: a pinned agents home, a spaces root and a project
/// root the registry rows' cwds point at.
struct World {
    tmp: tempfile::TempDir,
    _pins: PinGuard,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let pins = PinGuard::take(tmp.path());
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("spaces")).unwrap();
        fs::create_dir_all(tmp.path().join("proj")).unwrap();
        Self { tmp, _pins: pins }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("proj")
    }

    /// Overwrite the registry with the given row objects and return the
    /// loaded entries.
    fn set_registry(&self, agents: Value) -> Vec<fno_agent::state::RegistryEntry> {
        let path = self.home().join("registry.json");
        fs::write(
            &path,
            json!({
                "schema_version": fno_agent::state::REGISTRY_SCHEMA_VERSION,
                "agents": agents,
            })
            .to_string(),
        )
        .unwrap();
        fno_agent::state::load_registry(&path).unwrap().entries
    }

    /// Write one scope's manifest, fixed created_at so the birth-cursor
    /// seed is deterministic; `extra` carries respawn budget or ledger
    /// lines.
    fn manifest(&self, scope: &str, extra: &str) -> PathBuf {
        let leads = fno_agent::paths::space_dir(&self.root()).join("leads");
        fs::create_dir_all(&leads).unwrap();
        let path = leads.join(format!("{scope}.md"));
        fs::write(
            &path,
            format!(
                "---\nfno_id: wake-fixture\nscope: {scope}\nharness: claude\n\
                 harness_session_id: {SESSION}\ncreated_at: {BIRTH}\n{extra}---\n"
            ),
        )
        .unwrap();
        path
    }

    /// Drop and rewrite one scope's manifest fresh, dropping the ledger
    /// bill (the shape the Python tests' `fresh_manifest=True` produced).
    fn rewrite_manifest(&self, scope: &str, extra: &str) {
        let path = self.manifest(scope, extra);
        let _ = path;
    }

    fn sidecar(&self, scope: &str) -> PathBuf {
        let leads = fno_agent::paths::space_dir(&self.root()).join("leads");
        fs::create_dir_all(&leads).unwrap();
        leads.join(format!("{scope}.wake.json"))
    }

    fn write_sidecar(&self, scope: &str, payload: Value) {
        fs::write(self.sidecar(scope), payload.to_string()).unwrap();
    }

    fn read_sidecar(&self, scope: &str) -> Option<Value> {
        fs::read_to_string(self.sidecar(scope))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    fn wake_log(&self, scope: &str) -> Option<String> {
        let leads = fno_agent::paths::space_dir(&self.root()).join("leads");
        fs::read_to_string(leads.join(format!("{scope}.md.wake.log"))).ok()
    }

    /// The manifest's `wake_times` stamps after a run.
    fn wake_times(&self, scope: &str) -> Option<Vec<String>> {
        let leads = fno_agent::paths::space_dir(&self.root()).join("leads");
        let text = fs::read_to_string(leads.join(format!("{scope}.md"))).ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("wake_times:") {
                let stamps: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Some(stamps);
            }
        }
        None
    }
}

fn row(name: &str, cwd: &Path, short_id: &str, scope: &str) -> Value {
    row_session(name, cwd, short_id, scope, SESSION)
}

/// `row` with an explicit registry session id: `load_registry` refuses two
/// live rows on one session identity, so every multi-row fixture hands each
/// row its own. A row's session never surfaces in the goldens; the
/// manifest's session (the escalation delivery address) stays SESSION.
fn row_session(name: &str, cwd: &Path, short_id: &str, scope: &str, session: &str) -> Value {
    json!({
        "name": name,
        "status": "live",
        "cwd": cwd.display().to_string(),
        "short_id": short_id,
        "role_scope": scope,
        "role_level": 1,
        "created_at": "2026-08-28T12:00:00Z",
        "harness_session_id": session,
    })
}

type TruthStub =
    Arc<dyn Fn(&[String], Option<Instant>) -> (HashMap<String, TruthAnswer>, bool) + Send + Sync>;

fn truth_of(state: &str, reason: &str) -> TruthStub {
    let state = state.to_string();
    let reason = reason.to_string();
    Arc::new(move |handles: &[String], _: Option<Instant>| {
        let answers = handles
            .iter()
            .map(|h| {
                (
                    h.clone(),
                    TruthAnswer {
                        state: state.clone(),
                        reason: reason.clone(),
                    },
                )
            })
            .collect();
        (answers, true)
    })
}

fn truth_done() -> TruthStub {
    truth_of("done", "")
}

/// The fixture scope read: the rows whose `project` is `proj`, with the
/// not-closed count over them (the Python tests' `_scope_of`).
fn scope_of(board: &[Value]) -> Arc<dyn Fn(&str, &[Value]) -> ScopeRead + Send + Sync> {
    let proj: Vec<Value> = board
        .iter()
        .filter(|r| r.get("project").and_then(Value::as_str) == Some("proj"))
        .cloned()
        .collect();
    let ids: Vec<String> = proj
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str))
        .map(String::from)
        .collect();
    let undelivered = proj
        .iter()
        .filter(|r| !fno_agent::graph_store::is_terminal_entry(r))
        .count() as i64;
    Arc::new(move |_scope, _rows| ScopeRead {
        ids: Some(ids.clone()),
        undelivered,
    })
}

/// The collected observations one pass leaves behind, in the golden run's
/// shape.
#[derive(Default, Clone)]
struct Rec {
    events: Arc<Mutex<Vec<Value>>>,
    dispatches: Arc<Mutex<Vec<Value>>>,
    successor: Arc<Mutex<Vec<bool>>>,
    asks: Arc<Mutex<Vec<Value>>>,
    unread_calls: Arc<Mutex<Vec<String>>>,
    /// The holders the truth read was called with, when a case records
    /// them; `None` keeps the field out of the run JSON, as the goldens do.
    truth_probes: Arc<Mutex<Option<Vec<String>>>>,
}

impl Rec {
    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    fn dispatches(&self) -> Vec<Value> {
        self.dispatches.lock().unwrap().clone()
    }

    fn successor(&self) -> Vec<bool> {
        self.successor.lock().unwrap().clone()
    }

    fn asks(&self) -> Vec<Value> {
        self.asks.lock().unwrap().clone()
    }

    fn unread(&self) -> Vec<String> {
        self.unread_calls.lock().unwrap().clone()
    }

    fn truth_probes(&self) -> Option<Vec<String>> {
        self.truth_probes.lock().unwrap().clone()
    }
}

/// The seams one case runs with: recorder seams plus the case's answers.
fn seams(
    _world: &World,
    rows: Vec<fno_agent::state::RegistryEntry>,
    rec: &Rec,
    truth: TruthStub,
    unread_hits: Vec<String>,
    next_answer: Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>,
    entries: Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync>,
    scope_read: Arc<dyn Fn(&str, &[Value]) -> ScopeRead + Send + Sync>,
) -> Seams {
    let unread_calls = Arc::clone(&rec.unread_calls);
    let hits: std::collections::HashSet<String> = unread_hits.into_iter().collect();
    let dispatches = Arc::clone(&rec.dispatches);
    let successor = Arc::clone(&rec.successor);
    let asks = Arc::clone(&rec.asks);
    let events = Arc::clone(&rec.events);
    Seams {
        now: NOW,
        now_iso: NOW_ISO.to_string(),
        registry_rows: rows,
        truth,
        unread: Arc::new(move |address: &str| {
            unread_calls.lock().unwrap().push(address.to_string());
            hits.contains(address)
        }),
        answered: Arc::new(|| Vec::new()),
        next_answer,
        entries,
        scope_read,
        dispatch: Some(Box::new(
            move |target, reason, address, detail, successor_flag| {
                dispatches
                    .lock()
                    .unwrap()
                    .push(json!([target.scope, reason, address, detail,]));
                successor.lock().unwrap().push(successor_flag);
                true
            },
        )),
        ask: Box::new(move |target, count, ceiling, _marker| {
            asks.lock()
                .unwrap()
                .push(json!([target.scope, count, ceiling]));
        }),
        emit: Box::new(move |kind, payload| {
            events.lock().unwrap().push(json!([kind, payload]));
        }),
        seconds_left: None,
        truth_wait_s: 10.0,
    }
}

/// Fold the temp root and the space slug out of every string in the run.
/// `spaces/` sits beside the project root in the temp dir, so the temp dir
/// folds to `<ROOT>` too, the way the capture's one root did.
fn fold(value: &mut Value, root: &Path) {
    match value {
        Value::String(text) => {
            let slug = root.to_string_lossy().replace('/', "-");
            let base = root.parent().unwrap_or(root).to_string_lossy().into_owned();
            *text = text
                .replace(&slug, "<ROOTSLUG>")
                .replace(&root.to_string_lossy().into_owned(), "<ROOT>")
                .replace(&base, "<ROOT>")
                .replace("11111111-2222-3333-4444-555555555555", "<SESSION>");
        }
        Value::Array(items) => items.iter_mut().for_each(|v| fold(v, root)),
        Value::Object(map) => map.values_mut().for_each(|v| fold(v, root)),
        _ => {}
    }
}

/// The run JSON one pass leaves, in the golden's shape.
fn run_json(
    outcome: &TriggerOutcome,
    rec: &Rec,
    world: &World,
    scope: &str,
    wake_log: bool,
) -> Value {
    let summary = if outcome.armed {
        let mut map = serde_json::Map::new();
        map.insert("armed".into(), json!(true));
        map.insert("roles".into(), json!(outcome.roles));
        map.insert("evaluated".into(), json!(outcome.evaluated));
        map.insert("truth_reads".into(), json!(outcome.truth_reads));
        map.insert("woke".into(), json!(outcome.woke));
        map.insert(
            "refused".into(),
            json!(outcome
                .refused
                .iter()
                .map(|(scope, word)| json!({"scope": scope, "refusal": word}))
                .collect::<Vec<_>>()),
        );
        map.insert("note".into(), json!(outcome.note));
        map.insert("team_incomplete".into(), json!(outcome.team_incomplete));
        if outcome.budget_spent {
            map.insert("budget_spent".into(), json!(true));
        }
        Value::Object(map)
    } else {
        json!({"armed": false})
    };
    // The capture omitted absent fields rather than serializing nulls, so
    // `wake_log` (present only for the real-walk case) and `truth_probes`
    // (present only where a case recorded it) stay out when empty.
    let mut run = serde_json::Map::new();
    run.insert("summary".into(), summary);
    run.insert("events".into(), json!(rec.events()));
    run.insert("dispatches".into(), json!(rec.dispatches()));
    run.insert("successor_flags".into(), json!(rec.successor()));
    run.insert("asks".into(), json!(rec.asks()));
    run.insert("unread_calls".into(), json!(rec.unread()));
    if let Some(probes) = rec.truth_probes() {
        run.insert("truth_probes".into(), json!(probes));
    }
    run.insert("sidecar".into(), json!(world.read_sidecar(scope)));
    run.insert("wake_times".into(), json!(world.wake_times(scope)));
    if let Some(log) = wake_log.then(|| world.wake_log(scope)).flatten() {
        run.insert("wake_log".into(), json!(log));
    }
    Value::Object(run)
}

/// Compare one pass against the golden's run at `index`.
fn assert_run(label: &str, index: usize, mut mine: Value, root: &Path) {
    fold(&mut mine, root);
    let key = common::slug(label);
    let text = fs::read_to_string(golden_dir("lead_wake_pass").join(format!("{key}.json")))
        .unwrap_or_else(|e| panic!("[{label}] golden unreadable: {e}"));
    let golden: Value = serde_json::from_str(&text).unwrap();
    let want = &golden["runs"][index];
    assert_eq!(
        &mine, want,
        "[{label}] run {index} differs\ngolden={want}\nmine={mine}"
    );
}

fn base_board(id: &str) -> Value {
    json!({
        "id": id,
        "project": "proj",
        "status": "ready",
        "_kanban_column": "ready",
        "priority": "p1",
    })
}

fn board_a() -> Vec<Value> {
    vec![base_board("x-1")]
}

fn board_c() -> Vec<Value> {
    vec![base_board("x-1"), base_board("x-2"), base_board("x-3")]
}

fn no_answers() -> Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>
{
    Arc::new(|_addresses, _records, cursor| (None, cursor.to_string()))
}

fn answer_due() -> Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>
{
    answer_due_at(ANSWER_CURSOR)
}

fn answer_due_at(
    cursor: &str,
) -> Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync> {
    let cursor = cursor.to_string();
    Arc::new(move |_addresses, _records, _stored| (Some(PROMPT.to_string()), cursor.clone()))
}

fn static_entries(rows: Vec<Value>) -> Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync> {
    Arc::new(move || Ok(rows.clone()))
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[test]
fn answered_escalation_wakes_with_the_answer_as_the_prompt() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    world.write_sidecar("epic-x", json!({"answered_cursor": CURSOR_SEED}));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec![],
            answer_due(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "answered escalation wakes with the answer as the prompt",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

fn armed() -> TriggerConfig {
    TriggerConfig {
        enabled: true,
        ceiling: 32,
        debounce_s: 900,
        backstop_s: 1800,
    }
}

#[test]
fn a_dead_holder_wakes_a_successor_under_the_recorded_role() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_of("unknown", "not-found"),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a dead holder wakes a successor under the recorded role",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn a_dead_holder_at_the_respawn_ceiling_spawns_nothing_and_escalates() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "respawn_count: 4\nrespawn_ceiling: 4\n");
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_of("unknown", "not-found"),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a dead holder at the respawn ceiling spawns nothing and escalates",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn present_holder_states_never_wake() {
    let states = [
        ("working", "working", ""),
        ("watching", "watching", ""),
        ("your-move", "your-move", ""),
        ("stalled", "stalled", ""),
        ("unknown no-records", "unknown", "no-records"),
        ("unknown resolver-error", "unknown", "resolver-error"),
    ];
    for (label, state, reason) in states {
        let world = World::new();
        let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
        world.manifest("epic-x", "");
        let rec = Rec::default();
        let outcome = run_with(
            &armed(),
            seams(
                &world,
                rows,
                &rec,
                truth_of(state, reason),
                // The capture's unread seam answered for every address, so
                // the first sorted probe (the short id) is the hit.
                vec!["aa11bb22".into()],
                no_answers(),
                static_entries(Vec::new()),
                scope_of(&[]),
            ),
        );
        assert_run(
            &format!("present holder {label} never wakes"),
            0,
            run_json(&outcome, &rec, &world, "epic-x", false),
            world.root().as_path(),
        );
    }
}

#[test]
fn the_33rd_wake_is_refused_naming_ceiling_and_asks_once() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    // 32 real bills, ages 1270m..30m at 40m spacing, written oldest first
    // the way the native bill-wake kept the ledger line: all inside 24h,
    // all past the debounce.
    let stamps: Vec<String> = (0..32)
        .map(|i| {
            let secs = NOW - (1270 - 40 * i) * 60;
            stamp(secs)
        })
        .collect();
    world.manifest("epic-x", &format!("wake_times: {}\n", stamps.join(",")));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            // The capture's unread seam answered for every address, so the
            // first sorted probe (the short id) is the hit.
            vec!["aa11bb22".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "the 33rd wake is refused naming ceiling and asks once",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

/// RFC3339 seconds stamp for a fixture epoch.
fn stamp(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

#[test]
fn an_unreadable_registry_wakes_nothing() {
    let world = World::new();
    fs::write(world.home().join("registry.json"), "not json").unwrap();
    let rec = Rec::default();
    let home = fno_agent::paths::AgentsHome::at(world.home());
    let outcome = fno_agent::lead_wake_triggers::run(&home, &world.root(), &armed());
    assert_run(
        "an unreadable registry wakes nothing",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn the_board_diff_names_the_two_added_rows_and_nothing_unchanged() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    // Run 1: the seed pass. The seeded board's backstop bills a wake; the
    // manifest is then rewritten fresh, dropping that bill, the shape the
    // Python test's second `_run` produced.
    let rec = Rec::default();
    let _ = run_with(
        &armed(),
        seams(
            &world,
            rows.clone(),
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(board_a()),
            scope_of(&board_a()),
        ),
    );
    world.rewrite_manifest("epic-x", "");
    let rec2 = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec2,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(board_c()),
            scope_of(&board_c()),
        ),
    );
    assert_run(
        "the board diff names the two added rows and nothing unchanged",
        0,
        run_json(&outcome, &rec2, &world, "epic-x", false),
        world.root().as_path(),
    );
}
