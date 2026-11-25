//! The front door under the sandbox pin. Inside a sandbox there is no fno
//! home, graph, claim store or GitHub login: `FNO_AGENT_SANDBOXED` is set and
//! the only way out is the guest socket. The loop's commands map to typed
//! ops the host answers; every other command refuses by name before any
//! store opens. The guest and the host each check the op again.

use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::time::Duration;

const METHOD: &str = "sandbox.op";
const SOCK: &str = "/tmp/fno/supervisor.sock";
/// A push, a PR create or the Stop decision can take minutes on the host.
const BOUND: Duration = Duration::from_secs(900);

/// Why the door refused or the host answered with an error. The text names
/// the command or the op.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Refused {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Refused {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// `Some(exit code)` when this process is sandboxed, after answering the
/// command; `None` on the host.
pub fn run(args: &[OsString]) -> Option<i32> {
    let id = std::env::var("FNO_AGENT_SANDBOXED").ok()?;
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let words: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "-J" && *a != "--json")
        .collect();
    if words.first() == Some(&"whoami") {
        println!("sandbox: {id} (sandboxed worker; the host holds the claim and the GitHub login)");
        return Some(0);
    }
    let op = match op_for(&words) {
        Ok(op) => op,
        Err(msg) => {
            eprintln!("fno (sandbox {id}): {msg}");
            return Some(2);
        }
    };
    match call(&op) {
        Ok(v) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
            );
            Some(0)
        }
        Err(e) => {
            eprintln!("fno (sandbox {id}): {e}");
            Some(1)
        }
    }
}

/// The value after `flag`, if any.
fn flag<'a>(words: &[&'a str], flag: &str) -> Option<&'a str> {
    words
        .iter()
        .position(|w| *w == flag)
        .and_then(|i| words.get(i + 1).copied())
}

/// The text a command carries: `--body-file <path>` (`-` is stdin), else
/// the positional words after `skip` leading ones, flags and their values
/// dropped.
fn text(words: &[&str], skip: usize, valued: &[&str]) -> Result<String, Refused> {
    if let Some(path) = flag(words, "--body-file") {
        let mut s = String::new();
        if path == "-" {
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| format!("stdin: {e}"))?;
        } else {
            s = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        }
        return Ok(s);
    }
    let mut out = Vec::new();
    let mut it = words.iter().skip(skip);
    while let Some(w) = it.next() {
        if valued.contains(w) {
            it.next();
        } else if !w.starts_with('-') {
            out.push(*w);
        }
    }
    Ok(out.join(" "))
}

fn refused(words: &[&str]) -> String {
    format!(
        "`fno {}` has no sandbox op. A sandboxed worker may run fno backlog get and note, \
         fno do target start and status, fno agent claim status, fno do plan settle, \
         fno agent mail send, fno do pr push, create, status, comments and reply. \
         The lead does everything else, including merge, review attestation, spawn and \
         backlog updates.",
        words.iter().take(3).copied().collect::<Vec<_>>().join(" ")
    )
}

