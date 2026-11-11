//! One opencode thread turn, driven by fno over HTTP:
//! `fno-agents-worker --opencode-turn <spec.json>`. The serve lane launches
//! it detached for every turn (launch and steering alike), in the place the
//! `opencode run --attach` writer used to hold. That writer answered every
//! ask itself: `once` under `--dangerously-skip-permissions`, `reject`
//! without it (opencode 1.18.33 run handler), so fno could never answer
//! first. This driver sends the turn and answers each ask through
//! [`crate::thread_permission`] until the session goes idle.
//!
//! A prose turn rides `POST /session/{id}/prompt_async` (204 at once). A
//! slash command rides `POST /session/{id}/command`, which expands the
//! template in the serve and answers only when the turn ends, so it runs on
//! its own thread beside the answer loop. A held ask waits on the serve
//! itself: `fno agents ask <name> --answer` replies to it over HTTP, and the
//! loop sends `reject` when `agents.thread_permission.hold_minutes` (default
//! 30) lapses. An unanswered V1 ask never times out on its own (measured:
//! still pending after 742s).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::opencode_permission::{self as perm, Contract, Pending, Reply, Serve};
use crate::opencode_serve::http_json;
use crate::thread_permission::{self, Answer};

const POLL: Duration = Duration::from_secs(1);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// A command turn holds its HTTP call open for the whole turn.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(12 * 3600);
/// A prompt the serve never marks busy is a turn that ended (or never ran).
const START_GRACE: Duration = Duration::from_secs(60);
/// The serve unreachable this long ends the driver; the session is lost.
const UNREACHABLE_LIMIT: Duration = Duration::from_secs(120);
const DEFAULT_HOLD_MINUTES: u64 = 30;
const MAX_REPLY_TRIES: u32 = 10;

/// Everything one turn needs, written by the serve lane beside the log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnSpec {
    pub base_url: String,
    pub token: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub worker: Option<String>,
    /// The turn text, envelope applied; a leading `/` is a command.
    pub message: String,
    /// `provider/model`, or none for the session's own.
    pub model: Option<String>,
    pub events: PathBuf,
}

/// The worker lane's entry: read the spec, drive the turn, exit 0 when the
/// session went idle.
pub fn run(spec_path: &Path) -> i32 {
    let spec: TurnSpec = match std::fs::read_to_string(spec_path)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
    {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("opencode-turn: spec {}: {e}", spec_path.display());
            return 2;
        }
    };
    // The spec carries the serve token; it is read once.
    let _ = std::fs::remove_file(spec_path);
    let lock = spec_path.with_file_name(format!("{}.lock", spec.session_id));
    let contract = serve_contract(&spec.base_url, &spec.token);
    match drive(
        &spec,
        contract,
        hold_window(&spec.cwd),
        START_GRACE,
        Some(&lock),
        &thread_permission::decide,
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("opencode-turn: {}: {e}", spec.session_id);
            1
        }
    }
}

/// The contract the serve's health version names; unreadable reads V1, the
/// live shared serve's.
pub(crate) fn serve_contract(base_url: &str, token: &str) -> Contract {
    http_json(
        base_url,
        "GET",
        "/global/health",
        None,
        Some(token),
        CALL_TIMEOUT,
    )
    .ok()
    .and_then(|(_, body)| serde_json::from_str::<Value>(&body).ok())
    .and_then(|h| {
        h.get("version")
            .and_then(Value::as_str)
            .map(Contract::of_version)
    })
    .unwrap_or(Contract::V1)
}

fn hold_window(cwd: &Path) -> Duration {
    let minutes = crate::agents_config::config_value_deep(
        cwd,
        &["agents", "thread_permission", "hold_minutes"],
    )
    .and_then(|v| v.as_integer())
    .and_then(|n| u64::try_from(n).ok())
    .unwrap_or(DEFAULT_HOLD_MINUTES);
    Duration::from_secs(minutes * 60)
}

/// `/name rest` -> (`name`, `rest`); prose -> None.
pub(crate) fn command_of(message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (!name.is_empty()).then(|| (name.to_string(), args.trim().to_string()))
}

