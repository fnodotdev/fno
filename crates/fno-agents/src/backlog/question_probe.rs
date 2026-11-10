//! The question probe arm: a question that names a mechanical external
//! action carries a probe, the daemon reads it on its own 300s stamp, and a
//! probe that reads done after a waiting baseline closes the question with
//! the observed evidence (`probe-done: run 42700851689 completed success`).
//!
//! Q2806 asked the user to approve release run 42700851689; the run
//! completed and v0.4.8rc1 published 3h before a lead closed the question by
//! hand, and the lead reported it as waiting in between. The probe makes the
//! read mechanical for the shapes a machine can answer, and every surviving
//! display line names when a machine last checked.
//!
//! One writer per fact: this arm writes the readings cache, and
//! [`crate::question_sweep::close_questions`] writes the close rows. A
//! failed read is `Unread` and never closes anything.

use crate::paths::AgentsHome;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};

/// Probe cadence: five minutes. The interval bounds discovery lag; the
/// daemon's 5s beat only bounds when the tick first fires.
const QUESTION_PROBE_INTERVAL_SECS: i64 = 300;

static QUESTION_PROBE_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct Gate<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for Gate<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// What the probe waits on. `Run`, `Tag` and `Pr` read GitHub through the
/// asker's cwd; `Node` reads the graph; `Host` resolves DNS; `Worker` folds
/// the census verdict. Only `Run` and `Host` ever derive from free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeKind {
    Run,
    Tag,
    Pr,
    Node,
    Host,
    Worker,
}

impl ProbeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ProbeKind::Run => "run",
            ProbeKind::Tag => "tag",
            ProbeKind::Pr => "pr",
            ProbeKind::Node => "node",
            ProbeKind::Host => "host",
            ProbeKind::Worker => "worker",
        }
    }

    pub(crate) fn parse(word: &str) -> Option<ProbeKind> {
        match word.trim().to_ascii_lowercase().as_str() {
            "run" => Some(ProbeKind::Run),
            "tag" => Some(ProbeKind::Tag),
            "pr" => Some(ProbeKind::Pr),
            "node" => Some(ProbeKind::Node),
            "host" => Some(ProbeKind::Host),
            "worker" => Some(ProbeKind::Worker),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Probe {
    pub kind: ProbeKind,
    pub target: String,
}

/// The state of the thing the probe reads. `Unread` is a failed read: it
/// never closes a question and never feeds the done rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ProbeState {
    #[serde(rename = "waiting")]
    Waiting,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "unread")]
    Unread,
}

/// One cached reading. The first reading that is not `Unread` is the
/// baseline: a probe whose baseline already reads done was done before the
/// ask, which usually means the ask wants something else, so it never
/// auto-closes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProbeReading {
    pub kind: String,
    pub target: String,
    pub baseline: ProbeState,
    pub state: ProbeState,
    #[serde(default)]
    pub evidence: String,
    pub checked_at: i64,
}

/// The row fields the probe fold reads, deserialized once at the boundary.
#[derive(Deserialize, Default)]
struct QuestionCore {
    #[serde(default)]
    probe: Option<String>,
    #[serde(default)]
    question: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    cwd: String,
}

/// The probe the question carries: an explicit `data.probe` key wins
/// (`<kind>:<target>`, or `none` for no probe), otherwise the question and
/// subject text is scanned. Option and context text is never scanned: a run
/// named in an option is context (Q3597 names the rc1 run but asks about
/// the stable promote).
pub(crate) fn derive(data: &Value) -> Option<Probe> {
    derive_core(&QuestionCore::deserialize(data).unwrap_or_default())
}

fn derive_core(core: &QuestionCore) -> Option<Probe> {
    if let Some(explicit) = core.probe.as_deref() {
        let explicit = explicit.trim();
        if explicit.eq_ignore_ascii_case("none") {
            return None;
        }
        if !explicit.is_empty() {
            return Probe::parse(explicit);
        }
    }
    derive_from_text(&core.question, &core.subject)
}

impl Probe {
    /// Parse an explicit `probe: <kind>:<target>` value.
    pub(crate) fn parse(value: &str) -> Option<Probe> {
        let (kind, target) = value.split_once(':')?;
        let kind = ProbeKind::parse(kind)?;
        let target = target.trim();
        (!target.is_empty()).then(|| Probe {
            kind,
            target: target.to_string(),
        })
    }
}

