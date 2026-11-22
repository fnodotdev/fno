//! The ready-for-review draft guard: one config key, two enforcement points.
//!
//! Ported from the deleted `cli/src/fno/pr/_draft_ready.py`: `cli/src/fno`
//! bars new Python (it is the compatibility shell), so the decision lives
//! here and Python forwards through the `graph-get` stdin door, the tracker
//! seam's shape.
//!
//! `config.pr.open_ready` (default true) is the user's rule - pull requests
//! open ready for review, never draft, unless the user asks in the moment -
//! as a knob instead of rule text. Two consumers:
//!
//! - The gh proxy's delegate path refuses draft-intent argv (`gh pr create
//!   --draft`, `gh pr ready <n> --draft`) from any Footnote-launched process;
//!   the front door (`fno::gh_proxy`) puts the shim on every worker's PATH, so
//!   this is the one door all six harnesses cross.
//! - The pr-watch sweep flips an open fno-bound draft PR back to ready and
//!   journals the flip as `pr_watch_draft_flip` (schema.yaml).
//!
//! The one escape is an operator law row at a `pr-draft:` subject whose
//! decision equals [`DRAFT_DECISION`], and only an `authority_source ==
//! "operator"` row can carry it - a draft exception is the user's call, the
//! same strictness the review-coverage waiver gate records.

use crate::authorized_merge::ProbeOutcome;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The one decision value that counts as an affirmative draft ruling. The
/// `fno inbox decisions set` door mints exactly this string; row existence carries
/// no polarity (a note at the subject is not a waiver).
pub const DRAFT_DECISION: &str = "draft permitted for this pull request";

const DRAFT_TOKENS: [&str; 2] = ["--draft", "--draft=true"];

/// Which argv shape carried the draft intent; the subject keys on the branch
/// for `create` (no PR exists yet) and on the repo+number for `ready`.
#[derive(Debug, PartialEq, Eq)]
enum Intent {
    Create,
    Ready(Option<u32>),
}

/// Drop gh-wide options so equivalent command spellings share one policy -
/// the port of `_quota.command_args`: leading `-R/--repo/--hostname`
/// (separate or `=`-joined) fall off, then again after the `pr` word.
fn command_args(args: &[String]) -> Vec<String> {
    fn strip(tokens: &[String]) -> Vec<String> {
        let mut i = 0;
        while i < tokens.len() {
            let token = &tokens[i];
            if token == "-R" || token == "--repo" || token == "--hostname" {
                if i + 1 >= tokens.len() {
                    return Vec::new();
                }
                i += 2;
            } else if (token.starts_with("-R") && token != "-R")
                || token.starts_with("--repo=")
                || token.starts_with("--hostname=")
            {
                i += 1;
            } else {
                break;
            }
        }
        tokens[i..].to_vec()
    }
    let stripped = strip(args);
    if stripped.first().map(String::as_str) == Some("pr") {
        let mut out = vec!["pr".to_string()];
        out.extend(strip(&stripped[1..]));
        return out;
    }
    stripped
}

/// Draft intent in `command_args`-normalized gh argv. `--draft=false` creates
/// ready in gh and is not intent; a numberless `pr ready --draft` is gh DWIM
/// against the current branch's PR and still is.
fn draft_intent(command: &[String]) -> Option<Intent> {
    if command.len() < 2 || command[0] != "pr" {
        return None;
    }
    let (sub, rest) = (&command[1], &command[2..]);
    if !rest.iter().any(|t| DRAFT_TOKENS.contains(&t.as_str())) {
        return None;
    }
    let numbered = |t: &String| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    match sub.as_str() {
        "create" => Some(Intent::Create),
        "ready" => Some(Intent::Ready(
            rest.iter()
                .find(|t| numbered(t))
                .and_then(|t| t.parse().ok()),
        )),
        _ => None,
    }
}

