//! The git-protection Bash guard. It refuses a push to a protected branch,
//! `--no-verify` and its core.hooksPath twin, and a direct GraphQL PR read.
//! It WARNS on a `gh pr create` whose branch names a node and whose command
//! shows no composed closure line (the merge-time closure gate still reds
//! the PR), and on a push that repeats one inside the debounce window or
//! lands while a GitHub run is in flight (the 2026-11-07 guard-purge
//! ruling: process and pacing gates are not one of the four harms). The
//! raw `gh pr merge` gate was removed on 2026-11-10 (the guard-diet
//! ruling): merging is decided by `fno do pr merge` and the server
//! ruleset, never by this hook.
//!
//! A PreToolUse allow approves the whole Bash call, so anything this guard
//! authorizes (an approved `--no-verify`) must stand alone. Its state
//! lives under `${FNO_HOME:-~/.fno}`. Git reads run in the process cwd,
//! never the payload cwd.

mod gh_gates;
mod git_gate;
mod script_gate;
mod segments;

use segments::Segment;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Child result of one probe: `(exit code, stderr, stdout)`, or the reason
/// the child could not answer (a timeout, a start failure).
pub(super) type Exec = Result<(i32, String, String), String>;

/// Every read the guard makes outside its own state files. The real side
/// runs git and this binary's own doors; tests answer from a fake.
pub(super) trait Probes {
    fn current_branch(&self) -> Option<String>;
    fn branch_node_ids(&self, head_ref: &str) -> Vec<String>;
    fn in_flight(&self, branch: &str) -> Option<Value>;
    fn push_stamps_dir(&self) -> PathBuf;
    fn emit_push_bypass(&self, branch: &str);
}

pub(super) struct Ctx<'a> {
    /// `${FNO_HOME:-~/.fno}`: the root of the guard's state.
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub probes: &'a dyn Probes,
}

/// One guard verdict for a Bash call: refuse, warn-and-proceed, or allow.
#[derive(Debug)]
pub(super) enum Verdict {
    Allow,
    Warn(String),
    Block(String),
}

pub fn run(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let payload: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match judge(&payload, &cwd) {
        Verdict::Block(reason) => super::emit_block(&reason),
        Verdict::Warn(reason) => super::emit_warn(&reason),
        Verdict::Allow => super::emit_allow(),
    }
}

/// The verdict for one payload with the real probes.
pub(super) fn judge(payload: &Value, cwd: &Path) -> Verdict {
    let probes = RealProbes {
        cwd: cwd.to_path_buf(),
    };
    let ctx = Ctx {
        home: fno_home(),
        cwd: cwd.to_path_buf(),
        probes: &probes,
    };
    judge_verdict(payload, &ctx)
}

fn fno_home() -> PathBuf {
    match std::env::var_os("FNO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".fno"))
            .unwrap_or_else(|| PathBuf::from(".fno")),
    }
}

/// The verdict for one payload: refuse, warn-and-proceed, or allow. A
/// block anywhere outranks a warning anywhere, which outranks an allow.
pub(super) fn judge_verdict(payload: &Value, ctx: &Ctx) -> Verdict {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return Verdict::Allow;
    }
    let Some(command) = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
    else {
        return Verdict::Allow;
    };
    // On an open quote command position is unknowable: every gate falls back
    // to a deny-leaning read of the whole string.
    let segments = segments::command_segments(command).ok();
    // Script doors: a shell runner on a script file or a heredoc stdin
    // hides the script's lines from command position, so the gates never
    // see a push inside. Read what the runner will execute and parse it as
    // more command segments; a script the guard cannot read refuses.
    let mut script_refusal: Option<String> = None;
    let mut all_segs: Vec<Segment> = Vec::new();
    if let Some(segs) = &segments {
        all_segs.extend(segs.iter().cloned());
        match script_gate::expand(command, segs, ctx) {
            Ok(extra) => all_segs.extend(extra),
            Err(reason) => script_refusal = Some(reason.into()),
        }
    }
    if let Some(reason) = script_refusal {
        return Verdict::Block(reason);
    }

    // The closure line is enforced by the merge gate (check-pr-node-closure
    // reds the PR); the hook notice reminds before that cost, then proceeds.
    let seg_view: Option<&[Segment]> = match &segments {
        Some(_) => Some(&all_segs),
        None => None,
    };
    let mut notice: Option<String> = None;
    if let Some(reason) = gh_gates::closure_refusal(command, seg_view, ctx) {
        notice = Some(reason);
    }

    let git_segs = match &segments {
        Some(_) => git_gate::find_git_segments(&all_segs),
        None if git_gate::mentions_git(command) => vec![command.to_string()],
        None => Vec::new(),
    };
    let has_approval = git_gate::fresh_marker(&git_gate::approval_flag(&ctx.home));

    // A deny writes state, so evaluation stops at the first one. A deny
    // anywhere outranks an allow anywhere: one approval never carries a
    // sibling segment another gate refuses. A warning outranks nothing: a
    // gate's notice is remembered, and surfaces only when no gate refuses
    // (the closure notice keeps its head start over the debounce notice).
    let mut git_allows: Vec<(String, String)> = Vec::new();
    for seg in &git_segs {
        match git_gate::evaluate(seg, has_approval, segments.is_some(), ctx) {
            Some(git_gate::Decision::Deny(reason)) => return Verdict::Block(reason),
            Some(git_gate::Decision::Warn(reason)) => {
                if notice.is_none() {
                    notice = Some(reason);
                }
            }
            Some(git_gate::Decision::Allow(reason)) => git_allows.push((seg.clone(), reason)),
            None => {}
        }
    }
    let git_allow = git_allows.first().cloned();

    // One authorization authorizes one action.
    if git_allows.len() > 1 {
        return Verdict::Block(git_gate::compound_deny_message(command));
    }
    match &segments {
        // Nothing can be counted on the fallback, so a single-use approval
        // authorizes nothing there.
        None if git_allow.is_some() => {
            return Verdict::Block(git_gate::compound_deny_message(command))
        }
        None => {}
        Some(_) => {
            let tokens: Vec<String> = all_segs.iter().flatten().cloned().collect();
            let carries_substitution = tokens
                .iter()
                .any(|t| ["`", "$(", "<(", ">("].iter().any(|f| t.contains(f)))
                || segments::writes_a_file(&tokens)
                || segments::has_unquoted_heredoc(command);
            let alone =
                all_segs.len() == 1 && !carries_substitution && !git_gate::shell_wrapped(command);
            if git_allow.is_some() && !alone {
                return Verdict::Block(git_gate::compound_deny_message(command));
            }
        }
    }

    if let Some((seg, _)) = git_allow {
        // The claim must win: two hooks can both see a fresh flag, and only
        // the one that removes it is authorized.
        if !git_gate::claim_marker(&git_gate::approval_flag(&ctx.home)) {
            return Verdict::Block(git_gate::no_verify_deny_message(&seg, &ctx.home));
        }
    }
    match notice {
        Some(reason) => Verdict::Warn(reason),
        None => Verdict::Allow,
    }
}