/// Scan the question and subject sentences only. A GitHub Actions run URL
/// (`/actions/runs/<digits>`), `run <9+ digits>` and a `.ts.net` token
/// (port stripped) derive; two different candidates of the same kind yield
/// None, and no other kind ever derives from free text.
pub(crate) fn derive_from_text(question: &str, subject: &str) -> Option<Probe> {
    let text = format!("{question} {subject}");
    let mut run: Option<(usize, String)> = None;
    let mut host: Option<(usize, String)> = None;
    for (pos, raw) in text.split_whitespace().enumerate() {
        let tok = raw.trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?', '"', '\'']);
        if let Some(idx) = tok.find("/actions/runs/") {
            let digits: String = tok[idx + "/actions/runs/".len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !digits.is_empty() {
                run = keep_unique(run, (pos, digits));
                continue;
            }
        }
        if tok.eq_ignore_ascii_case("run") {
            if let Some(next) = text.split_whitespace().nth(pos + 1) {
                let next = next.trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?']);
                let digits = next.chars().take_while(char::is_ascii_digit).count();
                if digits == next.len() && digits >= 9 {
                    run = keep_unique(run, (pos, next.to_string()));
                    continue;
                }
            }
        }
        let bare = match tok.rsplit_once(':') {
            Some((left, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                left
            }
            _ => tok,
        };
        if bare.ends_with(".ts.net") && bare.len() > ".ts.net".len() {
            host = keep_unique(host, (pos, bare.to_string()));
        }
    }
    match (run, host) {
        (Some((rp, target)), Some((hp, _))) if rp <= hp => Some(Probe {
            kind: ProbeKind::Run,
            target,
        }),
        (Some((_, target)), None) => Some(Probe {
            kind: ProbeKind::Run,
            target,
        }),
        (None, Some((_, target))) => Some(Probe {
            kind: ProbeKind::Host,
            target,
        }),
        // Two distinct candidates of one kind collapsed to None; both being
        // None falls here too.
        _ => None,
    }
}

/// Keep one candidate per kind: the first seen, unless a different target
/// appears, which poisons the kind (two candidates of one kind yield None).
fn keep_unique(held: Option<(usize, String)>, next: (usize, String)) -> Option<(usize, String)> {
    match held {
        None => Some(next),
        Some((pos, target)) if target == next.1 => Some((pos, target)),
        Some(_) => None,
    }
}

/// The real reading. `cwd` is the asker's cwd, so a GitHub probe reads the
/// asker's repo; an Err is `Unread` (a 404 on a tag is a definitive no, so
/// it reads waiting).
pub(crate) fn read_probe(probe: &Probe, cwd: &str, home: &AgentsHome) -> (ProbeState, String) {
    match probe.kind {
        ProbeKind::Run => run_reading(&probe.target, cwd),
        ProbeKind::Tag => tag_reading(&probe.target, cwd),
        ProbeKind::Pr => pr_reading(&probe.target, cwd),
        ProbeKind::Node => node_reading(&probe.target, cwd),
        ProbeKind::Host => host_reading(&probe.target),
        ProbeKind::Worker => worker_reading(&probe.target, home),
    }
}

#[derive(Deserialize)]
struct RunPayload {
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
}

fn run_reading(target: &str, cwd: &str) -> (ProbeState, String) {
    let path = format!("repos/{{owner}}/{{repo}}/actions/runs/{target}");
    match crate::pr_push::gh_api("gh", Path::new(cwd), &path, &[]) {
        Ok(raw) => match serde_json::from_str::<RunPayload>(&raw) {
            Ok(payload) if payload.status == "completed" => (
                ProbeState::Done,
                format!(
                    "completed {}",
                    payload.conclusion.as_deref().unwrap_or("unknown")
                ),
            ),
            Ok(payload) => (ProbeState::Waiting, payload.status),
            Err(_) => (ProbeState::Unread, String::new()),
        },
        Err(_) => (ProbeState::Unread, String::new()),
    }
}

#[derive(Deserialize)]
struct ReleasePayload {
    #[serde(default)]
    published_at: Option<String>,
}

fn tag_reading(target: &str, cwd: &str) -> (ProbeState, String) {
    let path = format!("repos/{{owner}}/{{repo}}/releases/tags/{target}");
    match crate::pr_push::gh_api("gh", Path::new(cwd), &path, &[]) {
        Ok(raw) => match serde_json::from_str::<ReleasePayload>(&raw) {
            Ok(payload) => (
                ProbeState::Done,
                payload
                    .published_at
                    .unwrap_or_else(|| "published".to_string()),
            ),
            Err(_) => (ProbeState::Unread, String::new()),
        },
        Err(err) if err.contains("Not Found") || err.contains("404") => {
            (ProbeState::Waiting, "no release yet".to_string())
        }
        Err(_) => (ProbeState::Unread, String::new()),
    }
}

fn pr_reading(target: &str, cwd: &str) -> (ProbeState, String) {
    let Ok(pr) = target.parse::<u64>() else {
        return (ProbeState::Unread, "target is not a PR number".to_string());
    };
    match crate::gc_sweep::gh_pr_is_open(pr, cwd) {
        Some(false) => (ProbeState::Done, "merged or closed".to_string()),
        Some(true) => (ProbeState::Waiting, "open".to_string()),
        None => (ProbeState::Unread, String::new()),
    }
}

fn node_reading(target: &str, cwd: &str) -> (ProbeState, String) {
    let graph = crate::org_board::graph_json_path(Path::new(cwd));
    let store = crate::backlog::api::Store::new(&graph);
    let facts = crate::question_sweep::statuses_of(&store);
    match facts.statuses.get(target) {
        Some(status) if status == "done" || status == "superseded" => {
            (ProbeState::Done, status.clone())
        }
        Some(status) => (ProbeState::Waiting, status.clone()),
        None => (ProbeState::Waiting, "no node row".to_string()),
    }
}

fn host_reading(target: &str) -> (ProbeState, String) {
    match (target, 443u16).to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => (ProbeState::Done, addr.ip().to_string()),
            None => (ProbeState::Waiting, "no address".to_string()),
        },
        Err(_) => (ProbeState::Waiting, "does not resolve".to_string()),
    }
}