/// Send the turn, then answer asks until the session is idle.
pub(crate) fn drive(
    spec: &TurnSpec,
    contract: Contract,
    hold: Duration,
    grace: Duration,
    answer_lock: Option<&Path>,
    decide: &dyn Fn(&thread_permission::ThreadAsk, &Path) -> Answer,
) -> Result<(), String> {
    let sid = &spec.session_id;
    let dir = format!(
        "directory={}",
        perm::encode_query_path(&spec.cwd.to_string_lossy())
    );
    let command_done = Arc::new(AtomicBool::new(false));
    let is_command = match command_of(&spec.message) {
        Some((command, arguments)) => {
            let mut body = json!({"command": command, "arguments": arguments});
            if let Some(m) = &spec.model {
                body["model"] = json!(m);
            }
            let (url, token, path, done) = (
                spec.base_url.clone(),
                spec.token.clone(),
                format!("/session/{sid}/command?{dir}"),
                command_done.clone(),
            );
            std::thread::spawn(move || {
                if let Err(e) = http_json(
                    &url,
                    "POST",
                    &path,
                    Some(&body),
                    Some(&token),
                    COMMAND_TIMEOUT,
                ) {
                    eprintln!("opencode-turn: command call: {e}");
                }
                done.store(true, Ordering::SeqCst);
            });
            true
        }
        None => {
            let mut body = json!({"parts": [{"type": "text", "text": spec.message}]});
            if let Some((provider, model)) = spec.model.as_deref().and_then(|m| m.split_once('/')) {
                body["model"] = json!({"providerID": provider, "modelID": model});
            }
            let path = format!("/session/{sid}/prompt_async?{dir}");
            let (status, text) = http_json(
                &spec.base_url,
                "POST",
                &path,
                Some(&body),
                Some(&spec.token),
                CALL_TIMEOUT,
            )?;
            if !(200..300).contains(&status) {
                return Err(format!("prompt_async answered {status}: {text}"));
            }
            false
        }
    };

    // One answerer per session: a steer's driver sends its turn, then waits
    // here while an earlier driver of the same session still answers.
    let _lock = match answer_lock {
        Some(path) => Some(lock_exclusive(path)?),
        None => None,
    };
    let serve = Serve {
        base_url: &spec.base_url,
        token: &spec.token,
        contract,
    };
    let started = Instant::now();
    let mut seen_busy = false;
    let mut last_reach = Instant::now();
    let mut answered: HashSet<String> = HashSet::new();
    let mut held: HashMap<String, (Instant, String)> = HashMap::new();
    // A decided answer whose reply did not land: sent again on the next
    // poll, the same answer, so one failed POST cannot hang the turn. A
    // reply that keeps failing stops after MAX_REPLY_TRIES.
    let mut unsent: HashMap<String, (Answer, u32)> = HashMap::new();
    // The held ids, published beside the lock so `--answer` finds them.
    let held_file = answer_lock.map(|p| p.with_extension("held"));
    let mut published = String::new();
    loop {
        match perm::list_pending(&serve, &spec.cwd, sid) {
            Ok(pending) => {
                last_reach = Instant::now();
                let live: HashSet<&str> = pending.iter().map(|p| p.request_id.as_str()).collect();
                held.retain(|id, _| live.contains(id.as_str()));
                unsent.retain(|id, _| live.contains(id.as_str()));
                for p in &pending {
                    let (answer, tries) = if let Some(retry) = unsent.remove(&p.request_id) {
                        retry
                    } else if let Some((since, qid)) = held.get(&p.request_id) {
                        if since.elapsed() < hold {
                            continue;
                        }
                        let note = format!(
                            "fno held this {} call for the user (question {qid}) and nobody answered in time. Do not retry it. Do the task another way, or say in your reply that this step needs the user.",
                            p.action
                        );
                        held.remove(&p.request_id);
                        (Answer::ModelNote(note), 0)
                    } else if answered.insert(p.request_id.clone()) {
                        let a = decide_one(spec, &serve, p, decide);
                        if let Answer::HoldForUser { question_id } = &a {
                            held.insert(
                                p.request_id.clone(),
                                (Instant::now(), question_id.clone()),
                            );
                        }
                        (a, 0)
                    } else {
                        continue;
                    };
                    let sent = perm::answer(&serve, &spec.cwd, p, &answer, &spec.events);
                    if sent.is_err() && tries + 1 < MAX_REPLY_TRIES {
                        unsent.insert(p.request_id.clone(), (answer, tries + 1));
                    }
                }
            }
            Err(e) if last_reach.elapsed() >= UNREACHABLE_LIMIT => {
                return Err(format!(
                    "serve unreachable for {}s: {e}",
                    UNREACHABLE_LIMIT.as_secs()
                ));
            }
            Err(_) => {}
        }
        if let Some(file) = &held_file {
            let mut ids: Vec<&str> = held.keys().map(String::as_str).collect();
            ids.sort_unstable();
            let now = ids.join("\n");
            if now != published {
                let _ = std::fs::write(file, &now);
                published = now;
            }
        }
        let busy = session_busy(&spec.base_url, &spec.token, &dir, sid);
        seen_busy |= busy == Some(true);
        let ended = if is_command {
            command_done.load(Ordering::SeqCst) && busy != Some(true)
        } else {
            busy == Some(false) && (seen_busy || started.elapsed() >= grace)
        };
        if ended && held.is_empty() {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
}

fn decide_one(
    spec: &TurnSpec,
    serve: &Serve,
    p: &Pending,
    decide: &dyn Fn(&thread_permission::ThreadAsk, &Path) -> Answer,
) -> Answer {
    let user_rule =
        perm::user_rule_asks(&perm::user_rules(serve, &spec.cwd), &p.action, &p.resources);
    let ask = perm::thread_ask(p, &spec.cwd, spec.worker.as_deref(), user_rule);
    decide(&ask, &spec.events)
}

/// Block until this process holds an exclusive flock on `path`. The lock
/// drops with the file handle when the driver exits.
fn lock_exclusive(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("answer lock {}: {e}", path.display()))?;
    // SAFETY: flock on an fd this function owns for the File's lifetime.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "answer lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(file)
}

/// `GET /session/status`: the session is busy (`busy` or `retry`), idle
/// (absent or `idle`), or unknown (the read failed).
fn session_busy(base_url: &str, token: &str, dir: &str, sid: &str) -> Option<bool> {
    let (status, body) = http_json(
        base_url,
        "GET",
        &format!("/session/status?{dir}"),
        None,
        Some(token),
        CALL_TIMEOUT,
    )
    .ok()?;
    if status != 200 {
        return None;
    }
    let map: Value = serde_json::from_str(&body).ok()?;
    let kind = map
        .get(sid)
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str);
    Some(matches!(kind, Some("busy" | "retry")))
}