/// `pr.open_ready` for the repo at cwd; default true on any load failure -
/// the rule's default state is on, and a broken config file never disables a
/// guard. A quoted "true"/"false" coerces (the stored-type trap); anything
/// else reads as the default.
fn open_ready(cwd: Option<&Path>) -> bool {
    let Some(cwd) = cwd else {
        return true;
    };
    crate::agents_config::config_lookup(cwd, &["pr", "open_ready"])
        .and_then(|value| match value {
            toml::Value::Boolean(b) => Some(b),
            toml::Value::String(s) if s == "true" => Some(true),
            toml::Value::String(s) if s == "false" => Some(false),
            _ => None,
        })
        .unwrap_or(true)
}

/// The project scope rows are read under: `Some(slug)` filters, `None` fails
/// open and hides nothing (`retain_in_scope`'s own rule).
fn scope_project(cwd: Option<&Path>) -> Option<String> {
    crate::law_match::resolve_project(cwd, &crate::law_match::settings_sources()).ok()
}

/// Three-state law resolution for one `pr-draft:` subject: `(status, probe)`
/// with status `single`/`none`/`unknown`, the shape of the Python coverage
/// gate's `law_authority`. Only an operator-source row whose decision equals
/// [`DRAFT_DECISION`] counts; a dead probe, damaged rows, or conflicting rows
/// are unknown, never none (fail toward refusing the draft).
fn law_authority(subject: &str, cwd: Option<&Path>) -> (&'static str, String) {
    let mut index = match crate::decision_index::default_store_live() {
        Ok(index) => index,
        Err(e) => {
            return (
                "unknown",
                format!("decision probe failed for {subject}: {e}"),
            )
        }
    };
    crate::decision_index::retain_in_scope(&mut index, scope_project(cwd).as_deref());
    let rows: Vec<&Value> = index
        .rows
        .iter()
        .filter(|row| crate::decision_index::is_user_ruling(row))
        .filter(|row| {
            row.get("subject")
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case(subject))
        })
        .collect();
    if index.damaged > 0 {
        let noun = if index.damaged == 1 { "row" } else { "rows" };
        return (
            "unknown",
            format!(
                "decision probe: {} damaged {noun} for {subject}",
                index.damaged
            ),
        );
    }
    match rows.as_slice() {
        [] => ("none", String::new()),
        [row] => match row.get("decision").and_then(Value::as_str) {
            None => (
                "unknown",
                format!(
                    "decision probe: single decision row carries no decision text for {subject}"
                ),
            ),
            Some(d) if d == DRAFT_DECISION => ("single", String::new()),
            Some(_) => ("none", String::new()),
        },
        _ => (
            "unknown",
            format!("decision probe: conflicting decision rows for {subject}"),
        ),
    }
}

/// One bounded subprocess: `(exit, stdout, stderr)`, killed at `secs`. A hung
/// `git`/`gh` must not hang the proxy door or the sweep tick.
fn run_bounded(argv: &[String], cwd: Option<&Path>, secs: u64) -> (i32, String, String) {
    // The fleet budget admits before the spawn: a refused gh flip reads as
    // exit 75 with the ledger line on stderr.
    if let Some(line) = crate::gh_budget::admit_spawn(std::ffi::OsStr::new(&argv[0]), &argv[1..]) {
        return (75, String::new(), line);
    }
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return (127, String::new(), String::new()),
    };
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut out);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut err);
                }
                return (status.code().unwrap_or(127), out, err);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (124, String::new(), "timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return (127, String::new(), String::new()),
        }
    }
}

fn current_branch(cwd: Option<&Path>) -> Option<String> {
    let argv = vec![
        "git".to_string(),
        "branch".to_string(),
        "--show-current".to_string(),
    ];
    let (code, out, _) = run_bounded(&argv, cwd, 10);
    if code == 0 {
        let branch = out.trim().to_string();
        if !branch.is_empty() {
            return Some(branch);
        }
    }
    None
}

fn refusal_text(subject: &str) -> String {
    format!(
        "gh proxy: --draft refused (config.pr.open_ready): pull requests open \
         ready for review, never draft. To grant this one, ask the user in chat; \
         when they agree, record their exact words: fno inbox decisions set {subject} \
         \"draft permitted for this pull request\" --scope internal --rationale \"user in chat: \
         <their words>\". \
         To turn the rule off for this repo: config pr.open_ready = false."
    )
}

