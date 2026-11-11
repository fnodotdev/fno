//! fnod: the local decision model, served by `kev.serve` on loopback. It
//! labels and picks among choices; it is no guard and holds no permission.
//! A caller journals the answer beside its own rule verdict and keeps the
//! rule's answer, so a missing server, a timeout or a malformed reply costs
//! the answer and nothing else.
//!
//! [`ask`] is the one client. Its [`Decision`] says who acts next: nobody
//! (fnod is sure), the model (fnod failed, is unsure, or was asked a
//! question it was never trained on), or the user (only for an act the
//! caller marked irreversible, and only once fnod is sure). Unsure never
//! goes straight to the user.

use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// `python -m kev.serve` listens here unless told otherwise.
const DEFAULT_URL: &str = "http://127.0.0.1:8008";
const DEFAULT_TIMEOUT_MS: u64 = 500;
/// Asks sit at the end of a message, so a long one keeps its tail.
const MAX_MESSAGE_CHARS: usize = 16_000;

/// One question for fnod: the wire form kev reads (`type`, `instructions`,
/// and `criteria` for a choice), where it was trained, and the caller's bar.
pub(crate) struct Question {
    pub id: String,
    pub wire: Value,
    /// A head scores at chance on wording it never saw, so an edited or
    /// new question always escalates.
    pub trained: bool,
    pub escalate_below: f64,
    pub irreversible: bool,
}

impl Question {
    /// The shipped corpus question `id` (`intel_corpus::question`), word
    /// for word, so it reads as trained.
    pub(crate) fn shipped(id: &str) -> Self {
        Self {
            id: id.to_string(),
            wire: crate::intel_corpus::question(id),
            trained: true,
            escalate_below: 0.0,
            irreversible: false,
        }
    }

    /// The shipped question with the user's live edits from
    /// `[fnod.questions.<id>]`: `instructions`, `criteria.<label>` and
    /// `escalate_below`. Config is read on every call, so an edit applies
    /// on the next fire with no restart. Changed wording is untrained.
    pub(crate) fn configured(cwd: &Path, id: &str) -> Self {
        let mut q = Self::shipped(id);
        let read = |key: &str| {
            crate::agents_config::config_value_deep(cwd, &["fnod", "questions", id, key])
        };
        if let Some(text) = read("instructions").and_then(|v| v.as_str().map(str::to_string)) {
            q.wire["instructions"] = json!(text);
        }
        if let Some(toml::Value::Table(criteria)) = read("criteria") {
            let map: Map<String, Value> = criteria
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), json!(s))))
                .collect();
            q.wire["criteria"] = Value::Object(map);
        }
        if let Some(bar) = read("escalate_below")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|n| n as f64)))
        {
            q.escalate_below = bar;
        }
        q.trained = q.wire == crate::intel_corpus::question(id);
        q
    }
}

/// Who acts on a [`Decision`] next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Escalate {
    None,
    Model,
    User,
}

impl Escalate {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Escalate::None => "none",
            Escalate::Model => "model",
            Escalate::User => "user",
        }
    }
}

/// fnod's answer. A noul question labels `yes` or `no` and keeps its raw
/// score in `noul`; a choice question labels the argmax.
#[derive(Debug)]
pub(crate) struct Decision {
    pub label: Option<String>,
    pub probabilities: Map<String, Value>,
    /// A noul question's raw score.
    pub noul: Option<f64>,
    pub confidence: f64,
    pub escalate: Escalate,
    pub ms: u64,
    pub error: Option<String>,
}

/// Ask fnod one question about `state`. Never fails: an off, refused,
/// slow or malformed server answers `escalate: Model` with the reason.
pub(crate) fn ask(url: &str, timeout: Duration, state: Value, q: &Question) -> Decision {
    let start = Instant::now();
    let reply = if url.trim().is_empty() {
        Err("off".to_string())
    } else if !loopback(url) {
        // The state is chat or record text: config must not route it off
        // the machine.
        Err(format!("fnod.url {url} is not loopback; refused"))
    } else {
        let mut questions = Map::new();
        questions.insert(q.id.clone(), q.wire.clone());
        let body = json!({"state": state, "questions": questions});
        crate::opencode_serve::http_json(url, "POST", "/v1/systemone", Some(&body), None, timeout)
            .and_then(|(status, text)| match status {
                200 => serde_json::from_str::<Value>(&text).map_err(|e| format!("reply: {e}")),
                _ => Err(format!("HTTP {status}")),
            })
            .and_then(|reply| {
                read_answer(&reply, &q.id)
                    .ok_or_else(|| format!("reply carries no usable answers.{}", q.id))
            })
    };
    let ms = start.elapsed().as_millis() as u64;
    match reply {
        Ok((label, probabilities, confidence, noul)) => {
            let escalate = if !q.trained || confidence < q.escalate_below {
                Escalate::Model
            } else if q.irreversible {
                Escalate::User
            } else {
                Escalate::None
            };
            Decision {
                label: Some(label),
                probabilities,
                noul,
                confidence,
                escalate,
                ms,
                error: None,
            }
        }
        Err(why) => Decision {
            label: None,
            probabilities: Map::new(),
            noul: None,
            confidence: 0.0,
            escalate: Escalate::Model,
            ms,
            error: Some(why),
        },
    }
}

