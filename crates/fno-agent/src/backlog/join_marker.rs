//! A session joins fno at its first fno skill or command.
//!
//! Joining is opt-in: a session the user opened but never pointed at fno owns
//! no registry row, holds no spawn-gate slot and gets no using-fno preamble.
//! The join is one marker file, `<agents home>/sessions/<harness>/<sid>/joined`,
//! plus the register act `/fno:fno-me` runs. The marker outlives the row, so a
//! joined session whose row went missing mints back at its next session
//! report. `crates/fno/src/backlog/join_marker.rs` reads the same path at the front
//! door; the layout is a wire format between the two crates.
//!
//! Every hook command runs with `FNO_HOOK_CONTEXT=1`. 25 hook scripts run fno
//! commands inside every session, so a join fired from a hook would join every
//! session at its first SessionStart. The join act refuses there. The prompt
//! and Skill hooks that ARE deliberate triggers clear the variable for their
//! one call.

use std::path::{Path, PathBuf};

use crate::paths::AgentsHome;

pub const HOOK_CONTEXT_ENV: &str = "FNO_HOOK_CONTEXT";

/// A session id is a path segment here: reject anything that could walk out
/// of the sessions tree.
fn segment_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
}

pub fn marker_path(root: &Path, harness: &str, session_id: &str) -> Option<PathBuf> {
    (segment_ok(harness) && segment_ok(session_id)).then(|| {
        root.join("sessions")
            .join(harness)
            .join(session_id)
            .join("joined")
    })
}

pub fn is_joined(root: &Path, harness: &str, session_id: &str) -> bool {
    marker_path(root, harness, session_id).is_some_and(|p| p.exists())
}

pub(crate) fn write_marker(
    root: &Path,
    harness: &str,
    session_id: &str,
    trigger: &str,
) -> Result<(), String> {
    let path = marker_path(root, harness, session_id)
        .ok_or_else(|| format!("not a session id: {session_id:?}"))?;
    if path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        // Escalation-only: a law trigger may overwrite a non-law word; every
        // other rewrite is a no-op, so a later bare command never strips a
        // session's adoption evidence.
        if !(!trigger_is_law(existing.trim()) && trigger_is_law(trigger)) {
            return Ok(());
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("marker dir: {e}"))?;
    }
    std::fs::write(&path, trigger).map_err(|e| format!("marker write: {e}"))
}

/// The trigger word a join stamps: `spawn` (birth), `fno-me` (the register
/// act), `skill:<name>` / `prompt:<name>` (a deliberate fno carrier naming a
/// skill), or `command` (a bare fno CLI call). Only the law words adopt:
/// user law D2402 names a /fno: skill other than using-fno, or /fno:fno-me.
fn trigger_is_law(word: &str) -> bool {
    match word.split_once(':') {
        Some(("skill" | "prompt", name)) => !name.is_empty() && name != "using-fno",
        _ => matches!(word, "spawn" | "fno-me"),
    }
}

/// The law-D2402 read: `Some(trigger word)` only when the session's marker
/// carries a word whose cause adopts - a /fno: skill other than using-fno, a
/// prompt naming one, /fno:fno-me, or spawn birth. Absent, legacy-empty and
/// `command` markers answer None: an unknown cause is not a law cause.
pub fn adoption_trigger(root: &Path, harness: &str, session_id: &str) -> Option<String> {
    let path = marker_path(root, harness, session_id)?;
    let word = std::fs::read_to_string(path).ok()?.trim().to_string();
    trigger_is_law(&word).then_some(word)
}

fn env_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    Joined,
    AlreadyJoined,
    /// A spawned worker has its row from the spawn: the marker is the whole act.
    SpawnedWorker,
}