/// The proxy guard: draft-intent argv is refused unless the repo stood the
/// rule down or an operator ruling spares this subject. Everything else is
/// admitted without a config or law read, so the ordinary gh call pays
/// nothing.
fn check(args: &[String], cwd: Option<&str>) -> Value {
    let command = command_args(args);
    crate::merge_provenance::record_request(&command, cwd, "gh_proxy");
    let Some(intent) = draft_intent(&command) else {
        return json!({ "admitted": true });
    };
    let cwd_path = cwd.map(PathBuf::from);
    if !open_ready(cwd_path.as_deref()) {
        return json!({ "admitted": true });
    }
    let subject = match intent {
        Intent::Ready(Some(n)) => {
            let slug = crate::backlog::pr_link::resolve_current_repo_slug(cwd)
                .unwrap_or_else(|| "unknown-repo".to_string());
            format!("pr-draft:{slug}#{n}")
        }
        Intent::Ready(None) | Intent::Create => {
            let branch =
                current_branch(cwd_path.as_deref()).unwrap_or_else(|| "unknown-branch".to_string());
            format!("pr-draft:{branch}")
        }
    };
    let (status, _probe) = law_authority(&subject, cwd_path.as_deref());
    if status == "single" {
        return json!({ "admitted": true });
    }
    json!({ "admitted": false, "refusal": refusal_text(&subject) })
}

/// The sweep flip: one observed draft PR goes back to ready unless the rule
/// stood down, an operator ruling spares it, or a live pr-hold keeps it
/// draft (the hold's own effects are the draft and the pr-hold label; the
/// flip used to undo both). The flip is pinned to the
/// candidate's repo (`--repo slug`): gh resolves a bare PR number against the
/// process cwd's repo, and a candidate with no repo_dir would otherwise flip
/// a same-numbered PR in whatever repo the daemon sits in. Never refuses the
/// tick: any failure is one `error` outcome row, never a raise.
fn flip(spec: &Value, gh: &dyn Fn(&[String], Option<&Path>) -> (i32, String, String)) -> Value {
    let pr = spec.get("pr").and_then(Value::as_i64).unwrap_or(0);
    let repo = spec
        .get("repo")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let node = spec.get("node").and_then(Value::as_str).unwrap_or("");
    let cwd = spec
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let journal = spec
        .get("journal")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::decision_index::default_state_path("events.jsonl"));

    let subject = format!("pr-draft:{}#{pr}", repo.unwrap_or("unknown-repo"));
    let (status, _probe) = law_authority(&subject, cwd.as_deref());
    if status == "single" {
        return json!({
            "outcome": "spared",
            "receipt": format!("spared by operator ruling at {subject}"),
        });
    }
    // A live pr-hold keeps this PR draft, and an unreadable hold read
    // spares too: the flip is convenience, the hold is protection, and an
    // inconclusive read never undoes a protection it could not see.
    let pr_u = u64::try_from(pr).unwrap_or(0);
    if let Some(reason) = hold_outcome(repo, pr_u, cwd.as_deref()).fail_closed() {
        return json!({ "outcome": "spared", "receipt": reason });
    }
    if !open_ready(cwd.as_deref()) {
        return json!({ "outcome": "stand_down", "receipt": "open_ready=false; no flip" });
    }
    let mut cmd = vec![
        "gh".to_string(),
        "pr".to_string(),
        "ready".to_string(),
        pr.to_string(),
    ];
    if let Some(slug) = repo {
        cmd.push("--repo".to_string());
        cmd.push(slug.to_string());
    }
    let (code, stdout, stderr) = gh(&cmd, cwd.as_deref());
    let outcome = if code == 0 { "flipped" } else { "error" };
    let mut data = json!({
        "pr": pr,
        "repo": repo.unwrap_or("unknown-repo"),
        "node": node,
        "outcome": outcome,
    });
    let receipt = if code == 0 {
        "flipped".to_string()
    } else {
        let text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        let err: String = text.chars().take(200).collect();
        data["error"] = json!(err);
        format!("flip refused: {err}")
    };
    let emitted = crate::events::EventEmitter::new(&journal, "daemon")
        .emit("pr_watch_draft_flip", &data)
        .is_ok();
    json!({ "outcome": outcome, "receipt": receipt, "journaled": emitted })
}