/// One question's answer in a kev reply: a noul score or choice
/// probabilities.
#[derive(serde::Deserialize)]
struct Answer {
    noul: Option<f64>,
    #[serde(default)]
    probabilities: Map<String, Value>,
}

/// The label, the probabilities, the confidence and the raw noul score, or
/// None when the reply carries no usable answer for `id`.
fn read_answer(reply: &Value, id: &str) -> Option<(String, Map<String, Value>, f64, Option<f64>)> {
    let answer: Answer =
        serde_json::from_value(reply.pointer(&format!("/answers/{id}"))?.clone()).ok()?;
    if let Some(p) = answer.noul {
        let label = if p >= 0.5 { "yes" } else { "no" };
        return Some((label.to_string(), Map::new(), p.max(1.0 - p), Some(p)));
    }
    let (label, p) = answer
        .probabilities
        .iter()
        .filter_map(|(k, v)| v.as_f64().map(|p| (k.clone(), p)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    Some((label, answer.probabilities, p, None))
}

/// fnod's answer to "does `message` ask the user for a decision or an
/// answer?" as journal attrs: `fnod_is_ask` (the probability), `fnod_ms`,
/// `fnod_trained` and `fnod_escalate`, or `fnod_error` with the reason the
/// score is missing. `fnod.url` (empty turns it off), `fnod.timeout_ms`
/// and `[fnod.questions.is_ask]` come from fno config.
pub(crate) fn is_ask(cwd: &Path, message: &str) -> Map<String, Value> {
    let timeout = crate::agents_config::config_value_deep(cwd, &["fnod", "timeout_ms"])
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    let q = Question::configured(cwd, "is_ask");
    let state = json!({"message": tail(message, MAX_MESSAGE_CHARS)});
    let d = ask(
        &configured_url(cwd),
        Duration::from_millis(timeout),
        state,
        &q,
    );
    let mut out = Map::new();
    out.insert("fnod_question".to_string(), json!(q.id));
    out.insert("fnod_trained".to_string(), json!(q.trained));
    out.insert("fnod_escalate".to_string(), json!(d.escalate.as_str()));
    match (d.noul, d.error) {
        (Some(p), None) => {
            out.insert("fnod_is_ask".to_string(), json!(p));
            out.insert("fnod_confidence".to_string(), json!(d.confidence));
            out.insert("fnod_ms".to_string(), json!(d.ms));
        }
        (_, why) => {
            let why = why.unwrap_or_else(|| "reply carries no answers.is_ask.noul".to_string());
            out.insert("fnod_error".to_string(), json!(why));
        }
    }
    out
}

/// `fnod.url` from fno config, else the kev.serve default.
pub(crate) fn configured_url(cwd: &Path) -> String {
    crate::agents_config::config_value_deep(cwd, &["fnod", "url"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| DEFAULT_URL.to_string())
}

/// Every address `url` resolves to is a loopback address.
pub(crate) fn loopback(url: &str) -> bool {
    let authority = url
        .strip_prefix("http://")
        .unwrap_or_default()
        .trim_end_matches('/');
    std::net::ToSocketAddrs::to_socket_addrs(authority).is_ok_and(|addrs| {
        let addrs: Vec<_> = addrs.collect();
        !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
    })
}

fn tail(text: &str, max: usize) -> &str {
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let skip = text.char_indices().nth(count - max).map_or(0, |(i, _)| i);
    &text[skip..]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A one-shot kev stand-in on an ephemeral port: it reads one request,
    /// hands its body back through the join handle, and answers `reply`
    /// after `delay`.
    pub(crate) fn serve_once(
        reply: &'static str,
        delay: Duration,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= len {
                        std::thread::sleep(delay);
                        let out = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                            reply.len()
                        );
                        stream.write_all(out.as_bytes()).ok();
                        return body.to_string();
                    }
                }
                if n == 0 {
                    return String::new();
                }
            }
        });
        (url, handle)
    }

    fn cwd_with(url: &str, timeout_ms: u64) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        std::fs::write(
            dir.path().join(".fno/config.toml"),
            format!("[fnod]\nurl = \"{url}\"\ntimeout_ms = {timeout_ms}\n"),
        )
        .unwrap();
        dir
    }

    /// A sure answer acts alone, an unsure, failed or untrained one goes to
    /// the model, and only a sure answer on an irreversible act goes to the
    /// user. Live config edits apply per call.
    #[test]
    fn ask_picks_a_label_and_escalates_unsure_to_the_model_first() {
        let wait = Duration::from_secs(2);
        let choice = |bar: f64, irreversible: bool| Question {
            id: "pick".into(),
            wire: json!({"type": "choice", "criteria": {"a": "A.", "b": "B."}}),
            trained: true,
            escalate_below: bar,
            irreversible,
        };
        let reply = r#"{"answers":{"pick":{"probabilities":{"a":0.8,"b":0.2}}}}"#;
        let (url, server) = serve_once(reply, Duration::ZERO);
        let d = ask(&url, wait, json!({}), &choice(0.6, false));
        server.join().unwrap();
        assert_eq!(d.label.as_deref(), Some("a"));
        assert_eq!(d.confidence, 0.8);
        assert_eq!(d.escalate, Escalate::None);
        let (url, server) = serve_once(reply, Duration::ZERO);
        assert_eq!(
            ask(&url, wait, json!({}), &choice(0.6, true)).escalate,
            Escalate::User
        );
        server.join().unwrap();
        let (url, server) = serve_once(reply, Duration::ZERO);
        assert_eq!(
            ask(&url, wait, json!({}), &choice(0.9, true)).escalate,
            Escalate::Model
        );
        server.join().unwrap();
        let mut untrained = choice(0.0, true);
        untrained.trained = false;
        let (url, server) = serve_once(reply, Duration::ZERO);
        assert_eq!(
            ask(&url, wait, json!({}), &untrained).escalate,
            Escalate::Model
        );
        server.join().unwrap();
        let (url, server) = serve_once(reply, Duration::from_millis(600));
        let slow = ask(
            &url,
            Duration::from_millis(100),
            json!({}),
            &choice(0.0, true),
        );
        assert_eq!(
            slow.escalate,
            Escalate::Model,
            "a timeout never reaches the user"
        );
        assert!(slow.error.is_some());
        server.join().ok();

        // A raised bar escalates a trained question; changed wording reads
        // untrained. Both apply with no restart.
        let (url, server) = serve_once(
            r#"{"answers":{"is_ask":{"type":"noul","noul":0.85}}}"#,
            Duration::ZERO,
        );
        let cwd = cwd_with(&url, 2000);
        let cfg = cwd.path().join(".fno/config.toml");
        let base = std::fs::read_to_string(&cfg).unwrap();
        std::fs::write(
            &cfg,
            format!("{base}[fnod.questions.is_ask]\nescalate_below = 0.9\n"),
        )
        .unwrap();
        let row = is_ask(cwd.path(), "Ship it?");
        server.join().unwrap();
        assert_eq!(row["fnod_trained"], true);
        assert_eq!(row["fnod_escalate"], "model");
        std::fs::write(
            &cfg,
            format!("{base}[fnod.questions.is_ask]\ninstructions = \"Is this a question?\"\n"),
        )
        .unwrap();
        assert!(!Question::configured(cwd.path(), "is_ask").trained);
    }

    #[test]
    fn a_kev_reply_scores_and_a_slow_or_absent_server_fails_open() {
        let (url, server) = serve_once(
            r#"{"model":"kev-latest","answers":{"is_ask":{"type":"noul","noul":0.91}}}"#,
            Duration::ZERO,
        );
        let cwd = cwd_with(&url, 2000);
        let answer = is_ask(cwd.path(), "Ship it now or wait for review?");
        assert_eq!(answer["fnod_is_ask"], 0.91);
        let sent: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(
            sent["questions"]["is_ask"],
            crate::intel_corpus::question("is_ask")
        );
        assert_eq!(sent["state"]["message"], "Ship it now or wait for review?");

        let (url, server) = serve_once("{}", Duration::from_millis(600));
        let cwd = cwd_with(&url, 100);
        let started = Instant::now();
        let answer = is_ask(cwd.path(), "Which lane?");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(answer["fnod_error"]
            .as_str()
            .unwrap()
            .starts_with("read from"));
        server.join().ok();

        let cwd = cwd_with("", 100);
        assert_eq!(is_ask(cwd.path(), "Which lane?")["fnod_error"], "off");

        // A non-loopback host never receives the message.
        let cwd = cwd_with("http://10.0.0.1:8008", 100);
        let refused = is_ask(cwd.path(), "Which lane?");
        assert!(refused["fnod_error"]
            .as_str()
            .unwrap()
            .contains("not loopback"));

        // A long message keeps its tail, cut on a char boundary.
        assert_eq!(tail(&format!("{}é?", "x".repeat(20)), 3), "xé?");
    }
}