/// The census fold: the registry row's door verdict reconciled with the
/// transcript probe, exactly what `fno agents liveness` answers. `finished`
/// reads done; `unknown` reads unread, never dead.
fn worker_reading(target: &str, home: &AgentsHome) -> (ProbeState, String) {
    let Ok(reg) = crate::state::load_registry(&home.registry_json()) else {
        return (ProbeState::Unread, String::new());
    };
    use crate::agent_ref::{self, Join, Key};
    let join = match agent_ref::resolve(&reg.entries, Key::Name(target), |_| true) {
        Join::One(row) => Join::One(row),
        Join::Ambiguous => Join::Ambiguous,
        Join::None => match agent_ref::resolve(&reg.entries, Key::Id(target), |_| true) {
            Join::One(row) => Join::One(row),
            other => other,
        },
    };
    let Join::One(row) = join else {
        let reason = if matches!(join, Join::Ambiguous) {
            "ambiguous handle".to_string()
        } else {
            String::new()
        };
        return (ProbeState::Unread, reason);
    };
    let door = crate::row_verdict::fno_verdict(row);
    let vendor = crate::truth_probe::family1_truth_probe(target).map(|p| p.state);
    match crate::row_verdict::reconcile(&door, vendor.as_deref()) {
        crate::row_verdict::RowVerdict::Finished(reason) => (ProbeState::Done, reason),
        crate::row_verdict::RowVerdict::Live(source) => (ProbeState::Waiting, source.to_string()),
        crate::row_verdict::RowVerdict::Unknown(_) => (ProbeState::Unread, String::new()),
    }
}

/// `~/.fno/state/attention/question-probes.json`, mirrored from
/// [`crate::attention_arm::attention_dir`] onto a passed home so the arm and
/// the test drive the same path.
pub(crate) fn cache_path_for(home: &AgentsHome) -> Option<PathBuf> {
    let parent = home.root().parent()?;
    Some(crate::state_layout::place(parent, "attention"))
        .map(|dir| dir.join("question-probes.json"))
}

pub(crate) fn load_cache(path: &Path) -> BTreeMap<String, ProbeReading> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Whole-file atomic rewrite (tmp + rename in the same directory).
pub(crate) fn write_cache(
    path: &Path,
    cache: &BTreeMap<String, ProbeReading>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string(cache).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// The daemon's tick arm: one pass in flight, off the select loop.
pub fn daemon_tick(home: &AgentsHome, now: i64) {
    use std::sync::atomic::Ordering;
    if !QUESTION_PROBE_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        let home = home.clone();
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        tokio::task::spawn_blocking(move || {
            let _gate = Gate(&QUESTION_PROBE_IN_FLIGHT);
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let fno_dir = home
                .root()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".fno"));
            let raw = crate::question_sweep::journals_raw(&fno_dir, &cwd);
            probe_pass_in(&home, &emitter, now, &raw, &read_probe);
        });
    }
}

