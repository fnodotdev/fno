//! The oh-my-pi (`omp`) harness: its session store and its provider.
//!
//! omp is a fork of earendil pi with its own binary, store and identity rules,
//! read from source at can1357/oh-my-pi origin/main d210c30657 (18.18.9):
//!
//! - The store is `~/.omp/agent/sessions/<encoded cwd>/<ts>_<id>.jsonl`
//!   (`utils/src/dirs.ts`, `session/session-paths.ts`). `PI_CODING_AGENT_DIR`
//!   moves the agent dir, the same override omp honours.
//! - Line 1 of a session file is a fixed-width title slot; the
//!   `{"type":"session", id, cwd}` header follows it
//!   (`session-manager.ts createEmptySessionFile`). pi's header is line 1, so
//!   the readers here find the header row by its type, never by position.
//! - omp mints a UUIDv7 and takes none on the command line. `--resume <id>`
//!   opens an existing session and answers "not found" on a miss, so a resume
//!   can never create a second session under the same id the way pi's
//!   `--session-id` can.
//!
//! The cwd-to-dir encoding has three schemes and a migration path upstream,
//! so nothing here recomputes it: a lookup by id walks every project dir.

use crate::provider::{AgentEntry, CreateContext, Provider, ReachabilityProbeError, ResumeContext};
use crate::ParsedEvent;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How many leading lines may precede the session header. omp writes one
/// title slot before it; the bound keeps a corrupt file from being read whole.
const HEADER_SCAN_LINES: usize = 4;

/// omp's sessions root: `$PI_CODING_AGENT_DIR/sessions`, else
/// `~/.omp/agent/sessions`.
pub fn sessions_dir() -> PathBuf {
    sessions_dir_from(|key| std::env::var_os(key))
}

fn sessions_dir_from(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    if let Some(dir) = get("PI_CODING_AGENT_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("sessions");
    }
    match get("HOME") {
        Some(home) => PathBuf::from(home)
            .join(".omp")
            .join("agent")
            .join("sessions"),
        None => PathBuf::from("."),
    }
}

/// The session header row of one session file, found by `type == "session"`
/// within the first few lines.
pub fn header(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    use std::io::BufRead;
    std::io::BufReader::new(file)
        .lines()
        .take(HEADER_SCAN_LINES)
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|row| row.get("type").and_then(Value::as_str) == Some("session"))
}

/// The header's session id, when the header names one.
pub fn header_id(path: &Path) -> Option<String> {
    header(path)?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Where a lookup by id landed.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionLookup {
    One { file: PathBuf },
    Duplicate { files: Vec<PathBuf> },
    None,
    Unknown { dir: PathBuf, reason: String },
}

/// Find the session files whose header carries `id`. The file name is
/// `<ts>_<id>.jsonl`, so the name narrows the scan and the header decides.
pub fn lookup_session(root: &Path, id: &str) -> SessionLookup {
    let projects = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            return SessionLookup::Unknown {
                dir: root.to_path_buf(),
                reason: e.to_string(),
            }
        }
    };
    let suffix = format!("_{id}.jsonl");
    let mut files: Vec<PathBuf> = projects
        .flatten()
        .map(|p| p.path())
        .filter(|p| p.is_dir())
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|f| f.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .filter(|p| header_id(p).as_deref() == Some(id))
        .collect();
    files.sort();
    match files.len() {
        0 => SessionLookup::None,
        1 => SessionLookup::One {
            file: files.remove(0),
        },
        _ => SessionLookup::Duplicate { files },
    }
}

/// The reply text of an `agent_end` event's last assistant message, when that
/// message stopped on `stop`.
fn agent_end_reply(event: &Value) -> Option<String> {
    let message = event
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))?;
    if message.get("stopReason").and_then(Value::as_str) != Some("stop") {
        return None;
    }
    let text: String = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub struct OmpProvider;

