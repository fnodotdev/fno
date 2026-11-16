//! The git segment gates: a push to a protected branch, `--no-verify` and
//! its core.hooksPath twin, and the push debounce notice. The branch gate
//! and the `--no-verify` gate are one protection with two doors:
//! `--no-verify` skips .git/hooks/pre-push, and that hook is the branch
//! guard. No marker opens a push to a protected branch. The debounce is a
//! warning, not a refusal (the 2026-11-07 guard-purge ruling): pacing
//! is not one of the four harms (data loss, secret leak, real money,
//! irreversible public act), and the standing CI law wants supersede-and-push.

use super::segments::{self, base_lower, effective_argv, Segment};
use super::Ctx;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

pub(super) const PROTECTED_BRANCHES: &[&str] = &["main", "master", "develop", "dev"];
/// Subcommands this gate never judges, matched by position (argv[1]), so a
/// commit message reading "git push origin main" is not a push.
const ALLOWED_GIT_SUBCOMMANDS: &[&str] = &[
    "status", "log", "diff", "branch", "checkout", "fetch", "pull", "add", "commit", "stash",
    "show", "config", "remote", "tag", "grep",
];
/// Appended after the synthetic `--no-verify` that stands for a hooksPath
/// override, so the deny path can print the right text.
const HOOKS_PATH_MARK: &str = "--hooks-path-override";

pub(super) enum Decision {
    Deny(String),
    /// A pacing notice: the call proceeds; the text surfaces as a warning.
    Warn(String),
    Allow(String),
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

fn flag_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\s+(-[a-zA-Z]+|--[a-zA-Z-]+)(=\S+)?")
}

pub(super) fn approval_flag(home: &Path) -> PathBuf {
    home.join("approve_no_verify.flag")
}

/// The approval marker expires: a forgotten sentinel must not hold a gate open.
const MARKER_TTL: Duration = Duration::from_secs(300);

fn marker_age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

/// True if `path` is younger than the TTL. An expired marker is removed.
pub(super) fn fresh_marker(path: &Path) -> bool {
    match marker_age(path) {
        Some(a) if a < MARKER_TTL => true,
        Some(_) => {
            let _ = std::fs::remove_file(path);
            false
        }
        None => false,
    }
}

/// The unlink IS the claim: of two hooks that both saw a fresh marker, only
/// the one that removes it is authorized.
pub(super) fn claim_marker(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
}

fn state_file(home: &Path) -> PathBuf {
    crate::state_layout::place(home, "git-protection.json")
}

/// Record the refused command. Best-effort: refusing is not, so a write
/// that fails never turns the deny into an allow.
fn record_block(home: &Path, command: &str) {
    let path = state_file(home);
    let mut state = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    state["last_blocked_command"] = json!(command);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&state).unwrap_or_default(),
    );
}

/// The executable-onward string of every segment whose command is `git`,
/// with git's global options dropped. A hooksPath override is encoded as a
/// trailing `--no-verify` it is equivalent to.
pub(super) fn find_git_segments(segs: &[Segment]) -> Vec<String> {
    let mut out = Vec::new();
    for seg in segs {
        let argv = effective_argv(seg);
        if argv.first().map(|t| base_lower(t)).as_deref() != Some("git") {
            continue;
        }
        let mut normalized = segments::strip_git_global_opts(&argv);
        if segments::sets_hooks_path(&argv) {
            normalized.push("--no-verify".to_string());
            normalized.push(HOOKS_PATH_MARK.to_string());
        }
        out.push(normalized.join(" "));
    }
    out
}

/// The fallback's deny-leaning read: any `git` word hands the whole
/// command to the gate.
pub(super) fn mentions_git(command: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)\bgit\b").is_match(command)
}

/// A shell runner's `-c` argument is one segment however many commands it
/// holds, so an approval never rides inside one.
pub(super) fn shell_wrapped(command: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\b(sh|bash|zsh|dash|ksh)\b[^|;&]*\s-\w*c\b").expect("static regex")
    })
    .is_match(command)
}

