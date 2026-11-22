use crate::harness::{AsHarness, Harness};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

fn model_samples(agent: &str, lines: impl Iterator<Item = String>) -> (Option<String>, usize) {
    let mut last = None;
    let mut samples = 0;
    for line in lines {
        let Some(model) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|record| crate::session_truth::model_of(agent, &record))
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        last = Some(model);
        samples += 1;
    }
    (last, samples)
}

fn read_error(error: &std::io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let detail = error.to_string();
            let suffix = format!(" (os error {code})");
            let detail = detail.strip_suffix(&suffix).unwrap_or(&detail);
            format!(
                "[Errno {code}] {detail}: {}",
                crate::client_commands::py_repr_str(&path.to_string_lossy())
            )
        }
        None => error.to_string(),
    }
}

fn observed_model(agent: &str, path: Option<&Path>) -> Value {
    if agent.as_harness() != Harness::Claude && agent.as_harness() != Harness::Codex {
        return json!({"kind":"not-file-backed"});
    }
    let Some(path) = path else {
        return json!({"kind":"no-transcript"});
    };
    let read_tail = || -> std::io::Result<(Vec<u8>, bool)> {
        let mut file = std::fs::File::open(path)?;
        let size = file.seek(SeekFrom::End(0))?;
        let start = size.saturating_sub(256 * 1024);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok((bytes, start > 0))
    };
    let (bytes, windowed) = match read_tail() {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return json!({"kind":"no-transcript"})
        }
        Err(error) => return json!({"kind":"unreadable", "reason": read_error(&error, path)}),
    };
    if bytes.is_empty() {
        return json!({"kind":"no-model-yet"});
    }
    if !bytes.ends_with(b"\n") {
        return json!({"kind":"unreadable", "reason":"torn final line (mid-write)"});
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.split('\n');
    if windowed {
        lines.next();
    }
    let (mut last, mut samples) = model_samples(agent, lines.map(str::to_string));
    if last.is_none() && windowed {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) => return json!({"kind":"unreadable", "reason": read_error(&error, path)}),
        };
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some(model) = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|record| crate::session_truth::model_of(agent, &record))
                        .filter(|model| !model.is_empty())
                    {
                        last = Some(model);
                        samples += 1;
                    }
                }
                Err(error) => {
                    return json!({"kind":"unreadable", "reason": read_error(&error, path)})
                }
            }
        }
    }
    match last {
        Some(model) => json!({"kind":"observed", "model": model, "samples": samples}),
        None => json!({"kind":"no-model-yet"}),
    }
}

/// Flags that make a diff command a size probe, not a read of the change.
const DIFF_STAT_ONLY: [&str; 7] = [
    "--stat",
    "--name-only",
    "--name-status",
    "--numstat",
    "--shortstat",
    "--summary",
    "--quiet",
];

/// True when one shell segment prints the change itself: `git diff`,
/// `git show` or `gh pr diff` with no stat-only flag.
fn segment_reads_diff(segment: &str) -> bool {
    let words: Vec<&str> = segment.split_whitespace().collect();
    let reads = words
        .windows(2)
        .any(|w| w == ["git", "diff"] || w == ["git", "show"])
        || words.windows(3).any(|w| w == ["gh", "pr", "diff"]);
    reads && !words.iter().any(|w| DIFF_STAT_ONLY.contains(w))
}

fn command_segments(command: &str) -> impl Iterator<Item = &str> {
    command.split(['\n', ';', '|', '&'])
}

/// Did the session read the diff after its last commit? A transcript that
/// loaded the review skill and went straight to the emit has no such read.
/// Only Claude transcripts are parsed; every other harness is `unobserved`.
fn diff_read(agent: &str, path: Option<&Path>) -> Value {
    if agent.as_harness() != Harness::Claude {
        return json!({"kind":"unobserved"});
    }
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else {
        return json!({"kind":"unobserved"});
    };
    let (mut last_commit, mut last_read) = (None, None);
    for (index, line) in text.lines().enumerate() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let blocks = record["message"]["content"].as_array();
        for block in blocks.into_iter().flatten() {
            if block["type"] != "tool_use" || block["name"] != "Bash" {
                continue;
            }
            let command = block["input"]["command"].as_str().unwrap_or("");
            for segment in command_segments(command) {
                if segment.contains("git commit") {
                    last_commit = Some(index);
                }
                if segment_reads_diff(segment) {
                    last_read = Some(index);
                }
            }
        }
    }
    match (last_read, last_commit) {
        (Some(read), Some(commit)) if read < commit => json!({"kind":"stale"}),
        (Some(_), _) => json!({"kind":"read"}),
        (None, _) => json!({"kind":"none"}),
    }
}