/// [`daemon_tick`]'s body with the journal rows and the reading injected, so
/// tests drive the stamp, the baseline rule and the closes against fixtures
/// instead of the live journals, graph or GitHub. Returns the number of
/// questions this pass closed.
pub(crate) fn probe_pass_in(
    home: &AgentsHome,
    emitter: &crate::events::EventEmitter,
    now: i64,
    raw: &str,
    read: &dyn Fn(&Probe, &str, &AgentsHome) -> (ProbeState, String),
) -> usize {
    let stamp = home.root().join("question-probe.stamp");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) < QUESTION_PROBE_INTERVAL_SECS {
        return 0;
    }
    let (asked, closed) = crate::needs::open_question_rows(raw);
    let mut qids: Vec<&String> = asked
        .keys()
        .filter(|qid| !closed.contains(*qid))
        .filter(|qid| crate::numbers::canonical(qid) == qid.as_str())
        .collect();
    qids.sort();
    let cache_path = cache_path_for(home);
    let mut cache = cache_path.as_deref().map(load_cache).unwrap_or_default();
    let mut probed = 0usize;
    let mut unread = 0usize;
    let mut closes: Vec<(String, String)> = Vec::new();
    for qid in qids {
        let Some(data) = asked.get(qid.as_str()) else {
            continue;
        };
        let core = QuestionCore::deserialize(data).unwrap_or_default();
        let Some(probe) = derive_core(&core) else {
            continue;
        };
        let (state, evidence) = read(&probe, &core.cwd, home);
        probed += 1;
        if state == ProbeState::Unread {
            unread += 1;
        }
        let entry = cache.entry(qid.clone()).or_insert_with(|| ProbeReading {
            kind: probe.kind.as_str().to_string(),
            target: probe.target.clone(),
            baseline: state,
            state,
            evidence: String::new(),
            checked_at: now,
        });
        entry.kind = probe.kind.as_str().to_string();
        entry.target = probe.target.clone();
        entry.state = state;
        entry.checked_at = now;
        if entry.baseline != ProbeState::Done && state == ProbeState::Done {
            closes.push((
                qid.clone(),
                format!(
                    "probe-done: {} {} {}",
                    probe.kind.as_str(),
                    probe.target,
                    evidence
                ),
            ));
        }
        entry.evidence = evidence;
    }
    let mut errors: Vec<String> = Vec::new();
    if probed > 0 {
        if let Some(path) = cache_path.as_deref() {
            if let Err(e) = write_cache(path, &cache) {
                errors.push(format!("cache write: {e}"));
            }
        }
    }
    let index = crate::provider_cap::questions_path(home);
    let (closed_now, close_errors) =
        crate::question_sweep::close_questions(&index, &closes, now, "question-probe");
    errors.extend(close_errors);
    let outcome = if !errors.is_empty() {
        "partial"
    } else if closed_now.is_empty() {
        "none"
    } else {
        "closed"
    };
    let _ = emitter.emit(
        "question_probe",
        &serde_json::json!({
            "probed": probed,
            "closed": closed_now.len(),
            "unread": unread,
            "outcome": outcome,
        }),
    );
    if errors.is_empty() {
        let _ = std::fs::write(&stamp, now.to_string());
    }
    closed_now.len()
}

/// One display suffix: `(checked 3m ago: run 42700851689 waiting)`,
/// `(checked 3m ago: done before first check: run completed success)`,
/// `(checked 3m ago: unread)` or `(not machine-checkable)`.
pub(crate) fn check_suffix(reading: Option<&ProbeReading>, now: i64) -> String {
    format!("({})", check_detail(reading, now))
}