/// The join act: the register `/fno:fno-me` runs, then the marker. Refuses in
/// hook context. The mail-hold register rides only a law trigger (user law
/// D2402): a bare-command or using-fno join writes the marker and mints no
/// row, so a discussion session never holds a worker slot. A law trigger
/// over a non-law marker escalates it, and that escalation is the moment a
/// session whose first fno contact was a bare command adopts.
pub fn join(
    root: &Path,
    harness: &str,
    session_id: &str,
    trigger: &str,
) -> Result<JoinOutcome, String> {
    if env_set(HOOK_CONTEXT_ENV) {
        return Err(format!(
            "{HOOK_CONTEXT_ENV} is set: a hook's fno call never joins its session"
        ));
    }
    marker_path(root, harness, session_id)
        .ok_or_else(|| format!("not a session id: {session_id:?}"))?;
    let law = trigger_is_law(trigger);
    let existing_is_law = marker_path(root, harness, session_id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| trigger_is_law(s.trim()))
        .unwrap_or(false);
    if is_joined(root, harness, session_id) && (!law || existing_is_law) {
        return Ok(JoinOutcome::AlreadyJoined);
    }
    if law && !env_set("FNO_AGENT_SELF") {
        crate::mail_cli::hold::register_existing_session(harness, session_id)?;
    }
    write_marker(root, harness, session_id, trigger)?;
    Ok(if env_set("FNO_AGENT_SELF") {
        JoinOutcome::SpawnedWorker
    } else {
        JoinOutcome::Joined
    })
}

/// Whether ANY session on this machine has joined fno: one joined marker
/// anywhere under sessions/<harness>/<sid>/.
fn any_joined(root: &Path) -> bool {
    let Ok(harnesses) = std::fs::read_dir(root.join("sessions")) else {
        return false;
    };
    harnesses.flatten().any(|h| {
        std::fs::read_dir(h.path())
            .map(|sids| {
                sids.flatten()
                    .any(|sid| sid.path().join("joined").is_file())
            })
            .unwrap_or(false)
    })
}

/// Whether the using-fno preamble is due: only for a joined session (a
/// spawned worker counts as joined), then once and again after each
/// compaction boundary. `record` stamps it taught.
pub fn preamble_due(
    root: &Path,
    harness: &str,
    session_id: &str,
    transcript: Option<&Path>,
    record: bool,
) -> bool {
    if !is_joined(root, harness, session_id) && !env_set("FNO_AGENT_SELF") {
        // A fresh install has no joined session anywhere: the install itself
        // is the pointing-at-fno, so the preamble reads due without waiting
        // for a /fno: prompt to trip the join. The first join on the machine
        // closes the window and opt-in resumes.
        return !any_joined(root);
    }
    crate::mail_teach::lesson_due_at(
        root,
        crate::mail_teach::Lesson::UsingFno,
        session_id,
        transcript,
        None,
        record,
    )
}

/// The `--preamble-due` answer. A carrier that fires exactly at a compaction
/// says so with `compacted`: opencode, pi and agy transcripts carry no
/// boundary shape the stamp could read, so any joined session is due there.
pub fn preamble_due_now(
    root: &Path,
    harness: &str,
    session_id: &str,
    transcript: Option<&Path>,
    mark: bool,
    compacted: bool,
) -> bool {
    if !compacted {
        return preamble_due(root, harness, session_id, transcript, mark);
    }
    let joined = is_joined(root, harness, session_id) || env_set("FNO_AGENT_SELF");
    if joined && mark {
        preamble_due(root, harness, session_id, transcript, true);
    }
    joined
}

// ---------------------------------------------------------------------------
// `fno-agent report --kind join --harness <h> --session-id <id>
//      [--trigger <word>] [--check] [--preamble-due [--mark] [--compacted]
//      [--transcript <path>]]`
//
// Without --check it joins (refused in hook context, exit 3). --trigger names
// the join cause the carrier knows: `command` (the default, a bare fno call),
// `spawn`, `fno-me`, `skill:<name>`, `prompt:<name>`. The word is the law
// read's only input, so an old carrier pairing with a new binary stamps the
// non-adopting default: degraded posture is never over-adoption.
// --preamble-due prints `due` when the preamble is due, nothing otherwise;
// with --compacted it is due for any joined session. Rides the report
// action so no command is added.
// ---------------------------------------------------------------------------