/// The destination branch a push names, or None. Flags with an attached
/// value drop whole; `feature:main`, `+main` and `refs/heads/main` all
/// read as `main`.
fn extract_branch_from_push(command: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let cleaned = flag_re().replace_all(command, " ");
    let caps = re(&R, r"(?i)git\s+push\s+(?:\S+\s+)?(\S+)").captures(&cleaned)?;
    let refspec = &caps[1];
    let dest = refspec.rsplit(':').next().unwrap_or(refspec);
    let dest = dest.trim_start_matches('+');
    let dest = ["refs/heads/", "heads/"]
        .iter()
        .find_map(|p| dest.strip_prefix(p))
        .unwrap_or(dest);
    Some(dest.to_string())
}

/// True only for `git push <remote> <branch>` whose target is not HEAD or @:
/// a remote-only or current-branch push falls through to the branch check.
fn push_names_explicit_dest(command: &str) -> bool {
    static CONT: OnceLock<Regex> = OnceLock::new();
    static PUSH: OnceLock<Regex> = OnceLock::new();
    let cleaned = re(&CONT, r"\\\s*\n").replace_all(command, " ");
    let cleaned = flag_re().replace_all(&cleaned, " ");
    let Some(caps) = re(&PUSH, r"(?i)git\s+push\b(.*)$").captures(&cleaned) else {
        return false;
    };
    let args: Vec<&str> = caps[1].split_whitespace().collect();
    args.len() >= 2 && !matches!(args.last(), Some(&"HEAD") | Some(&"@"))
}

pub(super) fn is_a_push(command: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)git\s+push").is_match(command)
}

/// The protected branch this push lands on, or None.
fn protected_push_target(command: &str, ctx: &Ctx) -> Option<String> {
    static ALL: OnceLock<Regex> = OnceLock::new();
    if !is_a_push(command) {
        return None;
    }
    // `--all` and `--mirror` update every ref without naming one.
    if re(&ALL, r"\s--(all|mirror)\b").is_match(command) {
        return Some("all refs (--all/--mirror)".to_string());
    }
    let explicit = extract_branch_from_push(command);
    if let Some(branch) = &explicit {
        if PROTECTED_BRANCHES.contains(&branch.as_str()) {
            return Some(branch.clone());
        }
        // A named non-protected destination is authoritative over the cwd's
        // branch: a background ship from a cwd on main pushes feature/x.
        if push_names_explicit_dest(command) {
            return None;
        }
    }
    ctx.probes
        .current_branch()
        .filter(|b| PROTECTED_BRANCHES.contains(&b.as_str()))
}

/// Positional: argv[1] is the subcommand whatever the message text says.
/// A `--no-verify` token anywhere disqualifies.
fn is_allowed_git_command(command: &str) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    parts.len() >= 2
        && !parts.iter().any(|p| p.eq_ignore_ascii_case("--no-verify"))
        && ALLOWED_GIT_SUBCOMMANDS.contains(&parts[1].to_lowercase().as_str())
}

/// One git segment: a deny, an allow that rides the `--no-verify`
/// approval, or None (no opinion). `allowlist_ok` is false on the fallback,
/// where argv[1] is the first command's subcommand and positions nothing.
pub(super) fn evaluate(
    command: &str,
    has_approval: bool,
    allowlist_ok: bool,
    ctx: &Ctx,
) -> Option<Decision> {
    if allowlist_ok && is_allowed_git_command(command) {
        return None;
    }
    // The branch gate first: it outranks the --no-verify approval, so
    // `git push --no-verify origin main` is a branch refusal.
    let protected = protected_push_target(command, ctx);
    if let Some(branch) = &protected {
        record_block(&ctx.home, command);
        let no_verify = command.to_lowercase().contains("--no-verify");
        return Some(Decision::Deny(push_deny_message(
            command, branch, no_verify,
        )));
    }
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.iter().any(|p| p.eq_ignore_ascii_case("--no-verify")) {
        if has_approval {
            return Some(Decision::Allow(
                "[Approved] User approved --no-verify commit".to_string(),
            ));
        }
        if parts.ends_with(&["--no-verify", HOOKS_PATH_MARK]) {
            let shown = parts[..parts.len() - 2].join(" ");
            return Some(Decision::Deny(hooks_path_deny_message(&shown, &ctx.home)));
        }
        return Some(Decision::Deny(no_verify_deny_message(command, &ctx.home)));
    }
    // Pacing, not safety: last among the push gates so a safety refusal is
    // the sentence the operator reads. The debounce warns and proceeds.
    if is_a_push(command) {
        let branch = extract_branch_from_push(command)
            .or_else(|| ctx.probes.current_branch())
            .unwrap_or_default();
        if let Some(notice) = push_debounce_warning(&branch, ctx) {
            return Some(Decision::Warn(notice));
        }
        stamp_push(&branch, ctx);
    }
    None
}