fn parent_for(rows: &[crate::state::RegistryEntry], sid: &str, harness: &str) -> Option<String> {
    let handle = crate::identity::canonical_handle(sid);
    for row in rows {
        let stored = if harness.as_harness() == Harness::Claude {
            Some(row.short_id.as_str())
        } else {
            row.harness_session_id.as_deref()
        };
        let matches = row.name == handle
            || (row.harness.as_deref() == Some(harness)
                && stored.is_some_and(|id| id == sid || id == handle));
        if matches {
            let parent = row.current_owner_session().filter(|v| !v.is_empty())?;
            // The harness guard rides the same ownership switch: a session
            // reowned by succession takes the provenance owner's harness, a
            // birth-edge parent keeps the row's own.
            row.current_owner_harness()
                .or(row.spawned_by_harness.as_deref())
                .filter(|v| !v.is_empty())?;
            return Some(crate::identity::canonical_handle(parent));
        }
    }
    None
}

fn event_context() -> Value {
    let get = |key: &str| std::env::var(key).ok();
    let canonical = crate::spawn_context::resolve_canonical_resolution(&get);
    let (harness, session) = match canonical.class {
        crate::spawn_context::CanonicalClass::Absent => {
            crate::spawn_context::vendor_identity(&canonical.markers)
        }
        crate::spawn_context::CanonicalClass::Canonical => {
            (canonical.harness, canonical.session_id)
        }
        _ => (None, None),
    };
    let home = crate::paths::AgentsHome::from_env();
    let rows = crate::state::load_registry(&home.registry_json())
        .ok()
        .map(|registry| registry.entries)
        .unwrap_or_default();
    let parent = match (&session, &harness) {
        (Some(sid), Some(harness)) => parent_for(&rows, sid, harness),
        _ => None,
    };
    let (attester, witness) = crate::spawn_context::resolve_attester_identity(&get);
    let (witness_name, conflict) = match witness {
        crate::spawn_context::AttesterWitness::Process => ("process", None),
        crate::spawn_context::AttesterWitness::EnvOnly => ("env_only", None),
        crate::spawn_context::AttesterWitness::Conflict(marker, ancestor, environment) => ("conflict", Some(format!("attester identity conflict on {marker}: process ancestry carries {}, the emitting environment carries {}", crate::client_commands::py_repr_str(&ancestor), crate::client_commands::py_repr_str(&environment)))),
    };
    let mut diff_evidence = json!({"kind":"unobserved"});
    let observed = match (
        std::env::var("FNO_AGENT_HARNESS")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("FNO_AGENT_PROVIDER").ok()),
        attester.as_deref(),
    ) {
        (Some(agent), Some(sid)) if !agent.is_empty() && !sid.is_empty() => {
            let stores = crate::session_truth::Stores::ambient();
            let session = crate::session_truth::TruthSession {
                agent: agent.clone(),
                session_id: sid.to_string(),
                transcript_path: None,
                row: None,
            };
            let path = crate::session_truth::session_transcript(&session, &stores);
            diff_evidence = diff_read(&agent, path.as_deref());
            observed_model(&agent, path.as_deref())
        }
        _ => json!({"kind":"no-transcript"}),
    };
    let model = if session.is_some() && harness.is_some() {
        Some(crate::mail_cli::send::resolve_self_model())
    } else {
        None
    };
    let handle = session.as_deref().map(crate::identity::canonical_handle);
    json!({"session_id":session,"handle":handle,"harness":harness,"model":model,"parent":parent,"attester_session_id":attester.unwrap_or_default(),"attester_witness":witness_name,"attester_conflict":conflict,"observed_model":observed,"diff_read":diff_evidence})
}