impl Provider for OmpProvider {
    fn name(&self) -> &'static str {
        "omp"
    }

    fn create_argv(&self, ctx: &CreateContext) -> Vec<String> {
        let mut argv =
            crate::harness_capabilities::render_session_argv("omp", "headless_create", None)
                .expect("embedded omp headless-create capability");
        argv.push("--".to_string());
        argv.push(ctx.message.clone());
        argv
    }

    fn resume_argv(&self, ctx: &ResumeContext) -> Vec<String> {
        let mut argv = crate::harness_capabilities::render_session_argv(
            "omp",
            "headless_resume",
            Some(&ctx.session_id),
        )
        .expect("embedded omp headless-resume capability");
        argv.push("--".to_string());
        argv.push(ctx.message.clone());
        argv
    }

    fn parse_stream_event(&self, chunk: &str) -> ParsedEvent {
        let reply = serde_json::from_str::<Value>(chunk.trim())
            .ok()
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("agent_end"))
            .and_then(|e| agent_end_reply(&e));
        match reply {
            Some(text) => ParsedEvent::ReplyComplete {
                text,
                duration_ms: 0,
            },
            None => ParsedEvent::Unknown {
                raw: chunk.to_string(),
            },
        }
    }

    /// Two answers only, like pi's probe: `Ok(true)` or inconclusive. A
    /// missing file is not proof a session is gone, because the measured
    /// source does not show when omp first writes the file of a session that
    /// has not been prompted.
    fn reachability(
        &self,
        entry: &AgentEntry,
        _timeout: Duration,
    ) -> Result<bool, ReachabilityProbeError> {
        let sid = entry
            .session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ReachabilityProbeError::new("omp", "no session id in entry"))?;
        let root = sessions_dir();
        match lookup_session(&root, sid) {
            SessionLookup::One { .. } => Ok(true),
            SessionLookup::Duplicate { files } => Err(ReachabilityProbeError::new(
                "omp",
                format!("session id {sid:?} names {} session files", files.len()),
            )),
            SessionLookup::Unknown { dir, reason } => Err(ReachabilityProbeError::new(
                "omp",
                format!("session store {} unreadable: {reason}", dir.display()),
            )),
            SessionLookup::None => Err(ReachabilityProbeError::new(
                "omp",
                format!("no session file for {sid:?} under {}", root.display()),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "019a7c3e-1f2a-7b3c-8d4e-5f6a7b8c9d0e";

    // The two leading lines omp writes, condensed: the fixed-width title slot,
    // then the header.
    fn write_session(dir: &Path, id: &str, cwd: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join(format!("2026-11-11T22-00-00-000Z_{id}.jsonl"));
        let body = format!(
            "{{\"type\":\"title_slot\",\"v\":1,\"title\":\"\",\"updatedAt\":\"t\",\"pad\":\"   \"}}\n\
             {{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"t\",\"cwd\":\"{cwd}\"}}\n"
        );
        std::fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn the_store_root_follows_omps_own_override() {
        let home = |key: &str| (key == "HOME").then(|| "/h".into());
        assert_eq!(
            sessions_dir_from(home),
            PathBuf::from("/h/.omp/agent/sessions")
        );
        let moved = |key: &str| match key {
            "PI_CODING_AGENT_DIR" => Some("/a".into()),
            "HOME" => Some("/h".into()),
            _ => None,
        };
        assert_eq!(sessions_dir_from(moved), PathBuf::from("/a/sessions"));
    }

    #[test]
    fn a_lookup_reads_the_header_behind_the_title_slot() {
        let root = tempfile::tempdir().unwrap();
        let file = write_session(&root.path().join("-tmp-a-"), ID, "/tmp/a");
        assert_eq!(header_id(&file).as_deref(), Some(ID));
        assert_eq!(lookup_session(root.path(), ID), SessionLookup::One { file });
        assert_eq!(
            lookup_session(root.path(), "019a7c3e-0000-7000-8000-000000000000"),
            SessionLookup::None
        );
        write_session(&root.path().join("-tmp-b-"), ID, "/tmp/b");
        assert!(matches!(
            lookup_session(root.path(), ID),
            SessionLookup::Duplicate { files } if files.len() == 2
        ));
    }

    #[test]
    fn the_resume_argv_fences_the_message_behind_the_id() {
        let argv = OmpProvider.resume_argv(&ResumeContext {
            session_id: ID.to_string(),
            message: "--version".to_string(),
            cwd: PathBuf::from("/tmp/a"),
            from_name: None,
            yolo: false,
        });
        assert_eq!(
            argv,
            [
                "omp",
                "--print",
                "--mode",
                "json",
                "--resume",
                ID,
                "--",
                "--version"
            ]
        );
    }

    #[test]
    fn only_a_stopped_agent_end_is_a_reply() {
        let done = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"pong"}],"stopReason":"stop"}]}"#;
        assert!(matches!(
            OmpProvider.parse_stream_event(done),
            ParsedEvent::ReplyComplete { text, .. } if text == "pong"
        ));
        let failed = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[],"stopReason":"error"}]}"#;
        assert!(matches!(
            OmpProvider.parse_stream_event(failed),
            ParsedEvent::Unknown { .. }
        ));
    }
}