/// The bare last-check text, no parens: `checked 3m ago: run 42700851689
/// waiting`, `done before first check: ...`, `unread`, or `not
/// machine-checkable`. The questions board joins it into its detail list.
pub(crate) fn check_detail(reading: Option<&ProbeReading>, now: i64) -> String {
    let Some(r) = reading else {
        return "not machine-checkable".to_string();
    };
    let age = now.saturating_sub(r.checked_at);
    let age_text = if age < 90 {
        format!("{age}s")
    } else if age < 3_600 {
        format!("{}m", (age / 60).max(1))
    } else {
        format!("{}h", age / 3_600)
    };
    match (r.state, r.baseline) {
        (ProbeState::Waiting, _) => {
            format!("checked {age_text} ago: {} {} waiting", r.kind, r.target)
        }
        (ProbeState::Done, ProbeState::Done) => format!(
            "checked {age_text} ago: done before first check: {} {} {}",
            r.kind, r.target, r.evidence
        ),
        (ProbeState::Done, _) => format!(
            "checked {age_text} ago: {} {} done: {}",
            r.kind, r.target, r.evidence
        ),
        (ProbeState::Unread, _) => format!("checked {age_text} ago: unread"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- derive -------------------------------------------------------

    #[test]
    fn q2806_derives_the_named_run_once() {
        let data = json!({
            "question": "Approve release run 42700851689 for rc1? https://github.com/fnodotdev/fno/actions/runs/42700851689",
        });
        assert_eq!(
            derive(&data),
            Some(Probe {
                kind: ProbeKind::Run,
                target: "42700851689".to_string()
            })
        );
    }

    #[test]
    fn q3595_derives_the_tailnet_host_with_port_stripped() {
        let data = json!({
            "question": "May the fno-web lead turn Tailscale DNS back on for this laptop? The phone sink posts to m2-mbp.bigeye-truck.ts.net:8443, and that name does not resolve.",
        });
        assert_eq!(
            derive(&data),
            Some(Probe {
                kind: ProbeKind::Host,
                target: "m2-mbp.bigeye-truck.ts.net".to_string()
            })
        );
    }

    #[test]
    fn option_and_context_text_is_never_scanned() {
        // Q3597 names the run only in option and context text; the sentence
        // asks about the stable promote.
        let data = json!({
            "question": "Promote v0.4.8rc1 to stable now?",
            "options": [{"text": "Promote run 42700851689"}],
            "context": {"evidence": "run 42700851689 completed success"},
        });
        assert_eq!(derive(&data), None);
    }

    #[test]
    fn two_distinct_runs_yield_none() {
        let data = json!({
            "question": "Did run 11111111111 finish before run 22222222222 started?",
        });
        assert_eq!(derive(&data), None);
    }

    #[test]
    fn explicit_probe_wins_and_none_disables() {
        let explicit = json!({"question": "nothing mechanical", "probe": "tag:v0.4.8"});
        assert_eq!(
            derive(&explicit),
            Some(Probe {
                kind: ProbeKind::Tag,
                target: "v0.4.8".to_string()
            })
        );
        let none = json!({"question": "approve run 99999999999", "probe": "none"});
        assert_eq!(derive(&none), None);
    }

    #[test]
    fn pr_tag_node_and_worker_never_derive_from_text() {
        let data = json!({
            "question": "Close PR 3212 for worker t-user-docs-background once tag v0.4.1 ships and node x-abc is done?",
        });
        assert_eq!(derive(&data), None);
    }

    // ---- arm ----------------------------------------------------------

    fn tmp_home(tag: &str) -> AgentsHome {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-qprobe-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(p.join("agents"));
        home.ensure_root().unwrap();
        home
    }

    fn row_raw(qid: &str, question: &str) -> String {
        json!({
            "ts": "2026-11-10T00:00:00Z",
            "type": "operator_question",
            "source": "target",
            "data": {"question_id": qid, "question": question, "cwd": "/no-such-repo"},
        })
        .to_string()
    }

    fn reader(
        seq: Vec<(ProbeState, String)>,
    ) -> impl Fn(&Probe, &str, &AgentsHome) -> (ProbeState, String) {
        let cell = std::cell::RefCell::new(seq.into_iter());
        move |probe, _cwd, _home| {
            assert_eq!(probe.kind, ProbeKind::Run);
            cell.borrow_mut()
                .next()
                .unwrap_or((ProbeState::Unread, String::new()))
        }
    }

    #[test]
    fn ac1_second_done_reading_closes_with_the_observed_evidence() {
        let home = tmp_home("ac1");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let raw = row_raw(
            "q-bf79ab2f",
            "Approve release run 42700851689? https://github.com/fnodotdev/fno/actions/runs/42700851689",
        );
        let read = reader(vec![
            (ProbeState::Waiting, "in_progress".to_string()),
            (ProbeState::Done, "completed success".to_string()),
        ]);
        assert_eq!(probe_pass_in(&home, &emitter, 1_000_000, &raw, &read), 0);
        assert_eq!(probe_pass_in(&home, &emitter, 1_000_300, &raw, &read), 1);
        let questions = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["operator_question_closed"],
        );
        assert!(
            questions.contains("q-bf79ab2f")
                && questions.contains("probe-done: run 42700851689 completed success"),
            "{questions}"
        );
    }

    #[test]
    fn ac3_done_on_the_first_reading_sets_the_baseline_and_closes_nothing() {
        let home = tmp_home("ac3");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let raw = row_raw("q-early", "Did release run 99999999999 finish?");
        let read = reader(vec![(ProbeState::Done, "completed success".to_string())]);
        assert_eq!(probe_pass_in(&home, &emitter, 1_000_000, &raw, &read), 0);
        let cache = load_cache(&cache_path_for(&home).unwrap());
        let reading = cache.values().next().expect("baseline row cached");
        assert_eq!(reading.baseline, ProbeState::Done);
        assert_eq!(reading.state, ProbeState::Done);
        let questions = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["operator_question_closed"],
        );
        assert!(!questions.contains("q-early"), "{questions}");
    }

    #[test]
    fn ac4_a_failed_read_is_unread_and_closes_nothing() {
        let home = tmp_home("ac4");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let raw = row_raw("q-err", "Did release run 88888888888 finish?");
        let read = reader(vec![(ProbeState::Unread, String::new())]);
        assert_eq!(probe_pass_in(&home, &emitter, 1_000_000, &raw, &read), 0);
        let cache = load_cache(&cache_path_for(&home).unwrap());
        assert_eq!(
            cache.values().next().map(|r| r.state),
            Some(ProbeState::Unread)
        );
        let events = crate::events::committed_journal_text(&home.events_jsonl());
        assert!(events.contains(r#""unread":1"#), "{events}");
        let questions = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["operator_question_closed"],
        );
        assert!(!questions.contains("q-err"), "{questions}");
    }

    #[test]
    fn the_stamp_bounds_the_pass_and_copies_are_skipped() {
        let home = tmp_home("stamp");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let raw = row_raw("q-copy", "Did release run 77777777777 finish?");
        let read = reader(vec![(ProbeState::Done, "completed success".to_string())]);
        assert_eq!(probe_pass_in(&home, &emitter, 1_000_000, &raw, &read), 0);
        // Inside the interval floor the pass is a no-op even for a fresh row.
        let grown = format!(
            "{raw}\n{}",
            row_raw("q-fresh", "Did release run 66666666666 finish?")
        );
        assert_eq!(probe_pass_in(&home, &emitter, 1_000_010, &grown, &read), 0);
        // A machine copy is the owning machine's probe, never ours.
        let all = format!(
            "{grown}\n{}",
            row_raw("q-1234abcd@mac2", "Did release run 55555555555 finish?")
        );
        let probed = std::cell::Cell::new(0);
        let read_copy = |_probe: &Probe, _cwd: &str, _home: &AgentsHome| {
            probed.set(probed.get() + 1);
            (ProbeState::Done, "completed success".to_string())
        };
        probe_pass_in(&home, &emitter, 1_000_300, &all, &read_copy);
        assert_eq!(
            probed.get(),
            2,
            "the copy is skipped; q-copy and q-fresh are probed, the copy never is"
        );
    }

    // ---- suffix -------------------------------------------------------

    fn reading(
        state: ProbeState,
        baseline: ProbeState,
        evidence: &str,
        checked_at: i64,
    ) -> ProbeReading {
        ProbeReading {
            kind: "run".to_string(),
            target: "42700851689".to_string(),
            baseline,
            state,
            evidence: evidence.to_string(),
            checked_at,
        }
    }

    #[test]
    fn suffix_forms_match_the_contract() {
        let waiting = reading(
            ProbeState::Waiting,
            ProbeState::Waiting,
            "in_progress",
            1_000_000,
        );
        assert_eq!(
            check_suffix(Some(&waiting), 1_000_180),
            "(checked 3m ago: run 42700851689 waiting)"
        );
        let early = reading(
            ProbeState::Done,
            ProbeState::Done,
            "completed success",
            1_000_000,
        );
        assert_eq!(
            check_suffix(Some(&early), 1_000_180),
            "(checked 3m ago: done before first check: run 42700851689 completed success)"
        );
        let unread = reading(ProbeState::Unread, ProbeState::Unread, "", 1_000_000);
        assert_eq!(
            check_suffix(Some(&unread), 1_000_045),
            "(checked 45s ago: unread)"
        );
        assert_eq!(check_suffix(None, 1), "(not machine-checkable)");
    }
}