pub(super) fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("event-context") => {
            println!("{}", event_context());
            0
        }
        Some("observed-model") => {
            let mut agent = String::new();
            let mut path: Option<PathBuf> = None;
            let mut it = args[1..].iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--agent" => agent = it.next().cloned().unwrap_or_default(),
                    "--path" => path = it.next().map(PathBuf::from),
                    _ => {
                        eprintln!("error: unknown observed-model argument {arg}");
                        return 2;
                    }
                }
            }
            println!("{}", observed_model(&agent, path.as_deref()));
            0
        }
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker reowned by lead succession reports to its owner session, and
    /// a birth-edge row with no provenance and no harness keeps returning no
    /// parent at all.
    #[test]
    fn parent_for_routes_a_reowned_worker_to_its_owner_session() {
        let session_ref = |sid: &str| crate::spawn_contract::SessionRef {
            harness: "claude".into(),
            session_id: sid.into(),
            cwd: "/tmp".into(),
        };
        let reowned = crate::state::RegistryEntry {
            name: "w-reowned".into(),
            harness: Some("claude".into()),
            short_id: "03401fb3".into(),
            spawned_by_session: Some("vacatedlead1".into()),
            spawned_by_harness: Some("claude".into()),
            spawn_provenance: Some(crate::spawn_contract::SpawnProvenance {
                origin: crate::spawn_contract::SpawnOrigin::Session {
                    parent: session_ref("vacatedlead1"),
                    invocation: None,
                },
                owner: crate::spawn_contract::SpawnOwner::Session(session_ref("succsess1")),
            }),
            ..Default::default()
        };
        assert_eq!(
            parent_for(&[reowned], "03401fb3-92b2-cafe", "claude").as_deref(),
            Some("succsess"),
            "the reowned worker's reports route to its owner session"
        );

        let bare = crate::state::RegistryEntry {
            name: "w-bare".into(),
            harness: Some("claude".into()),
            short_id: "03401fb3".into(),
            spawned_by_session: Some("parentsess1".into()),
            ..Default::default()
        };
        assert_eq!(
            parent_for(&[bare], "03401fb3-92b2-cafe", "claude"),
            None,
            "no provenance and no birth harness still names no parent"
        );
    }

    #[test]
    fn observed_model_distinguishes_valid_torn_and_unobserved_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\"}}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            observed_model("claude", Some(&path)),
            json!({"kind":"observed", "model":"model-a", "samples":1})
        );
        let old_model = "{\"type\":\"turn_context\",\"payload\":{\"model\":\"old-model\"}}\n";
        let long_tail = "{\"type\":\"other\"}\n".repeat(20_000);
        std::fs::write(&path, format!("{old_model}{long_tail}")).unwrap();
        assert_eq!(
            observed_model("codex", Some(&path)),
            json!({"kind":"observed", "model":"old-model", "samples":1})
        );
        std::fs::write(&path, "{\"type\":\"turn_context\"").unwrap();
        assert_eq!(
            observed_model("codex", Some(&path)),
            json!({"kind":"unreadable", "reason":"torn final line (mid-write)"})
        );
        assert_eq!(
            observed_model("codex", None),
            json!({"kind":"no-transcript"})
        );
        assert_eq!(
            observed_model("opencode", None),
            json!({"kind":"not-file-backed"})
        );
    }

    /// The PR 512 shape: the skill loads, a stat probe runs, the emit follows.
    /// Only a read of the change after the last commit counts as evidence.
    #[test]
    fn diff_read_wants_the_change_itself_after_the_last_commit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let bash = |cmd: &str| {
            json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","name":"Bash","input":{"command":cmd}}]}})
            .to_string()
        };
        let kind = |cmds: &[&str]| {
            let lines: Vec<String> = cmds.iter().map(|c| bash(c)).collect();
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
            diff_read("claude", Some(&path))["kind"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            kind(&[
                "git commit -m x",
                "git diff --stat origin/main...HEAD",
                "emit-attestation.sh code-review"
            ]),
            "none"
        );
        assert_eq!(
            kind(&["git commit -m x", "git diff origin/main...HEAD | head -400"]),
            "read"
        );
        assert_eq!(
            kind(&["git diff origin/main...HEAD", "git commit -m fix"]),
            "stale"
        );
        assert_eq!(kind(&["gh pr diff 512 --name-only"]), "none");
        assert_eq!(
            diff_read("codex", Some(&path)),
            json!({"kind":"unobserved"})
        );
    }
}
