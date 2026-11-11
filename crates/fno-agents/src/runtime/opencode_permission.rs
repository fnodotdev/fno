//! The opencode transport for thread permission asks: list a session's
//! pending asks, read them as [`ThreadAsk`], and reply. The decision is
//! [`crate::thread_permission`]'s; this module only speaks HTTP.
//!
//! Two contracts, measured 2026-11-11:
//!
//! - V1 (1.18.33, the live shared serve): `GET /permission?directory=<cwd>`
//!   lists every pending ask of that directory's instance (with no
//!   `directory` it reads `[]` while an ask waits). A row is `{id, sessionID,
//!   permission, patterns[], metadata, always[], tool}`. The reply is `POST
//!   /permission/{id}/reply?directory=<cwd>` with `{reply, message?}`. 1.18.33
//!   also serves the `/api` routes, but its asks never show there.
//! - V2 (2.x docs, HTTP API.md): `GET /api/session/{sid}/permission` answers
//!   `{data: [{id, sessionID, action, resources[]}]}`; the reply is `POST
//!   /api/session/{sid}/permission/{id}/reply` with `{decision, message?}`.
//!
//! fno never replies `always`: it saves a rule, and fno owns no permission
//! layer. A reject carries a message, which the model reads as the tool's
//! refusal (`The user rejected permission ... with the following feedback`).

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::opencode_serve::http_json;
use crate::thread_permission::{Answer, ThreadAsk};

const CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Contract {
    V1,
    V2,
}

impl Contract {
    /// The contract a serve's `/global/health` version names.
    pub(crate) fn of_version(version: &str) -> Self {
        if crate::opencode_serve::version_at_least(
            version,
            crate::opencode_serve::OPENCODE_V2_FLOOR,
        ) {
            Contract::V2
        } else {
            Contract::V1
        }
    }
}

/// Where to reach one serve.
pub(crate) struct Serve<'a> {
    pub base_url: &'a str,
    pub token: &'a str,
    pub contract: Contract,
}

/// One pending ask as the serve reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub request_id: String,
    pub session_id: String,
    pub action: String,
    pub resources: Vec<String>,
}

#[derive(Deserialize)]
struct V1Request {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    permission: String,
    #[serde(default)]
    patterns: Vec<String>,
}

#[derive(Deserialize)]
struct V2Request {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    action: String,
    #[serde(default)]
    resources: Vec<String>,
}

#[derive(Deserialize)]
struct V2List {
    data: Vec<V2Request>,
}

/// Percent-encode a path for the `?directory=` query value: every byte
/// outside the RFC 3986 unreserved set plus `/` becomes `%XX`.
pub(crate) fn encode_query_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn dir_query(cwd: &Path) -> String {
    format!("directory={}", encode_query_path(&cwd.to_string_lossy()))
}

/// The pending asks of `session_id`, oldest first.
pub(crate) fn list_pending(
    serve: &Serve,
    cwd: &Path,
    session_id: &str,
) -> Result<Vec<Pending>, String> {
    let path = match serve.contract {
        Contract::V1 => format!("/permission?{}", dir_query(cwd)),
        Contract::V2 => format!("/api/session/{session_id}/permission"),
    };
    let (status, body) = http_json(
        serve.base_url,
        "GET",
        &path,
        None,
        Some(serve.token),
        CALL_TIMEOUT,
    )?;
    if status != 200 {
        return Err(format!("GET {path} answered {status}"));
    }
    let rows: Vec<Pending> = match serve.contract {
        Contract::V1 => serde_json::from_str::<Vec<V1Request>>(&body)
            .map_err(|e| format!("permission list parse: {e}"))?
            .into_iter()
            .map(|r| Pending {
                request_id: r.id,
                session_id: r.session_id,
                action: r.permission,
                resources: r.patterns,
            })
            .collect(),
        Contract::V2 => serde_json::from_str::<V2List>(&body)
            .map_err(|e| format!("permission list parse: {e}"))?
            .data
            .into_iter()
            .map(|r| Pending {
                request_id: r.id,
                session_id: r.session_id,
                action: r.action,
                resources: r.resources,
            })
            .collect(),
    };
    Ok(rows
        .into_iter()
        .filter(|p| p.session_id == session_id)
        .collect())
}

/// Allow once, or reject with the text the model reads.
pub(crate) enum Reply<'a> {
    Once,
    Reject(&'a str),
}