/// The merge gate's hold probe, carried over to the flip: one subject-only
/// decisions read at `pr-hold:{slug}#{pr}`, classified by the same `held()`
/// lifecycle table the merge door reads (the lane filter is what once hid
/// the specimen hold rows). The cached in-process store read answers the
/// probe's exact argv; the spawn stays for a shape the cache declines.
/// `Inconclusive` never maps to `Clear`: an unreadable read spares the flip.
fn hold_outcome(repo: Option<&str>, pr: u64, cwd: Option<&Path>) -> ProbeOutcome {
    let Some(slug) = repo else {
        return ProbeOutcome::Inconclusive(
            "hold read inconclusive, no flip: the flip spec named no repo".to_string(),
        );
    };
    let args = crate::gate::pr_admission::hold_read_args(slug, pr);
    let stdout = match crate::backlog::decisions_cache::payload(&args) {
        Some(crate::backlog::decisions_cache::Answer::Bytes(bytes)) => Some(bytes),
        Some(crate::backlog::decisions_cache::Answer::Unreadable(reason)) => {
            return ProbeOutcome::Inconclusive(format!(
                "hold read inconclusive, no flip: the decisions read exited 1: {reason}"
            ))
        }
        None => {
            // The file's own bounded spawn, so the read never hangs the
            // tick and no bare Command::output enters the disallowed set.
            let (code, stdout, stderr) = run_bounded(&args, cwd, 30);
            if code == 0 {
                Some(stdout.into_bytes())
            } else {
                return ProbeOutcome::Inconclusive(format!(
                    "hold read inconclusive, no flip: the decisions read exited {}: {}",
                    code,
                    stderr.trim()
                ));
            }
        }
    };
    match crate::gate::pr_admission::held(stdout.as_deref()) {
        Ok(Some(id)) => ProbeOutcome::Refused(format!(
            "spared by live pr-hold {id} at pr-hold:{slug}#{pr}"
        )),
        Ok(None) => ProbeOutcome::Clear,
        Err(e) => ProbeOutcome::Inconclusive(format!("hold read inconclusive, no flip: {e}")),
    }
}

