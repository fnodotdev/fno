//! The git-protection Bash guard. It refuses a push to a protected branch,
//! `--no-verify` and its core.hooksPath twin, a raw `gh pr merge` without
//! two-factor evidence, and a direct GraphQL PR read. It WARNS on a `gh pr
//! create` whose branch names a node and whose command shows no composed
//! closure line (the merge gate still reds the PR), and on a push that
//! repeats one inside the debounce window or lands while a GitHub run is in
//! flight (the 2026-11-07 guard-purge ruling: process and pacing gates
//! are not one of the four harms).
//!
//! A PreToolUse allow approves the whole Bash call, so anything this guard
//! authorizes (a merge, an approved `--no-verify`) must stand alone. Its
//! state lives under `${FNO_HOME:-~/.fno}`. Git reads run in the process
//! cwd, never the payload cwd.

mod gh_gates;
mod git_gate;
mod merge_gate;
mod script_gate;
mod segments;

use crate::authorized_merge::{Blocker, PrFacts, ProbeOutcome};
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
    fn repo_roots(&self) -> Vec<PathBuf>;
    fn pr_worktree(&self, cwd: &Path, branch: &str) -> Option<PathBuf>;
    fn review_claims_dir(&self, cwd: &Path) -> Option<PathBuf>;
    fn repo_slug(&self, cwd: &Path) -> Option<String>;
    fn merge_repo(&self) -> Option<String>;
    fn merge_host(&self) -> Option<String>;
    fn merge_facts(&self, cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String>;
    fn dispatch_hold(&self, cwd: &Path, facts: &PrFacts) -> ProbeOutcome;
    fn visual_hold(&self, cwd: &Path, facts: &PrFacts) -> Option<Blocker>;
    fn architect_evidence(&self, _cwd: &Path, _facts: &PrFacts) -> Option<Blocker> {
        None
    }
    fn disarm(&self, cwd: &Path, facts: &PrFacts) -> String;
    fn docs_warning(&self, cwd: &Path, facts: &PrFacts) -> Option<Blocker>;
    fn branch_node_ids(&self, head_ref: &str) -> Vec<String>;
    fn in_flight(&self, branch: &str) -> Option<Value>;
    fn merge_switch(&self, repo_root: &Path) -> bool;
    /// The lineage and coverage doors, scoped to the merge's repository.
    fn veto(&self, cwd: &Path, args: &[&str], timeout: Duration) -> Exec;
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

    let (merge_segs, git_segs) = match &segments {
        Some(_) => (
            merge_gate::find_merge_segments(&all_segs),
            git_gate::find_git_segments(&all_segs),
        ),
        None => (
            vec![vec![command.to_string()]; merge_gate::merge_mentions(command)],
            if git_gate::mentions_git(command) {
                vec![command.to_string()]
            } else {
                Vec::new()
            },
        ),
    };
    let has_approval = merge_gate::fresh_marker(&git_gate::approval_flag(&ctx.home));

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
    let merge_seg = merge_segs.first();
    let git_allow = git_allows.first().cloned();
    let merge_joined = merge_seg.map(|seg| seg.join(" "));

    // One authorization authorizes one action.
    if merge_segs.len() > 1 || git_allows.len() > 1 || (merge_seg.is_some() && git_allow.is_some())
    {
        return Verdict::Block(merge_gate::compound_deny_message(command));
    }
    match &segments {
        // Nothing can be counted on the fallback, so a single-use approval
        // authorizes nothing there. The merge gate keeps its own path.
        None if git_allow.is_some() => {
            return Verdict::Block(merge_gate::compound_deny_message(command))
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
                all_segs.len() == 1 && !carries_substitution && !merge_gate::shell_wrapped(command);
            if (merge_seg.is_some() || git_allow.is_some()) && !alone {
                return Verdict::Block(merge_gate::compound_deny_message(command));
            }
        }
    }

    if let Some(seg) = merge_seg {
        // Retain parsed arguments so quoted text cannot become repository flags.
        // The fallback's whole-string segment is one token: not parsed args.
        let words = if segments.is_some() {
            Some(seg.as_slice())
        } else {
            None
        };
        match merge_gate::judge_merge(merge_joined.as_deref().unwrap_or(command), words, ctx) {
            Verdict::Block(reason) => return Verdict::Block(reason),
            Verdict::Warn(reason) => notice = Some(reason),
            Verdict::Allow => {}
        }
    }
    if let Some((seg, _)) = git_allow {
        // The claim must win: two hooks can both see a fresh flag, and only
        // the one that removes it is authorized.
        if !merge_gate::claim_marker(&git_gate::approval_flag(&ctx.home)) {
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

    fn gate_reads(&self, _cwd: &Path, _facts: &PrFacts) -> crate::merge_gates::ScopedReads {
        crate::merge_gates::ScopedReads::for_checkout(_cwd)
    }
}

impl Probes for RealProbes {
    fn current_branch(&self) -> Option<String> {
        self.git(&["symbolic-ref", "--short", "HEAD"], 1)
            .map(|out| out.trim().to_string())
            .filter(|b| !b.is_empty())
    }

    fn repo_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut add = |p: &str| {
            let path = PathBuf::from(p);
            let path = path.canonicalize().unwrap_or(path);
            if !roots.contains(&path) {
                roots.push(path);
            }
        };
        if let Some(top) = self.git(&["rev-parse", "--show-toplevel"], 1) {
            if !top.trim().is_empty() {
                add(top.trim());
            }
        }
        if let Some(list) = self.git(&["worktree", "list", "--porcelain"], 2) {
            for line in list.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    add(path.trim());
                }
            }
        }
        for root in crate::territory::workspace_paths(&self.cwd).into_values() {
            if Path::new(&root).is_dir() {
                add(&root);
            }
        }
        roots
    }

    fn pr_worktree(&self, cwd: &Path, branch: &str) -> Option<PathBuf> {
        let worktree = crate::pr_worktree::resolve(cwd, branch).ok()?;
        worktree
            .is_dir()
            .then(|| worktree.canonicalize().unwrap_or(worktree))
    }

    fn review_claims_dir(&self, cwd: &Path) -> Option<PathBuf> {
        // The store a review hold is acquired in: the repo's space, shared
        // by every worktree of the checkout.
        crate::claims_root::claims_dir_in(
            "review:branch:",
            None,
            || Ok(cwd.to_path_buf()),
            std::env::var_os("FNO_CLAIMS_ROOT"),
            None,
        )
        .ok()
    }

    fn repo_slug(&self, cwd: &Path) -> Option<String> {
        crate::merge_gates::ScopedReads::for_checkout(cwd)
            .repository()
            .map(str::to_string)
    }

    fn merge_repo(&self) -> Option<String> {
        std::env::var("GH_REPO").ok().filter(|s| !s.is_empty())
    }

    fn merge_host(&self) -> Option<String> {
        std::env::var("GH_HOST")
            .ok()
            .filter(|value| !value.is_empty())
    }

    fn merge_facts(&self, cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String> {
        crate::merge_gates::ScopedReads::for_checkout(cwd).info(&self.cwd, pr)
    }

    fn dispatch_hold(&self, cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
        if std::env::var("FNO_TRACKER_BACKEND").unwrap_or_else(|_| "graph".into()) != "graph" {
            return ProbeOutcome::Clear;
        }
        crate::gate_probes::dispatch_hold(cwd, facts)
    }

    fn visual_hold(&self, cwd: &Path, facts: &PrFacts) -> Option<Blocker> {
        crate::merge_gates::visual_approval_blocker(
            &self.gate_reads(cwd, facts),
            cwd,
            facts.number,
            &facts.head_sha,
        )
    }

    fn architect_evidence(&self, cwd: &Path, facts: &PrFacts) -> Option<Blocker> {
        crate::merge_architect::evidence(
            &crate::authorized_merge::RealProbes,
            cwd,
            facts,
            false,
            &[],
        )
    }

    fn docs_warning(&self, cwd: &Path, facts: &PrFacts) -> Option<Blocker> {
        crate::docs_gate::docs_gate_blocker(&self.gate_reads(cwd, facts), cwd, facts)
    }

    fn disarm(&self, cwd: &Path, facts: &PrFacts) -> String {
        let repo = crate::merge_grant::qualified_repo_from_pr_url(&facts.url).unwrap_or_default();
        crate::merge_hold::disarm_automerge(cwd, facts.number, &repo)
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

    fn merge_switch(&self, repo_root: &Path) -> bool {
        crate::agents_config::auto_merge_enabled(repo_root)
    }

    fn veto(&self, cwd: &Path, args: &[&str], timeout: Duration) -> Exec {
        Self {
            cwd: cwd.to_path_buf(),
        }
        .exe(args, timeout)
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
    use std::collections::HashMap;

    /// A scripted world: every field answers its probe, and `calls` records
    /// each veto door the guard asked.
    #[derive(Default)]
    pub struct Fake {
        pub branch: Option<String>,
        pub roots: Vec<PathBuf>,
        pub pr_worktree: Option<PathBuf>,
        pub repo_slugs: HashMap<PathBuf, String>,
        pub ambient_repo: Option<String>,
        pub ambient_host: Option<String>,
        pub facts: Option<PrFacts>,
        pub facts_error: Option<String>,
        pub hold: Option<ProbeOutcome>,
        pub visual: Option<Blocker>,
        pub architect: Option<Blocker>,
        pub docs: Option<Blocker>,
        pub scopes: RefCell<Vec<PathBuf>>,
        pub disarms: RefCell<Vec<(PathBuf, u64, String)>>,
        pub claims_dir: Option<PathBuf>,
        pub node_ids: Vec<String>,
        pub in_flight: Option<Value>,
        pub switch: bool,
        pub vetoes: HashMap<&'static str, Exec>,
        pub stamps: PathBuf,
        pub calls: RefCell<Vec<String>>,
    }

    impl Probes for Fake {
        fn current_branch(&self) -> Option<String> {
            self.branch.clone()
        }
        fn repo_roots(&self) -> Vec<PathBuf> {
            self.roots.clone()
        }
        fn pr_worktree(&self, cwd: &Path, _branch: &str) -> Option<PathBuf> {
            self.scopes.borrow_mut().push(cwd.to_path_buf());
            self.pr_worktree.clone()
        }
        fn review_claims_dir(&self, _cwd: &Path) -> Option<PathBuf> {
            self.claims_dir.clone()
        }
        fn repo_slug(&self, cwd: &Path) -> Option<String> {
            self.repo_slugs.get(cwd).cloned()
        }
        fn merge_repo(&self) -> Option<String> {
            self.ambient_repo.clone()
        }
        fn merge_host(&self) -> Option<String> {
            self.ambient_host.clone()
        }
        fn merge_facts(&self, cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String> {
            self.scopes.borrow_mut().push(cwd.to_path_buf());
            if let Some(error) = &self.facts_error {
                return Err(error.clone());
            }
            if pr.is_none() && self.branch.is_none() {
                return Err("no current branch".into());
            }
            Ok(self.facts.clone().unwrap_or_else(|| PrFacts {
                number: pr.unwrap_or(42),
                head_sha: "a".repeat(40),
                head_ref: self.branch.clone().unwrap_or_else(|| "feature/x".into()),
                base_ref: "main".into(),
                url: format!(
                    "https://github.com/{}/pull/{}",
                    self.repo_slug(cwd).unwrap_or_else(|| "o/r".into()),
                    pr.unwrap_or(42)
                ),
                body: Some(String::new()),
                state: "OPEN".into(),
                armed: false,
            }))
        }
        fn dispatch_hold(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
            self.hold.clone().unwrap_or(ProbeOutcome::Clear)
        }
        fn visual_hold(&self, _cwd: &Path, _facts: &PrFacts) -> Option<Blocker> {
            self.visual.clone()
        }
        fn architect_evidence(&self, _cwd: &Path, _facts: &PrFacts) -> Option<Blocker> {
            self.architect.clone()
        }
        fn docs_warning(&self, _cwd: &Path, _facts: &PrFacts) -> Option<Blocker> {
            self.docs.clone()
        }
        fn disarm(&self, cwd: &Path, facts: &PrFacts) -> String {
            self.disarms
                .borrow_mut()
                .push((cwd.to_path_buf(), facts.number, facts.url.clone()));
            "issued".into()
        }
        fn branch_node_ids(&self, _head_ref: &str) -> Vec<String> {
            self.node_ids.clone()
        }
        fn in_flight(&self, _branch: &str) -> Option<Value> {
            self.in_flight.clone()
        }
        fn merge_switch(&self, _repo_root: &Path) -> bool {
            self.switch
        }
        fn veto(&self, cwd: &Path, args: &[&str], _timeout: Duration) -> Exec {
            self.scopes.borrow_mut().push(cwd.to_path_buf());
            let door = args[args.len() - 2];
            self.calls.borrow_mut().push(door.to_string());
            self.vetoes
                .get(door)
                .cloned()
                .unwrap_or_else(|| Ok((0, String::new(), String::new())))
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

#[cfg(test)]
mod scoped_read_tests {
    use super::*;
    use crate::merge_gates::GateReads;

    #[test]
    fn resolved_reads_pin_actual_child_repo_and_host() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo-b");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["remote", "add", "origin", "https://github.com/owner/b.git"],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .status()
                .unwrap()
                .success());
        }
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "state_dir = {:?}\n[merge]\nvisual_paint_paths = [\"paint.rs\"]\n",
                dir.path()
            ),
        )
        .unwrap();
        let log = dir.path().join("targets");
        let script = format!(
            r#"#!/bin/sh
printf '%s|%s|%s\n' "$GH_REPO" "$GH_HOST" "$*" >> '{}'
case "$1" in
  api) printf '%s\n' other.rs ;;
  pr) printf '%s\n' '{{"baseRefName":"main","headRefName":"feature"}}' ;;
  *) printf '%s\n' '{{}}' ;;
esac
"#,
            log.display()
        );
        crate::write_exec_stub(dir.path(), "gh", &script);
        let fno = crate::write_exec_stub(dir.path(), "fno", &script);
        let _fno = crate::claims::EnvVarGuard::set("FNO_BIN", fno.to_str().unwrap());
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path = crate::claims::EnvVarGuard::set("PATH", &path);
        let _cfg = crate::claims::EnvVarGuard::set("FNO_CONFIG", config.to_str().unwrap());
        let _repo = crate::claims::EnvVarGuard::set("GH_REPO", "owner/a");
        let _host = crate::claims::EnvVarGuard::set("GH_HOST", "ambient.invalid");
        let world = RealProbes { cwd: repo.clone() };
        assert!(std::process::Command::new("git")
            .args([
                "config",
                "url./local-mirror/.insteadOf",
                "https://github.com/"
            ])
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success());
        assert_eq!(world.repo_slug(&repo).as_deref(), Some("owner/b"));
        let facts = PrFacts {
            number: 42,
            head_sha: "a".repeat(40),
            head_ref: "feature".into(),
            base_ref: "main".into(),
            url: "https://github.com/owner/b/pull/42".into(),
            body: Some(String::new()),
            state: "OPEN".into(),
            armed: false,
        };
        assert!(world.visual_hold(&repo, &facts).is_none());
        assert!(world.docs_warning(&repo, &facts).is_none());
        assert_eq!(
            crate::merge_gates::pr_base_head_refs(&world.gate_reads(&repo, &facts), &repo, 42),
            Some(("main".into(), "feature".into()))
        );
        world
            .read_program(
                fno.as_os_str(),
                &["pr", "coverage-check", "42"],
                Duration::from_secs(5),
            )
            .unwrap();
        world
            .gate_reads(&repo, &facts)
            .fno_read(
                &repo,
                &[
                    "backlog".into(),
                    "decisions".into(),
                    "subject".into(),
                    "--json".into(),
                ],
            )
            .unwrap();
        let targets = std::fs::read_to_string(log).unwrap();
        eprintln!("Actual scoped child targets:\n{targets}");
        for required in ["|pr coverage-check 42", "|backlog decisions subject --json"] {
            assert!(
                targets.lines().any(|target| target.contains(required)),
                "missing scoped read {required}: {targets}"
            );
        }
        assert!(
            !targets.contains("{owner}"),
            "unscoped API endpoint: {targets}"
        );
        assert!(
            targets.contains("api repos/owner/b/pulls/42/files"),
            "{targets}"
        );
        assert!(
            targets.contains("--repo owner/b"),
            "unscoped pr view: {targets}"
        );
        for target in targets.lines() {
            assert!(
                target.starts_with("owner/b|github.com|"),
                "wrong read target: {target}"
            );
        }
    }
}