fn stamp_path(branch: &str, ctx: &Ctx) -> PathBuf {
    let branch = if branch.is_empty() { "HEAD" } else { branch };
    let safe: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    ctx.probes.push_stamps_dir().join(format!("{safe}.stamp"))
}

/// Warning text when this push would repeat one inside the debounce window
/// or land while a run is in flight. The call PROCEEDS either way; the
/// text rides the allow as a notice (the 2026-11-07 guard-purge ruling). The stamp covers
/// the window before GitHub registers the last push's run; the in-flight
/// read covers a run already visible. Every probe failure allows quietly.
/// `FNO_PUSH_NOW=1` journals the bypass and skips the notice.
///
/// `fno do pr push` never re-enters here: the hook reads the Bash command,
/// and that command is not a `git push` at command position.
fn push_debounce_warning(branch: &str, ctx: &Ctx) -> Option<String> {
    if std::env::var("FNO_PUSH_NOW").as_deref() == Ok("1") {
        ctx.probes
            .emit_push_bypass(if branch.is_empty() { "HEAD" } else { branch });
        return None;
    }
    let age = std::fs::metadata(stamp_path(branch, ctx))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
    if let Some(age) = age.filter(|a| a.as_secs() < crate::pr_push::PUSH_DEBOUNCE_SECS) {
        return Some(format!(
            "[fno push debounce] notice: this branch was pushed {}s ago and GitHub may not \
             have registered its run yet; the push proceeds. `fno do pr push` reads the \
             real check state and pushes once the branch is safe. FNO_PUSH_NOW=1 suppresses \
             this notice.",
            age.as_secs()
        ));
    }
    let probe = ctx.probes.in_flight(branch)?;
    if probe.get("in_flight") != Some(&Value::Bool(true)) {
        return None;
    }
    let field = |k: &str| {
        probe
            .get(k)
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| (!v.is_null()).then(|| v.to_string()))
            })
            .filter(|s| !s.is_empty())
    };
    // The in-flight read reports only GitHub Actions runs a push would
    // cancel. A probe naming no job is not one, so it never holds a push.
    let job = field("job")?;
    Some(format!(
        "[fno push debounce] notice: check '{}' (job {job}, head {}) is still running; the \
         push proceeds and cancels that run. If the run is red, that is the wanted \
         supersede (`fno do pr push --force-ci-cancel` journals it). If it is green, wait \
         for `fno do pr wait <n> --until settled` first. FNO_PUSH_NOW=1 suppresses this \
         notice.",
        field("check").unwrap_or_else(|| "?".into()),
        field("head").unwrap_or_else(|| "?".into()),
    ))
}

/// Best-effort: a stamp that cannot be written costs one skipped debounce.
fn stamp_push(branch: &str, ctx: &Ctx) {
    let path = stamp_path(branch, ctx);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
}

const RULE: &str = "═══════════════════════════════════════════════════════════════════";
const BOX_TOP: &str = "╔════════════════════════════════════════════════════════════════╗";
const BOX_BOTTOM: &str = "╚════════════════════════════════════════════════════════════════╝";

pub(super) fn compound_deny_message(command: &str) -> String {
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: one approval cannot authorize several actions
{BOX_BOTTOM}

Command: {command}

An approval here authorizes exactly ONE action, and a PreToolUse allow
applies to the WHOLE Bash call - so the approved action && <anything> would
approve the anything too. This command carries more than the one action:

  • several `--no-verify` segments
  • another command joined by && / ; / | (a leading `cd` counts too)
  • a command substitution: $(...), `...`, <(...) or >(...)
    A substitution IS a second command, so it disqualifies the
    approval even though it sits inside one segment.
  • an output redirection (>, >>) or an unquoted heredoc (<<EOF),
    which would ride the approval into a file write or an expansion

Run the approved action as its own substitution-free command. To pass a
long commit message without $(cat ...), use stdin instead:

  git commit --no-verify -F - <<'EOF'
  your message
  EOF

{RULE}
"
    )
}