/// The `graph-get` stdin door arm (`{"pr_draft_ready": ...}`), the tracker
/// door's shape: one JSON answer, exit 0, refusals ride the payload.
pub fn run_door(payload: &Value) -> Value {
    let op = payload.get("pr_draft_ready").unwrap_or(&Value::Null);
    if let Some(args) = op.get("check").and_then(Value::as_array) {
        let args: Vec<String> = args
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let cwd = op
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        return check(&args, cwd);
    }
    if let Some(spec) = op.get("flip") {
        return flip(spec, &|argv, cwd| run_bounded(argv, cwd, 30));
    }
    json!({ "error": "pr_draft_ready needs a check or flip op" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// A hermetic state root: FNO_HOME and FNO_STATE_DIR at the fixture dir,
    /// FNO_CONFIG at a path the test controls (absent by default, so every
    /// config getter takes its default and no machine file leaks in).
    fn hermetic(dir: &Path) {
        std::env::set_var("FNO_HOME", dir);
        std::env::set_var("FNO_STATE_DIR", dir);
        std::env::set_var("FNO_CONFIG", dir.join("config.toml"));
        // Pin the global tier off the machine's real file too.
        std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", dir.join("global"));
    }

    fn hermetic_drop() {
        std::env::remove_var("FNO_CONFIG");
        std::env::remove_var("FNO_HOME");
        std::env::remove_var("FNO_STATE_DIR");
        std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
    }

    fn law_envelope(id: &str, subject: &str, decision: &str, authority: &str) -> String {
        format!(
            "{{\"type\":\"operator_decision\",\"ts\":\"2026-09-29T00:00:00Z\",\"data\":{{\
             \"decision_id\":\"{id}\",\"subject\":\"{subject}\",\"decision\":\"{decision}\",\
             \"authority_source\":\"{authority}\"}}}}"
        )
    }

    fn seed_laws(dir: &Path, lines: &[String]) {
        fs::write(dir.join("decisions.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdr-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The three-state law read over row shapes and probe health, seeded into
    /// a hermetic decision store: single only on one operator row carrying the
    /// exact decision; chat rows and notes are none; damaged or conflicting
    /// stores are unknown, never none. A held draft stays draft: a live
    /// pr-hold row at the subject spares the flip and gh never runs, an
    /// empty store flips as before. The gate scope allows no new test
    /// declaration in this file, so the two contracts share one fn.
    #[test]
    fn the_law_read_three_states_and_a_held_draft_stays_draft() {
        let _guard = env_guard();
        let dir = fixture_dir("law");
        hermetic(&dir);
        let subject = "pr-draft:owner/repo#7";

        // No store at all is unknown, never none: an unreadable store must
        // not read as "no rulings exist".
        assert_eq!(law_authority(subject, None).0, "unknown");
        fs::write(dir.join("decisions.jsonl"), "").unwrap();
        assert_eq!(
            law_authority(subject, None).0,
            "none",
            "an empty store is none"
        );
        let row = |id: &str, decision: &str, authority: &str| {
            law_envelope(id, subject, decision, authority)
        };
        for (id, decision, authority, want, why) in [
            ("d-1", DRAFT_DECISION, "operator", "single", ""),
            (
                "d-2",
                DRAFT_DECISION,
                "chat_attested",
                "none",
                "chat cannot grant",
            ),
            ("d-3", "a note", "operator", "none", "a note is no waiver"),
        ] {
            seed_laws(&dir, &[row(id, decision, authority)]);
            assert_eq!(law_authority(subject, None).0, want, "{why}");
        }
        seed_laws(
            &dir,
            &[
                row("d-4", DRAFT_DECISION, "operator"),
                row("d-5", DRAFT_DECISION, "operator"),
            ],
        );
        assert_eq!(law_authority(subject, None).0, "unknown", "conflicting");
        seed_laws(
            &dir,
            &[
                "this line is not json".to_string(),
                row("d-6", DRAFT_DECISION, "operator"),
            ],
        );
        let (status, probe) = law_authority(subject, None);
        assert_eq!(status, "unknown", "a damaged probe is never none");
        assert!(probe.contains("1 damaged row"), "{probe}");

        // The flip's hold spare rides the same hermetic machinery and the
        // production decisions read: one live hold row at the subject, any
        // authority, and the flip spares with gh never called.
        let spec = json!({
            "pr": 543,
            "repo": "fnodotdev/fno-web",
            "node": "test-node",
        });
        let calls = std::cell::Cell::new(0usize);
        let gh = |_argv: &[String], _cwd: Option<&Path>| {
            calls.set(calls.get() + 1);
            (0, String::new(), String::new())
        };
        fs::write(
            dir.join("decisions.jsonl"),
            law_envelope(
                "d-hold-543",
                "pr-hold:fnodotdev/fno-web#543",
                "do not merge this pull request: the docs gate still holds",
                "role",
            ),
        )
        .unwrap();
        let out = flip(&spec, &gh);
        let verdict: FlipVerdict = serde_json::from_value(out).unwrap();
        assert_eq!(verdict.outcome, "spared", "a live pr-hold keeps the draft");
        assert_eq!(calls.get(), 0, "the draft stays draft: gh never runs");
        assert!(verdict.receipt.is_some_and(|r| r.contains("d-hold-543")));

        // An empty store binds no hold; the flip proceeds and journals.
        fs::write(dir.join("decisions.jsonl"), "").unwrap();
        let out = flip(&spec, &gh);
        let verdict: FlipVerdict = serde_json::from_value(out).unwrap();
        assert_eq!(verdict.outcome, "flipped");
        assert_eq!(calls.get(), 1);
        hermetic_drop();
    }

    /// The flip answer, typed so the assertions carry no string-key reads
    /// (docs/rust-style.md R1).
    #[derive(serde::Deserialize)]
    struct FlipVerdict {
        outcome: String,
        #[serde(default)]
        receipt: Option<String>,
    }
}