pub fn run_join(args: &[String], home: &AgentsHome) -> i32 {
    let mut harness: Option<String> = None;
    let mut session: Option<String> = None;
    let mut transcript: Option<PathBuf> = None;
    let mut trigger = "command".to_string();
    let (mut check, mut due, mut mark, mut compacted) = (false, false, false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--harness" => &mut harness,
            "--session-id" => &mut session,
            "--transcript" => {
                match it.next() {
                    Some(v) => transcript = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("report --kind join: --transcript needs a value");
                        return 2;
                    }
                }
                continue;
            }
            "--trigger" => {
                match it.next() {
                    Some(v) => trigger = v.clone(),
                    None => {
                        eprintln!("report --kind join: --trigger needs a value");
                        return 2;
                    }
                }
                continue;
            }
            "--check" => {
                check = true;
                continue;
            }
            "--preamble-due" => {
                due = true;
                continue;
            }
            "--mark" => {
                mark = true;
                continue;
            }
            "--compacted" => {
                compacted = true;
                continue;
            }
            other => {
                eprintln!("report --kind join: unexpected argument {other:?}");
                return 2;
            }
        };
        match it.next() {
            Some(v) => *slot = Some(v.clone()),
            None => {
                eprintln!("report --kind join: {arg} needs a value");
                return 2;
            }
        }
    }
    let (Some(harness), Some(session)) = (harness, session) else {
        eprintln!("report --kind join: --harness and --session-id are required");
        return 2;
    };
    let root = home.root();
    if !check {
        match join(root, &harness, &session, &trigger) {
            Ok(JoinOutcome::Joined) => eprintln!(
                "fno: this {harness} session joined fno; skills/using-fno/SKILL.md names both surfaces"
            ),
            Ok(_) => {}
            Err(msg) => {
                eprintln!("report --kind join: {msg}");
                if !due {
                    return 3;
                }
            }
        }
    }
    let due_now = preamble_due_now(
        root,
        &harness,
        &session,
        transcript.as_deref(),
        mark,
        compacted,
    );
    if due && due_now {
        println!("due");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_for(session: &str) -> usize {
        crate::state::try_load_registry(&AgentsHome::shared_registry_json())
            .ok()
            .flatten()
            .map(|r| {
                r.entries
                    .iter()
                    .filter(|e| e.harness_session_id.as_deref() == Some(session))
                    .count()
            })
            .unwrap_or(0)
    }

    struct EnvClear(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl EnvClear {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
            for k in keys {
                std::env::remove_var(k);
            }
            Self(saved)
        }
    }
    impl Drop for EnvClear {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn session_join_writes_the_row_on_a_law_trigger_and_refuses_hook_context() {
        let _root = crate::paths::DeclaredRoot::declare("session-join");
        let _env = EnvClear::new(&[HOOK_CONTEXT_ENV, "FNO_AGENT_SELF"]);
        let home = AgentsHome::from_env();
        std::fs::create_dir_all(home.root()).unwrap();
        let sid = "0f1e2d3c-aaaa-bbbb-cccc-000000000001";

        // AC2: a hook's call writes nothing.
        std::env::set_var(HOOK_CONTEXT_ENV, "1");
        assert!(join(home.root(), "claude", sid, "command").is_err());
        assert!(!is_joined(home.root(), "claude", sid));
        assert_eq!(rows_for(sid), 0);
        std::env::remove_var(HOOK_CONTEXT_ENV);

        // AC1: a law trigger joins with one row and the marker; a second
        // join of the same word changes nothing.
        assert_eq!(
            join(home.root(), "claude", sid, "skill:review"),
            Ok(JoinOutcome::Joined)
        );
        assert!(is_joined(home.root(), "claude", sid));
        assert_eq!(rows_for(sid), 1);
        // A join is not a hold: the row carries no bus-only stamp, and it
        // has a birth date the orphan sweep can age.
        let row = crate::state::load_registry(&AgentsHome::shared_registry_json())
            .unwrap()
            .entries
            .into_iter()
            .find(|e| e.harness_session_id.as_deref() == Some(sid))
            .unwrap();
        assert_eq!(row.delivery_policy, None);
        assert!(!row.created_at.is_empty());
        assert_eq!(
            join(home.root(), "claude", sid, "skill:review"),
            Ok(JoinOutcome::AlreadyJoined)
        );
        assert_eq!(rows_for(sid), 1);

        assert!(marker_path(home.root(), "claude", "../x").is_none());
    }

    /// The law read over every marker word a carrier can stamp, plus the
    /// absent case: only a /fno: skill other than using-fno, a prompt naming
    /// one, /fno:fno-me, or spawn birth adopts (user law D2402). An absent,
    /// legacy-empty or `command` marker is not a law cause.
    #[test]
    fn adoption_trigger_reads_the_trigger_word() {
        let _root = crate::paths::DeclaredRoot::declare("adoption-trigger");
        let home = AgentsHome::from_env();
        let root = home.root();
        std::fs::create_dir_all(root).unwrap();

        assert_eq!(adoption_trigger(root, "claude", "absent-sid"), None);
        write_marker(root, "claude", "legacy-sid", "").unwrap();
        assert_eq!(adoption_trigger(root, "claude", "legacy-sid"), None);
        write_marker(root, "claude", "cmd-sid", "command").unwrap();
        assert_eq!(adoption_trigger(root, "claude", "cmd-sid"), None);
        write_marker(root, "claude", "preamble-sid", "skill:using-fno").unwrap();
        assert_eq!(adoption_trigger(root, "claude", "preamble-sid"), None);

        write_marker(root, "claude", "skill-sid", "skill:review").unwrap();
        assert_eq!(
            adoption_trigger(root, "claude", "skill-sid"),
            Some("skill:review".to_string())
        );
        write_marker(root, "codex", "prompt-sid", "prompt:fix").unwrap();
        assert_eq!(
            adoption_trigger(root, "codex", "prompt-sid"),
            Some("prompt:fix".to_string())
        );
        write_marker(root, "claude", "fno-me-sid", "fno-me").unwrap();
        assert_eq!(
            adoption_trigger(root, "claude", "fno-me-sid"),
            Some("fno-me".to_string())
        );
        write_marker(root, "claude", "spawn-sid", "spawn").unwrap();
        assert_eq!(
            adoption_trigger(root, "claude", "spawn-sid"),
            Some("spawn".to_string())
        );
    }

    /// A bare-command join stamps the non-adopting default and mints no row;
    /// a later law trigger escalates the marker and that escalation is the
    /// moment the row lands; a later command can neither downgrade the word
    /// nor duplicate the row.
    #[test]
    fn a_join_registers_only_on_a_law_trigger_and_escalates() {
        let _root = crate::paths::DeclaredRoot::declare("join-escalation");
        let _env = EnvClear::new(&[HOOK_CONTEXT_ENV, "FNO_AGENT_SELF"]);
        let home = AgentsHome::from_env();
        std::fs::create_dir_all(home.root()).unwrap();
        let sid = "0f1e2d3c-aaaa-bbbb-cccc-000000000002";

        assert_eq!(
            join(home.root(), "claude", sid, "command"),
            Ok(JoinOutcome::Joined)
        );
        assert!(is_joined(home.root(), "claude", sid));
        assert_eq!(rows_for(sid), 0, "a bare command is not a law cause");
        let word =
            std::fs::read_to_string(marker_path(home.root(), "claude", sid).unwrap()).unwrap();
        assert_eq!(word, "command");

        assert_eq!(
            join(home.root(), "claude", sid, "skill:review"),
            Ok(JoinOutcome::Joined)
        );
        assert_eq!(rows_for(sid), 1, "the escalation is when the row lands");
        let word =
            std::fs::read_to_string(marker_path(home.root(), "claude", sid).unwrap()).unwrap();
        assert_eq!(word, "skill:review");

        assert_eq!(
            join(home.root(), "claude", sid, "command"),
            Ok(JoinOutcome::AlreadyJoined)
        );
        let word =
            std::fs::read_to_string(marker_path(home.root(), "claude", sid).unwrap()).unwrap();
        assert_eq!(word, "skill:review", "a command never downgrades");
        assert_eq!(rows_for(sid), 1);

        // The law's second named trigger: /fno:fno-me adopts the same way,
        // from a bare-command marker.
        let sid2 = "0f1e2d3c-aaaa-bbbb-cccc-000000000003";
        assert_eq!(
            join(home.root(), "claude", sid2, "command"),
            Ok(JoinOutcome::Joined)
        );
        assert_eq!(
            join(home.root(), "claude", sid2, "fno-me"),
            Ok(JoinOutcome::Joined)
        );
        assert_eq!(rows_for(sid2), 1, "fno-me registers");
        let word =
            std::fs::read_to_string(marker_path(home.root(), "claude", sid2).unwrap()).unwrap();
        assert_eq!(word, "fno-me");
    }

    #[test]
    fn session_join_gates_the_preamble() {
        let _root = crate::paths::DeclaredRoot::declare("session-join-due");
        let _env = EnvClear::new(&[HOOK_CONTEXT_ENV, "FNO_AGENT_SELF"]);
        let home = AgentsHome::from_env();
        let root = home.root();
        let sid = "s-due";
        let dir = std::env::temp_dir().join(format!("sj-due-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let boundary = |epoch: i64| {
            let ts = chrono::DateTime::from_timestamp(epoch, 0)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let p = dir.join(format!("t-{epoch}.jsonl"));
            std::fs::write(
                &p,
                format!("{{\"type\":\"summary\",\"subtype\":\"compact_boundary\",\"timestamp\":\"{ts}\"}}\n"),
            )
            .unwrap();
            p
        };
        let t1 = boundary(1_000);

        // AC4: an unjoined session reads due only while NO session on the
        // machine has joined (the fresh-install window); one joined marker
        // anywhere closes it. All three asserts read only: the window must
        // not stamp, so AC3's joined flow below stamps fresh at its own
        // first call.
        assert!(preamble_due(root, "codex", sid, Some(&t1), false));
        assert!(preamble_due(root, "codex", sid, Some(&t1), false));
        write_marker(root, "claude", "s-window-closer", "command").unwrap();
        assert!(!preamble_due(root, "codex", sid, Some(&t1), false));

        // AC3: joined, taught once, due once more after a newer boundary.
        write_marker(root, "codex", sid, "command").unwrap();
        assert!(preamble_due(root, "codex", sid, Some(&t1), true));
        assert!(!preamble_due(root, "codex", sid, Some(&t1), true));
        let t2 = boundary(2_000);
        assert!(preamble_due(root, "codex", sid, Some(&t2), true));
        assert!(!preamble_due(root, "codex", sid, Some(&t2), true));

        // The mail lesson keeps its own stamp.
        assert!(crate::mail_teach::teach_if_due_at(
            root,
            sid,
            Some(&t2),
            None,
            false
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_install_reads_due_until_any_join() {
        let _root = crate::paths::DeclaredRoot::declare("fresh-window");
        let _env = EnvClear::new(&[HOOK_CONTEXT_ENV, "FNO_AGENT_SELF"]);
        let home = AgentsHome::from_env();
        let root = home.root();

        // No sessions tree at all: the install window is open and an
        // unjoined session reads due.
        assert!(!any_joined(root));
        assert!(preamble_due(root, "claude", "fresh-1", None, false));

        // A joined marker for ANY harness closes the window for everyone.
        write_marker(root, "codex", "first-join", "command").unwrap();
        assert!(any_joined(root));
        assert!(!preamble_due(root, "claude", "fresh-2", None, false));
    }
}