pub(super) fn no_verify_deny_message(command: &str, home: &Path) -> String {
    let flag = approval_flag(home);
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: --no-verify flag detected
{BOX_BOTTOM}

Command: {command}

⛔ AI assistants MUST NOT bypass git hooks with --no-verify ⛔

Why this is blocked:
  • Hooks exist to prevent mistakes (Lambda anti-patterns, security, etc.)
  • Bypassing hooks defeats their purpose
  • Only humans can make the judgment call to override hooks
  • This protects code quality and prevents production issues

The proper workflow:

  1. Review the hook's feedback/warnings
  2. Fix the issues the hook identified
  3. Commit normally (without --no-verify)

{RULE}

⚠️  To approve this --no-verify commit:

Run this command:
  touch {}

Then I'll retry the commit automatically.
Approval expires after 5 minutes and is single-use.

There is no marker that turns this gate off. --no-verify skips
.git/hooks/pre-push, and that hook is the protected-branch guard, so
this gate and the branch gate are one protection with two doors.

{RULE}
",
        flag.display()
    )
}

fn hooks_path_deny_message(command: &str, home: &Path) -> String {
    format!(
        "🚫 BLOCKED: core.hooksPath override

Command: {command}

Pointing core.hooksPath elsewhere disables .git/hooks/pre-push, the
protected-branch guard, so it is a --no-verify by another name.

Reading it is fine: `git config core.hooksPath` or `git config --get core.hooksPath`.
To approve this override:

  touch {}

Approval expires after 5 minutes and is single-use.
",
        approval_flag(home).display()
    )
}