/// The op for one command line, or the refusal that names it.
pub fn op_for(words: &[&str]) -> Result<Value, Refused> {
    let w = |i: usize| words.get(i).copied().unwrap_or_default();
    let group = match w(0) {
        "agents" => "agent",
        other => other,
    };
    let op = match (group, w(1), w(2)) {
        ("backlog", "get", _) => json!({"op": "node-get"}),
        ("backlog", "note", _) => json!({
            "op": "node-note",
            "text": text(words, 3, &["--kind", "--read", "--resolve"])?,
        }),
        ("do", "target", "start" | "init") => json!({"op": "target-start"}),
        ("do", "target", "status") => json!({"op": "target-status"}),
        ("agent", "claim", "status") => json!({"op": "claim-status"}),
        ("do", "plan", "settle") => json!({
            "op": "plan-settle",
            "item": w(3),
            "text": text(words, 4, &[])?,
        }),
        ("agent", "mail", "send") | ("mail", "send", _) => {
            let skip = if group == "mail" { 2 } else { 3 };
            let kind = match flag(words, "--kind") {
                Some(k @ ("question" | "blocked")) => k,
                _ => "status",
            };
            // The recipient word is the host's to choose: the spawner.
            let body = text(
                words,
                skip,
                &[
                    "--kind",
                    "--from-name",
                    "-H",
                    "--harness",
                    "-c",
                    "--cwd",
                    "--to-lead",
                    "--to-project",
                ],
            )?;
            let body = body
                .split_once(' ')
                .map_or(body.as_str(), |(_, rest)| rest)
                .to_string();
            json!({"op": "mail", "kind": kind, "text": body})
        }
        ("do", "pr", "push") => json!({"op": "push"}),
        ("do", "pr", "create") => json!({
            "op": "pr-create",
            "title": flag(words, "--title").unwrap_or_default(),
            "body": text(words, 3, &["--title", "--base", "--not-duplicate"])?,
        }),
        ("do", "pr", "status" | "wait") => json!({"op": "pr-status"}),
        ("do", "pr", "comments") => json!({"op": "pr-comments"}),
        ("do", "pr", "reply") => json!({
            "op": "pr-reply",
            "comment": w(3).parse::<u64>().map_err(|_| "fno do pr reply <comment id> <text>".to_string())?,
            "text": text(words, 4, &[])?,
        }),
        _ => return Err(Refused(refused(words))),
    };
    Ok(op)
}

fn call(op: &Value) -> Result<Value, Refused> {
    let sock = std::env::var_os("FNO_SUPERVISOR_SOCKET").unwrap_or_else(|| SOCK.into());
    let mut s = std::os::unix::net::UnixStream::connect(&sock)
        .map_err(|e| format!("the guest socket {}: {e}", sock.to_string_lossy()))?;
    s.set_read_timeout(Some(BOUND)).ok();
    s.set_write_timeout(Some(BOUND)).ok();
    let body = serde_json::to_vec(&json!({"id": 1, "method": METHOD, "params": op}))
        .map_err(|e| e.to_string())?;
    s.write_all(&(body.len() as u32).to_le_bytes())
        .and_then(|()| s.write_all(&body))
        .map_err(|e| format!("send: {e}"))?;
    let mut len = [0u8; 4];
    s.read_exact(&mut len).map_err(|e| format!("reply: {e}"))?;
    let n = u32::from_le_bytes(len) as usize;
    if n > 16 * 1024 * 1024 {
        return Err("the reply is over 16 MiB".into());
    }
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).map_err(|e| format!("reply: {e}"))?;
    #[derive(serde::Deserialize)]
    struct Reply {
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<ErrorBody>,
    }
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        message: String,
    }
    let reply: Reply = serde_json::from_slice(&buf).map_err(|e| format!("reply: {e}"))?;
    if let Some(e) = reply.error {
        return Err(Refused(e.message));
    }
    Ok(reply.result.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop's commands map to ops that name no node, branch or
    /// recipient; every other command refuses by name.
    #[test]
    fn loop_commands_map_to_ops_and_the_rest_refuse() {
        let op = |s: &str| op_for(&s.split(' ').collect::<Vec<_>>());
        assert_eq!(op("backlog get x-9999").unwrap(), json!({"op": "node-get"}));
        assert_eq!(
            op("do pr create --title t --base main").unwrap(),
            json!({"op": "pr-create", "title": "t", "body": ""})
        );
        assert_eq!(
            op("agent mail send lead hello there").unwrap(),
            json!({"op": "mail", "kind": "status", "text": "hello there"})
        );
        assert_eq!(
            op("do plan settle U3 measured it").unwrap(),
            json!({"op": "plan-settle", "item": "U3", "text": "measured it"})
        );
        for refused in [
            "do pr merge 12",
            "backlog update x-1 --status done",
            "agent spawn --as headless",
            "do review attest",
            "backlog idea x",
            "config set sandbox.provider none",
        ] {
            let err = op(refused).unwrap_err();
            assert!(err.0.contains("lead does"), "{refused}: {err}");
        }
    }
}