/// Send one reply. An answer other than 200 is an error naming the status;
/// the ask stays pending on the serve.
pub(crate) fn reply(serve: &Serve, cwd: &Path, p: &Pending, r: Reply) -> Result<(), String> {
    let (word, message) = match r {
        Reply::Once => ("once", None),
        Reply::Reject(m) => ("reject", Some(m)),
    };
    let (path, mut body) = match serve.contract {
        Contract::V1 => (
            format!("/permission/{}/reply?{}", p.request_id, dir_query(cwd)),
            json!({"reply": word}),
        ),
        Contract::V2 => (
            format!(
                "/api/session/{}/permission/{}/reply",
                p.session_id, p.request_id
            ),
            json!({"decision": word}),
        ),
    };
    if let Some(m) = message {
        body["message"] = json!(m);
    }
    let (status, text) = http_json(
        serve.base_url,
        "POST",
        &path,
        Some(&body),
        Some(serve.token),
        CALL_TIMEOUT,
    )?;
    if status == 200 || status == 204 {
        Ok(())
    } else {
        Err(format!(
            "POST {path} answered {status}: {}",
            text.chars().take(120).collect::<String>()
        ))
    }
}

/// Carry out a ladder answer. A hold sends nothing: the ask waits on the
/// serve for the user. A failed reply is journaled and the ask stays pending;
/// it is never retried with `always`.
pub(crate) fn answer(
    serve: &Serve,
    cwd: &Path,
    p: &Pending,
    a: &Answer,
    events: &Path,
) -> Result<(), String> {
    let sent = match a {
        Answer::Allow => reply(serve, cwd, p, Reply::Once),
        Answer::ModelNote(m) => reply(serve, cwd, p, Reply::Reject(m)),
        Answer::HoldForUser { .. } => return Ok(()),
    };
    if let Err(e) = &sent {
        crate::claude_ask::emit_event(
            events,
            "thread_permission_reply_failed",
            &[
                ("harness", json!("opencode")),
                ("session_id", json!(p.session_id)),
                ("request_id", json!(p.request_id)),
                ("error", json!(e)),
            ],
        );
    }
    sent
}