fn push_deny_message(command: &str, branch: &str, no_verify: bool) -> String {
    let warning = if no_verify {
        "
⚠️  DETECTED: --no-verify flag
⚠️  This is EXACTLY the behavior we're trying to prevent!
⚠️  Bypassing git hooks is NOT acceptable for protected branches.
"
    } else {
        ""
    };
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: Direct push to protected branch '{branch}'
{BOX_BOTTOM}

Command: {command}
{warning}
⛔ THIS IS NOT COWBOY CODING. ⛔

Protected branches: {}

Why this is blocked:
  • Direct pushes to {branch} bypass code review
  • Changes should be reviewed by the team
  • This protects against accidental destructive changes
  • CI/CD expects PRs, not direct pushes

The proper workflow:

  1. Create a feature branch:
     git checkout -b feature/your-feature-name

  2. Make your changes and commit

  3. Push the feature branch:
     git push origin feature/your-feature-name

  4. Create a pull request for review

  5. Merge after approval

{RULE}

No marker turns this gate off. For an agent session there is
deliberately no bypass: push a branch and open a PR. A human pushing
from their own shell is not gated by this hook at all.

{RULE}
",
        PROTECTED_BRANCHES.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::super::fake::{bash, Fake};
    use super::super::{judge_with, Ctx};
    use super::*;

    fn ctx<'a>(dir: &Path, fake: &'a Fake) -> Ctx<'a> {
        Ctx {
            home: dir.join("home"),
            cwd: dir.to_path_buf(),
            probes: fake,
        }
    }

    fn feature(dir: &Path) -> Fake {
        Fake {
            branch: Some("feature/x".into()),
            stamps: dir.join("stamps"),
            ..Fake::default()
        }
    }

    #[test]
    fn a_push_to_a_protected_branch_is_refused_in_every_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "git push origin main",
            "cd /tmp && git push origin main",
            "GIT push origin main",
            "git -C /repo push origin main",
            "git push origin feature:main",
            "git push origin +main",
            "git push origin refs/heads/master",
            "git push --force-with-lease=origin/x origin main",
            "git push --all origin",
            "sudo -u me git push origin develop",
            "bash -c \"git push origin main\"",
            "echo 'oops && git push origin main",
        ] {
            let refusal = judge_with(&bash(cmd), &c).unwrap_or_else(|| panic!("allowed: {cmd}"));
            assert!(
                refusal.contains("BLOCKED: Direct push to protected branch"),
                "{cmd}: {refusal}"
            );
        }
        let state = std::fs::read_to_string(state_file(&c.home)).unwrap();
        assert!(state.contains("last_blocked_command"), "{state}");
    }

    #[test]
    fn a_feature_push_and_quoted_prose_are_not_protected_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = feature(dir.path());
        fake.branch = Some("main".into());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "git push origin feature/x",
            "git commit -m \"fix: block git push origin main\"",
            "git log --grep \"git push origin main\"",
            "fno backlog idea \"git push origin main\"",
        ] {
            assert_eq!(judge_with(&bash(cmd), &c), None, "{cmd}");
        }
        // From a cwd on main, a bare or remote-only push lands on main.
        assert!(judge_with(&bash("git push"), &c).is_some());
        assert!(judge_with(&bash("git push origin HEAD"), &c).is_some());
    }

    #[test]
    fn no_verify_needs_a_single_use_approval_that_stands_alone() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let cmd = "git commit --no-verify -m x";
        assert!(judge_with(&bash(cmd), &c)
            .unwrap()
            .contains("BLOCKED: --no-verify"));
        std::fs::create_dir_all(&c.home).unwrap();
        std::fs::write(approval_flag(&c.home), b"").unwrap();
        assert_eq!(judge_with(&bash(cmd), &c), None);
        assert!(!approval_flag(&c.home).exists(), "the approval is consumed");
        assert!(judge_with(&bash(cmd), &c).is_some(), "single use");
        // An approval never covers a sibling, a write or a protected push.
        for cmd in [
            "git commit --no-verify -m x && rm -rf /tmp/y",
            "git commit --no-verify -m x > /tmp/log",
            "git commit --no-verify -m \"$(id)\"",
            "git push --no-verify origin main",
        ] {
            std::fs::write(approval_flag(&c.home), b"").unwrap();
            assert!(judge_with(&bash(cmd), &c).is_some(), "{cmd}");
        }
        std::fs::write(approval_flag(&c.home), b"").unwrap();
        assert_eq!(
            judge_with(&bash("git commit --no-verify -m ok 2>&1"), &c),
            None
        );
    }

    #[test]
    fn a_hooks_path_override_is_a_no_verify() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("git -c core.hooksPath=/dev/null commit -m x"), &c).unwrap();
        assert!(
            refusal.contains("BLOCKED: core.hooksPath override"),
            "{refusal}"
        );
        assert!(judge_with(&bash("git config core.hooksPath /tmp/h"), &c).is_some());
        assert_eq!(
            judge_with(&bash("git config --get core.hooksPath"), &c),
            None
        );
    }

    #[test]
    fn the_debounce_warns_on_a_fresh_push_and_an_actions_run_and_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = feature(dir.path());
        let cmd = "git push origin feature/x";
        // The first push stamps; a second inside the window proceeds with a
        // notice, never a refusal.
        assert_eq!(
            super::super::judge_with(&bash(cmd), &ctx(dir.path(), &fake)),
            None
        );
        match super::super::judge_verdict(&bash(cmd), &ctx(dir.path(), &fake)) {
            super::super::Verdict::Warn(notice) => {
                assert!(notice.contains("pushed 0s ago"), "{notice}");
                assert!(notice.contains("push proceeds"), "{notice}");
            }
            other => panic!("expected a notice, got {other:?}"),
        }
        std::fs::remove_dir_all(dir.path().join("stamps")).unwrap();
        fake.in_flight =
            Some(json!({"in_flight": true, "check": "rust", "job": "77", "head": "abc"}));
        match super::super::judge_verdict(&bash(cmd), &ctx(dir.path(), &fake)) {
            super::super::Verdict::Warn(notice) => {
                assert!(
                    notice.contains("check 'rust' (job 77, head abc)"),
                    "{notice}"
                );
            }
            other => panic!("expected a notice, got {other:?}"),
        }
        // A local run with no GitHub job never notices at all.
        std::fs::remove_dir_all(dir.path().join("stamps")).ok();
        fake.in_flight =
            Some(json!({"in_flight": true, "check": "evals/gate", "job": null, "head": "abc"}));
        match super::super::judge_verdict(&bash(cmd), &ctx(dir.path(), &fake)) {
            super::super::Verdict::Allow => {}
            other => panic!("expected an allow, got {other:?}"),
        }
    }
}