/// The held ask ids a driver publishes for `session_id` under `turns`.
pub(crate) fn held_ids(turns: &Path, session_id: &str) -> Vec<String> {
    std::fs::read_to_string(turns.join(format!("{session_id}.held")))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Answer a held ask of `session_id` for the user, else the oldest pending:
/// `allow_once` or `deny`. Errors name why nothing was sent.
pub(crate) fn answer_for_user(
    base_url: &str,
    token: &str,
    cwd: &Path,
    session_id: &str,
    choice: &str,
    held: &[String],
) -> Result<String, String> {
    let reply = match choice {
        "allow_once" => Reply::Once,
        "deny" => Reply::Reject("The user denied this call. Do not retry it."),
        other => return Err(format!("--answer takes allow_once or deny, not {other:?}")),
    };
    let serve = Serve {
        base_url,
        token,
        contract: serve_contract(base_url, token),
    };
    let pending = perm::list_pending(&serve, cwd, session_id)?;
    let target = pending
        .iter()
        .find(|p| held.contains(&p.request_id))
        .or(pending.first());
    let Some(first) = target else {
        return Err(format!(
            "no permission ask is pending for session {session_id}"
        ));
    };
    perm::reply(&serve, cwd, first, reply)?;
    Ok(first.request_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencode_permission::tests::Fake;
    use crate::thread_permission::ThreadAsk;

    fn spec(url: &str, message: &str, dir: &Path) -> TurnSpec {
        TurnSpec {
            base_url: url.to_string(),
            token: "t".into(),
            session_id: "ses_a".into(),
            cwd: PathBuf::from("/w"),
            worker: Some("w1".into()),
            message: message.into(),
            model: Some("zai-coding-plan/glm-5.3".into()),
            events: dir.join("events.jsonl"),
        }
    }

    const ONE_ASK: &str = r#"[{"id":"per_1","sessionID":"ses_a","permission":"bash","patterns":["touch /tmp/x"],"metadata":{},"always":[]}]"#;

    #[test]
    fn a_prose_turn_prompts_async_and_the_ladder_answers_each_ask_once() {
        assert_eq!(
            command_of("/fno:target --no-merge x-1"),
            Some(("fno:target".into(), "--no-merge x-1".into()))
        );
        assert_eq!(
            command_of("/fno:whoami"),
            Some(("fno:whoami".into(), String::new()))
        );
        assert_eq!(command_of("[from: op]\n\nhello"), None);
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::start(vec![
            (
                "POST /session/ses_a/prompt_async",
                "204 No Content",
                String::new(),
            ),
            ("GET /permission?directory=/w", "200 OK", ONE_ASK.into()),
            ("POST /permission/per_1/reply", "200 OK", "true".into()),
            ("GET /config", "200 OK", r#"{"permission":{}}"#.into()),
            ("GET /session/status", "200 OK", "{}".into()),
        ]);
        let calls = std::sync::Mutex::new(0);
        let decide = |a: &ThreadAsk, _: &Path| {
            assert_eq!((a.action.as_str(), a.user_rule), ("bash", false));
            *calls.lock().unwrap() += 1;
            Answer::Allow
        };
        // The fake never reads busy, so a zero grace ends the turn at once.
        let s = spec(&fake.url, "do it", dir.path());
        drive(
            &s,
            Contract::V1,
            Duration::from_secs(60),
            Duration::ZERO,
            None,
            &decide,
        )
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), 1, "one decision per ask id");
        let seen = fake.seen.lock().unwrap();
        let prompt = seen
            .iter()
            .find(|(l, _)| l.starts_with("POST /session/ses_a/prompt_async"))
            .unwrap();
        let body: Value = serde_json::from_str(&prompt.1).unwrap();
        assert_eq!(
            body["model"],
            json!({"providerID": "zai-coding-plan", "modelID": "glm-5.3"})
        );
        let replies: Vec<_> = seen
            .iter()
            .filter(|(l, _)| l.starts_with("POST /permission/per_1/reply"))
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&replies[0].1).unwrap(),
            json!({"reply": "once"})
        );
        assert!(!seen.iter().any(|(l, _)| l.contains("opencode run")));
    }

    #[test]
    fn the_user_answers_a_held_ask_and_an_empty_queue_says_so() {
        let fake = Fake::start(vec![
            (
                "GET /global/health",
                "200 OK",
                r#"{"healthy":true,"version":"1.18.33"}"#.into(),
            ),
            ("GET /permission?directory=/w", "200 OK", ONE_ASK.into()),
            ("POST /permission/per_1/reply", "200 OK", "true".into()),
        ]);
        let id = answer_for_user(&fake.url, "t", Path::new("/w"), "ses_a", "deny", &[]).unwrap();
        assert_eq!(id, "per_1");
        let seen = fake.seen.lock().unwrap();
        let body: Value = serde_json::from_str(&seen.last().unwrap().1).unwrap();
        assert_eq!(body["reply"], "reject");
        drop(seen);
        let err = answer_for_user(
            &fake.url,
            "t",
            Path::new("/w"),
            "ses_other",
            "allow_once",
            &[],
        )
        .unwrap_err();
        assert!(err.contains("no permission ask is pending"), "{err}");
        assert!(answer_for_user(&fake.url, "t", Path::new("/w"), "ses_a", "always", &[]).is_err());

        // With two asks pending, the held one gets the user's answer, and the
        // driver's published file names it.
        let two = Fake::start(vec![
            ("GET /global/health", "200 OK", r#"{"version":"1.18.33"}"#.into()),
            (
                "GET /permission?directory=/w",
                "200 OK",
                r#"[{"id":"per_1","sessionID":"ses_a","permission":"bash","patterns":["ls"],"metadata":{},"always":[]},
                    {"id":"per_2","sessionID":"ses_a","permission":"bash","patterns":["rm x"],"metadata":{},"always":[]}]"#.into(),
            ),
            ("POST /permission/per_2/reply", "200 OK", "true".into()),
        ]);
        let turns = tempfile::tempdir().unwrap();
        std::fs::write(turns.path().join("ses_a.held"), "per_2").unwrap();
        let held = held_ids(turns.path(), "ses_a");
        let id =
            answer_for_user(&two.url, "t", Path::new("/w"), "ses_a", "allow_once", &held).unwrap();
        assert_eq!(id, "per_2");
    }
}