/// The merged `permission` config the serve reads for `cwd`. With fno
/// writing no permission key, this is the user's own rules.
pub(crate) fn user_rules(serve: &Serve, cwd: &Path) -> Value {
    let path = format!("/config?{}", dir_query(cwd));
    match http_json(
        serve.base_url,
        "GET",
        &path,
        None,
        Some(serve.token),
        CALL_TIMEOUT,
    ) {
        Ok((200, body)) => serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|c| c.get("permission").cloned())
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// The user's V1 rules evaluate `action` on some resource to `ask`. The map
/// flattens in order (`"bash": "ask"` is pattern `*`), the last matching rule
/// wins, and no match is no user rule (opencode's own defaults are not
/// in the map).
pub(crate) fn user_rule_asks(rules: &Value, action: &str, resources: &[String]) -> bool {
    let Some(map) = rules.as_object() else {
        return false;
    };
    let mut flat: Vec<(&str, &str, &str)> = Vec::new();
    for (perm, v) in map {
        match v {
            Value::String(a) => flat.push((perm.as_str(), "*", a.as_str())),
            Value::Object(patterns) => {
                for (pat, a) in patterns {
                    if let Some(a) = a.as_str() {
                        flat.push((perm.as_str(), pat.as_str(), a));
                    }
                }
            }
            _ => {}
        }
    }
    let glob = crate::state_root_drift::glob_match;
    resources.iter().any(|res| {
        flat.iter()
            .rev()
            .find(|(perm, pat, _)| glob(perm, action) && glob(pat, res))
            .is_some_and(|(_, _, a)| *a == "ask")
    })
}

/// A pending ask in the ladder's words.
pub(crate) fn thread_ask(
    p: &Pending,
    cwd: &Path,
    worker: Option<&str>,
    user_rule: bool,
) -> ThreadAsk {
    ThreadAsk {
        harness: "opencode".to_string(),
        session_id: p.session_id.clone(),
        request_id: p.request_id.clone(),
        action: p.action.clone(),
        resources: p.resources.clone(),
        cwd: cwd.to_path_buf(),
        worker: worker.map(str::to_string),
        user_rule,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// A fake serve: each request line is matched by prefix to a canned
    /// `(status, body)`; every request line and body is recorded.
    pub(crate) struct Fake {
        pub url: String,
        pub seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Fake {
        pub(crate) fn start(routes: Vec<(&'static str, &'static str, String)>) -> Fake {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
                    let mut req = String::new();
                    let mut buf = [0u8; 8192];
                    while let Ok(n) = stream.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                    let line = req.lines().next().unwrap_or("").to_string();
                    let body = req
                        .split_once("\r\n\r\n")
                        .map(|(_, b)| b.to_string())
                        .unwrap_or_default();
                    log.lock().unwrap().push((line.clone(), body));
                    let (status, out) = routes
                        .iter()
                        .find(|(prefix, _, _)| line.starts_with(prefix))
                        .map(|(_, s, b)| (*s, b.clone()))
                        .unwrap_or(("404 Not Found", "{}".to_string()));
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                        out.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                }
            });
            Fake { url, seen }
        }
    }

    fn pending(sid: &str) -> Pending {
        Pending {
            request_id: "per_1".into(),
            session_id: sid.into(),
            action: "shell".into(),
            resources: vec!["touch /tmp/x".into()],
        }
    }

    #[test]
    fn each_contract_lists_the_session_ask_and_replies_in_its_shape() {
        {
            let fake = Fake::start(vec![
            (
                "GET /api/session/ses_a/permission",
                "200 OK",
                r#"{"data":[{"id":"per_1","sessionID":"ses_a","action":"shell","resources":["touch /tmp/x"]}]}"#.into(),
            ),
            ("POST /api/session/ses_a/permission/per_1/reply", "200 OK", "true".into()),
        ]);
            let serve = Serve {
                base_url: &fake.url,
                token: "t",
                contract: Contract::V2,
            };
            let dir = tempfile::tempdir().unwrap();
            let listed = list_pending(&serve, Path::new("/w"), "ses_a").unwrap();
            assert_eq!(listed, vec![pending("ses_a")]);
            answer(
                &serve,
                Path::new("/w"),
                &listed[0],
                &Answer::Allow,
                &dir.path().join("e.jsonl"),
            )
            .unwrap();
            let seen = fake.seen.lock().unwrap();
            let (line, body) = seen.last().unwrap();
            assert!(
                line.starts_with("POST /api/session/ses_a/permission/per_1/reply"),
                "{line}"
            );
            assert_eq!(
                serde_json::from_str::<Value>(body).unwrap(),
                json!({"decision": "once"})
            );
        }
        let fake = Fake::start(vec![
            (
                "GET /permission?directory=/w%20x",
                "200 OK",
                r#"[{"id":"per_1","sessionID":"ses_a","permission":"shell","patterns":["touch /tmp/x"],"metadata":{},"always":[]},
                    {"id":"per_2","sessionID":"ses_other","permission":"bash","patterns":["ls"],"metadata":{},"always":[]}]"#.into(),
            ),
            ("POST /permission/per_1/reply?directory=/w%20x", "200 OK", "true".into()),
        ]);
        let serve = Serve {
            base_url: &fake.url,
            token: "t",
            contract: Contract::V1,
        };
        let cwd = Path::new("/w x");
        let dir = tempfile::tempdir().unwrap();
        let listed = list_pending(&serve, cwd, "ses_a").unwrap();
        assert_eq!(listed, vec![pending("ses_a")]);
        let note = Answer::ModelNote("fno held it".into());
        answer(&serve, cwd, &listed[0], &note, &dir.path().join("e.jsonl")).unwrap();
        let seen = fake.seen.lock().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&seen.last().unwrap().1).unwrap(),
            json!({"reply": "reject", "message": "fno held it"})
        );
    }

    #[test]
    fn a_failed_reply_journals_and_never_retries_with_always() {
        let fake = Fake::start(vec![]);
        let serve = Serve {
            base_url: &fake.url,
            token: "t",
            contract: Contract::V2,
        };
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("e.jsonl");
        let err = answer(
            &serve,
            Path::new("/w"),
            &pending("ses_a"),
            &Answer::Allow,
            &events,
        )
        .unwrap_err();
        assert!(err.contains("404"), "{err}");
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one reply, no retry: {seen:?}");
        assert!(!seen[0].1.contains("always"));
        let journal =
            crate::event_store::journal_text(&events, &["thread_permission_reply_failed"]);
        assert!(
            journal.contains("thread_permission_reply_failed") && journal.contains("404"),
            "{journal}"
        );
        // A hold sends nothing at all.
        drop(seen);
        let held = Answer::HoldForUser {
            question_id: "q".into(),
        };
        answer(&serve, Path::new("/w"), &pending("ses_a"), &held, &events).unwrap();
        assert_eq!(fake.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_user_ask_rule_is_read_from_the_merged_map_last_match_wins() {
        let rules = json!({"bash": {"*": "allow", "touch *": "ask"}, "edit": "ask"});
        let r = |s: &str| vec![s.to_string()];
        assert!(user_rule_asks(&rules, "bash", &r("touch /tmp/x")));
        assert!(!user_rule_asks(&rules, "bash", &r("ls")));
        assert!(user_rule_asks(&rules, "edit", &r("a.rs")));
        // A later `*` rule matches every permission, so it wins over both.
        let trailing = json!({"bash": {"touch *": "ask"}, "edit": "ask", "*": "allow"});
        assert!(!user_rule_asks(&trailing, "bash", &r("touch /tmp/x")));
        assert!(!user_rule_asks(&trailing, "edit", &r("a.rs")));
        assert!(!user_rule_asks(
            &json!({}),
            "external_directory",
            &r("/tmp/*")
        ));
        assert!(!user_rule_asks(&Value::Null, "bash", &r("ls")));
    }
}