/// The block-only view the sibling gates' tests read: a refusal, or None to
/// allow. A warning is not a refusal, so it reads as None here; tests that
/// need the notice call [`judge_verdict`].
#[cfg(test)]
pub(super) fn judge_with(payload: &Value, ctx: &Ctx) -> Option<String> {
    match judge_verdict(payload, ctx) {
        Verdict::Block(reason) => Some(reason),
        _ => None,
    }
}

struct RealProbes {
    cwd: PathBuf,
}

impl RealProbes {
    fn git(&self, args: &[&str], secs: u64) -> Option<String> {
        let out = crate::loopcheck::bounded_read_unadmitted(
            "git".as_ref(),
            args,
            &self.cwd,
            "git-protection",
            Duration::from_secs(secs),
        )
        .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn exe(&self, args: &[&str], timeout: Duration) -> Exec {
        let exe = std::env::current_exe().map_err(|e| format!("no binary path ({e})"))?;
        self.read_program(exe.as_os_str(), args, timeout)
    }

    fn read_program(&self, program: &std::ffi::OsStr, args: &[&str], timeout: Duration) -> Exec {
        crate::merge_gates::ScopedReads::for_checkout(&self.cwd)
            .program(program, args, &self.cwd, timeout)
            .map(|out| {
                (
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stderr_tail).into_owned(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                )
            })
    }
}

impl Probes for RealProbes {
    fn current_branch(&self) -> Option<String> {
        self.git(&["symbolic-ref", "--short", "HEAD"], 1)
            .map(|out| out.trim().to_string())
            .filter(|b| !b.is_empty())
    }

    fn branch_node_ids(&self, head_ref: &str) -> Vec<String> {
        let payload = serde_json::json!({"head_ref": head_ref, "verify": true});
        let (code, stdout, stderr) = crate::org_board::pr_closure_produce::leg_branch_ids(&payload);
        let ids = (code == 0)
            .then(|| serde_json::from_str::<Value>(&stdout).ok())
            .flatten()
            .and_then(|v| v.get("ids").and_then(Value::as_array).cloned());
        match ids {
            Some(ids) => ids
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            None => {
                let cause = stderr.trim();
                let cause = if cause.is_empty() {
                    format!("exit {code}")
                } else {
                    cause.to_string()
                };
                eprintln!(
                    "[fno closure trailer] could not read branch node ids ({cause}); \
                     run fno doctor update --rust"
                );
                Vec::new()
            }
        }
    }

    fn in_flight(&self, branch: &str) -> Option<Value> {
        let (_, _, stdout) = self
            .exe(
                &["do", "pr", "push", "--in-flight", branch],
                Duration::from_secs(15),
            )
            .ok()?;
        let line = stdout
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{'))?;
        serde_json::from_str::<Value>(line)
            .ok()
            .filter(Value::is_object)
    }

    fn push_stamps_dir(&self) -> PathBuf {
        crate::pr_push::default_stamps_dir()
    }

    fn emit_push_bypass(&self, branch: &str) {
        let payload = serde_json::json!({ "branch": branch }).to_string();
        let _ = crate::loopcheck::bounded_read_unadmitted(
            "fno".as_ref(),
            &[
                "doctor",
                "event",
                "emit",
                "push_debounce_bypass",
                "--json",
                &payload,
            ],
            &self.cwd,
            "git-protection",
            Duration::from_secs(10),
        );
    }
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;
    use std::cell::RefCell;

    /// A scripted world: every field answers its probe, and `calls` records
    /// each bypass door the guard asked.
    #[derive(Default)]
    pub struct Fake {
        pub branch: Option<String>,
        pub node_ids: Vec<String>,
        pub in_flight: Option<Value>,
        pub stamps: PathBuf,
        pub calls: RefCell<Vec<String>>,
    }

    impl Probes for Fake {
        fn current_branch(&self) -> Option<String> {
            self.branch.clone()
        }
        fn branch_node_ids(&self, _head_ref: &str) -> Vec<String> {
            self.node_ids.clone()
        }
        fn in_flight(&self, _branch: &str) -> Option<Value> {
            self.in_flight.clone()
        }
        fn push_stamps_dir(&self) -> PathBuf {
            self.stamps.clone()
        }
        fn emit_push_bypass(&self, _branch: &str) {
            self.calls.borrow_mut().push("bypass".to_string());
        }
    }

    pub fn bash(command: &str) -> Value {
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}})
    }
}
