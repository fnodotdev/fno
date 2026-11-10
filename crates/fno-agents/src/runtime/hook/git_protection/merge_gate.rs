//! The raw `gh pr merge` gate. Four vetoes run first, in this order: the
//! declared plan dispatch hold, the stacked base, review coverage and
//! a running review (each fails open on its own machinery). Then the
//! two-factor allow: an active target session armed for auto-merge plus
//! external-review evidence. Last, the operator's single-use override
//! marker, recorded in an append-only log. `fno do pr merge` is the
//! sanctioned primitive and this hook does not gate it.

use super::segments::{base_lower, effective_argv, split_words, Segment};
use super::Ctx;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// The lineage and coverage probes share a bound inside the hook budget.
pub(crate) const VETO_PROBE_TIMEOUT: Duration = Duration::from_secs(24);
/// Both markers expire: a forgotten sentinel must not hold a gate open.
const MARKER_TTL: Duration = Duration::from_secs(300);
const SESSION_FRESH: Duration = Duration::from_secs(3600);

fn merge_marker(home: &Path) -> PathBuf {
    home.join("merge-gate.disabled")
}

fn override_log(home: &Path) -> PathBuf {
    crate::state_layout::place(home, "merge-gate-overrides.log")
}

fn age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

/// True if `path` is younger than the TTL. An expired marker is removed.
pub(super) fn fresh_marker(path: &Path) -> bool {
    match age(path) {
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

fn is_merge(argv: &[String]) -> bool {
    argv.len() >= 3
        && base_lower(&argv[0]) == "gh"
        && argv[1].eq_ignore_ascii_case("pr")
        && argv[2].eq_ignore_ascii_case("merge")
}

/// Every `gh pr merge` segment, so one override cannot authorize three.
/// The segment is returned whole, so the caller passes the merge's own
/// argv to the gate (a script-derived merge is not the first segment).
pub(super) fn find_merge_segments(segs: &[Segment]) -> Vec<Segment> {
    segs.iter()
        .filter(|seg| is_merge(&effective_argv(seg)))
        .cloned()
        .collect()
}

/// The fallback's count of merge mentions in an unsplittable command.
pub(super) fn merge_mentions(command: &str) -> usize {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)gh\s+pr\s+merge").expect("static regex"))
        .find_iter(command)
        .count()
}

/// A shell runner's `-c` argument is one segment however many commands it
/// holds, so it never stands alone.
pub(super) fn shell_wrapped(command: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\b(sh|bash|zsh|dash|ksh)\b[^|;&]*\s-\w*c\b").expect("static regex")
    })
    .is_match(command)
}

enum ScopeSetting {
    Inherit,
    Clear,
    Value(String),
}

impl ScopeSetting {
    fn assign(value: &str) -> Self {
        if value.is_empty() {
            Self::Clear
        } else {
            Self::Value(value.into())
        }
    }
    fn resolve(&self, ambient: Option<String>) -> Option<String> {
        match self {
            Self::Inherit => ambient,
            Self::Clear => None,
            Self::Value(value) => Some(value.clone()),
        }
    }
}

struct MergeTarget {
    operand: Option<String>,
    repo: ScopeSetting,
    host: ScopeSetting,
}

fn repo_name(value: &str) -> Option<String> {
    crate::pr_list::parse_origin_slug(value).or_else(|| {
        let value = value.trim_matches('/');
        let (owner, repo) = value.split_once('/')?;
        (!owner.is_empty() && !repo.is_empty() && !repo.contains('/') && !owner.contains(':'))
            .then(|| value.to_lowercase())
    })
}

fn merge_target(tokens: &[String]) -> Result<MergeTarget, String> {
    let start = tokens
        .windows(3)
        .position(|t| matches!(t, [bin, pr, merge] if base_lower(bin) == "gh" && pr == "pr" && merge == "merge"))
        .ok_or("merge target is unreadable")?
        + 3;
    let mut repo = ScopeSetting::Inherit;
    let mut host = ScopeSetting::Inherit;
    let prefix = tokens
        .get(..start - 3)
        .ok_or("merge prefix is unreadable")?;
    let mut in_env = false;
    let mut at = 0;
    while at < prefix.len() {
        let token = prefix.get(at).ok_or("merge prefix is unreadable")?;
        if token.starts_with("GIT_DIR=")
            || token.starts_with("GIT_WORK_TREE=")
            || token.starts_with("GIT_COMMON_DIR=")
            || token.starts_with("GIT_CONFIG_")
        {
            return Err("inline Git context has no resolved merge scope".into());
        }
        if let Some(value) = token.strip_prefix("GH_REPO=") {
            repo = ScopeSetting::assign(value);
        } else if let Some(value) = token.strip_prefix("GH_HOST=") {
            host = ScopeSetting::assign(value);
        } else if base_lower(token) == "env" {
            in_env = true;
        } else if in_env && matches!(token.as_str(), "-i" | "--ignore-environment") {
            repo = ScopeSetting::Clear;
            host = ScopeSetting::Clear;
        } else if in_env {
            if matches!(token.as_str(), "-C" | "--chdir")
                || token.starts_with("--chdir=")
                || token.starts_with("-C")
            {
                return Err("environment working directory has no resolved merge scope".into());
            }
            let unset = if matches!(token.as_str(), "-u" | "--unset") {
                at += 1;
                prefix.get(at).map(String::as_str)
            } else {
                token
                    .strip_prefix("--unset=")
                    .or_else(|| token.strip_prefix("-u"))
            };
            match unset {
                Some("GH_REPO") => repo = ScopeSetting::Clear,
                Some("GH_HOST") => host = ScopeSetting::Clear,
                _ => {}
            }
        }
        at += 1;
    }
    let mut operand = None;
    let mut i = start;
    while i < tokens.len() {
        let token = tokens.get(i).ok_or("merge arguments are unreadable")?;
        if matches!(token.as_str(), "-R" | "--repo") {
            i += 1;
            repo =
                ScopeSetting::Value(tokens.get(i).ok_or("repository flag has no value")?.clone());
        } else if let Some(value) = token
            .strip_prefix("--repo=")
            .or_else(|| token.strip_prefix("-R"))
        {
            repo = ScopeSetting::Value(value.to_string());
        } else if matches!(
            token.as_str(),
            "--author-email"
                | "--body"
                | "-b"
                | "--body-file"
                | "-F"
                | "--subject"
                | "-t"
                | "--match-head-commit"
        ) {
            i += 1;
        } else if !token.starts_with('-') && operand.is_none() {
            operand = Some(token.clone());
        }
        i += 1;
    }
    if let Some(url) = operand.as_deref().filter(|t| t.contains("/pull/")) {
        let clean = url
            .split(['?', '#'])
            .next()
            .unwrap_or(url)
            .trim_end_matches('/');
        let identity =
            crate::merge_grant::pr_identity_from_url(clean).ok_or("PR URL is unreadable")?;
        let number = u64::try_from(identity.number)
            .ok()
            .filter(|n| *n > 0)
            .ok_or("PR URL number is invalid")?;
        let named = identity.repo.to_lowercase();
        let base = clean.rsplit_once("/pull/").ok_or("PR URL is unreadable")?.0;
        if repo_name(base).as_deref() != Some(named.as_str()) {
            return Err("PR URL repository is unresolved".into());
        }
        if repo
            .resolve(None)
            .as_deref()
            .and_then(repo_name)
            .is_some_and(|r| r != named)
        {
            return Err("repository selectors disagree".into());
        }
        repo = ScopeSetting::Value(named);
        host = ScopeSetting::Value("github.com".into());
        operand = Some(number.to_string());
    }
    Ok(MergeTarget {
        operand,
        repo,
        host,
    })
}

/// The probe's refusal line: the first line of its stderr, else stdout.
fn first_line(stderr: &str, stdout: &str) -> Option<String> {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    text.trim().lines().next().map(str::to_string)
}

/// Lineage and coverage: deny only on the door's refusal exit (3). A
/// timeout, a missing door or any other exit fails open, because a guard
/// whose own machinery is down must not become a merge outage.
fn open_veto(ctx: &Ctx, door: &str, pr: &str, fallback: String) -> Option<String> {
    match ctx
        .probes
        .veto(&ctx.cwd, &["pr", door, pr], VETO_PROBE_TIMEOUT)
    {
        Ok((3, stderr, stdout)) => Some(first_line(&stderr, &stdout).unwrap_or(fallback)),
        _ => None,
    }
}

/// `%XX` decoding for the claim key in a lockfile name.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit)
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Report registered reviews without making their presence a merge veto.
fn review_in_flight_warning(ctx: &Ctx) -> Option<String> {
    let claims = ctx.probes.review_claims_dir(&ctx.cwd)?;
    let mut held: Vec<String> = std::fs::read_dir(&claims)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("review%3Abranch%3A") && n.ends_with(".lock"))
        .collect();
    if held.is_empty() {
        return None;
    }
    held.sort();
    let names: Vec<String> = held
        .iter()
        .take(3)
        .map(|n| unquote(&n[..n.len() - ".lock".len()]))
        .collect();
    Some(format!(
        "a review is registered as in flight in this repo ({}); merging now can ship the code \
         the review is still fixing",
        names.join(", ")
    ))
}

/// The frontmatter of an active target session at `state`: fresh within
/// the hour and `status: IN_PROGRESS`. A candidate it cannot read is
/// skipped, never fatal: discovery spans every worktree.
fn active_state(state: &Path) -> Option<HashMap<String, String>> {
    if age(state)? > SESSION_FRESH {
        return None;
    }
    let text = match std::fs::read_to_string(state) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[git-protection] skip {}: {e}", state.display());
            return None;
        }
    };
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut fm = HashMap::new();
    for line in lines {
        let s = line.trim();
        if s == "---" {
            break;
        }
        if let Some((k, v)) = s.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            fm.insert(k.trim().to_string(), v.to_string());
        }
    }
    fm.get("status")
        .is_some_and(|s| s.eq_ignore_ascii_case("IN_PROGRESS"))
        .then_some(fm)
}

fn artifact_path(root: &Path, sid: &str) -> PathBuf {
    root.join(".fno")
        .join("artifacts")
        .join(format!("external-{sid}.md"))
}

fn artifact_pr(path: &Path) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let text = std::fs::read_to_string(path).ok()?;
    R.get_or_init(|| Regex::new(r#"(?m)^pr_number:\s*["']?(\d+)["']?\s*$"#).expect("static regex"))
        .captures(&text)
        .map(|c| c[1].to_string())
}

type Session = (PathBuf, HashMap<String, String>, PathBuf);

/// The one active session that authorizes this merge, failing closed on
/// ambiguity. The PR binds to its head branch in the resolved repository.
fn active_session(facts: &super::PrFacts, ctx: &Ctx) -> Option<Session> {
    let root = ctx.probes.pr_worktree(&ctx.cwd, &facts.head_ref)?;
    if ctx
        .probes
        .repo_slug(&ctx.cwd)
        .is_some_and(|repo| ctx.probes.repo_slug(&root).as_deref() != Some(repo.as_str()))
    {
        return None;
    }
    let roots = [root];
    let matches: Vec<Session> = roots
        .into_iter()
        .filter_map(|root| {
            // The space slice first, the pre-space checkout manifest second.
            let state = crate::target_orient::manifest_path(&root)?;
            active_state(&state).map(|fm| (state, fm, root))
        })
        .collect();
    let pr = facts.number.to_string();
    let mut neutral = Vec::new();
    for session in matches {
        let sid = session.1.get("session_id").map(|s| s.trim()).unwrap_or("");
        let recorded = (!sid.is_empty())
            .then(|| artifact_pr(&artifact_path(&session.2, sid)))
            .flatten();
        match recorded {
            Some(r) if r == pr => return Some(session),
            Some(_) => {}
            None => neutral.push(session),
        }
    }
    (neutral.len() == 1)
        .then(|| neutral.into_iter().next())
        .flatten()
}

fn truthy(v: Option<&String>) -> bool {
    v.is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "1"))
}

/// The two-factor allow reason, or None. The live switch must agree with
/// the manifest: a snapshot `true` never outlives the operator turning
/// auto-merge off, unless the run carries the spawn-time env grant.
fn two_factor_allows(facts: &super::PrFacts, ctx: &Ctx) -> Option<String> {
    let (state, fm, root) = active_session(facts, ctx)?;
    if !truthy(fm.get("auto_merge_approved")) {
        return None;
    }
    let env_grant = fm.get("auto_merge_source").map(|s| s.trim()) == Some("env-target-auto-merge");
    if !env_grant && !ctx.probes.merge_switch(&root) {
        return None;
    }
    Some(format!("active {} with merge consent", state.display()))
}

/// One line per consumed override, newlines flattened so a command cannot
/// forge entries. False when the line could not be written: the caller
/// refuses rather than allow an unrecorded override.
fn audit(message: &str, ctx: &Ctx) -> bool {
    use std::io::Write;
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    eprintln!("[Git Protection: merge-gate override] {flat}");
    let path = override_log(&ctx.home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{stamp} cwd={} {flat}", ctx.cwd.display()))
        .is_ok()
}

/// The merge verdict for one lone `gh pr merge` segment.
pub(super) fn judge_merge(seg: &str, parsed_args: Option<&[String]>, ctx: &Ctx) -> super::Verdict {
    use super::{PrFacts, ProbeOutcome, Verdict};
    let mut warnings = Vec::new();
    let parsed = parsed_args.is_some();
    let target = match parsed_args {
        Some(words) => merge_target(words),
        None => split_words(seg)
            .ok_or_else(|| "merge arguments are unreadable".to_string())
            .and_then(|words| merge_target(&words)),
    };
    let root = match &target {
        Ok(target) => {
            let named = target.repo.resolve(ctx.probes.merge_repo());
            let qualified = named
                .as_deref()
                .and_then(crate::pr_list::parse_origin_slug)
                .is_some();
            let host = if qualified {
                Some("github.com".into())
            } else {
                target.host.resolve(ctx.probes.merge_host())
            };
            if host
                .as_deref()
                .is_some_and(|host| !host.eq_ignore_ascii_case("github.com"))
            {
                warnings.push("merge host has no supported local read scope".into());
                None
            } else {
                match named {
                    Some(raw) => repo_name(&raw).and_then(|repo| {
                        ctx.probes
                            .repo_roots()
                            .into_iter()
                            .chain([ctx.cwd.clone()])
                            .find(|root| {
                                ctx.probes.repo_slug(root).as_deref() == Some(repo.as_str())
                            })
                    }),
                    None => Some(ctx.cwd.clone()),
                }
            }
        }
        Err(error) => {
            warnings.push(error.clone());
            None
        }
    };
    let scope = root.as_ref().map(|root| Ctx {
        home: ctx.home.clone(),
        cwd: root.clone(),
        probes: ctx.probes,
    });
    let numeric = target
        .as_ref()
        .ok()
        .and_then(|t| t.operand.as_deref())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0);
    let mut facts = None;
    if let Some(scope) = &scope {
        let read = if target
            .as_ref()
            .ok()
            .and_then(|t| t.operand.as_deref())
            .is_some()
            && numeric.is_none()
        {
            Err("branch selector cannot be resolved".into())
        } else if numeric.is_none() && ctx.probes.current_branch().is_none() {
            Err("current branch is unreadable".into())
        } else {
            ctx.probes.merge_facts(&scope.cwd, numeric)
        };
        match read {
            Ok(read)
                if numeric.is_some_and(|n| read.number != n)
                    || ctx.probes.repo_slug(&scope.cwd).is_some_and(|repo| {
                        crate::merge_grant::repo_slug_from_pr_url(&read.url)
                            .map(|s| s.to_lowercase())
                            .as_deref()
                            != Some(repo.as_str())
                    }) =>
            {
                warnings.push("PR facts do not match the requested repository and number".into());
            }
            Ok(read) => facts = Some(read),
            Err(error) => warnings.push(format!("dispatch hold lookup unavailable: {error}")),
        }
        let partial = numeric.and_then(|number| {
            ctx.probes.repo_slug(&scope.cwd).map(|repo| PrFacts {
                number,
                head_sha: String::new(),
                head_ref: String::new(),
                base_ref: String::new(),
                url: format!("https://github.com/{repo}/pull/{number}"),
                body: None,
                state: "OPEN".into(),
                armed: false,
            })
        });
        if let Some(known) = facts.as_ref().or(partial.as_ref()) {
            match ctx.probes.dispatch_hold(&scope.cwd, known) {
                ProbeOutcome::Clear => {}
                ProbeOutcome::Refused(why) => {
                    return Verdict::Block(format!("[fno agents dispatch-hold] {why}"))
                }
                ProbeOutcome::Inconclusive(why) => warnings.push(why),
            }
        }
        if let Some(facts) = &facts {
            if let Some(blocker) = ctx.probes.architect_evidence(&scope.cwd, facts) {
                let detail = format!("{}: {}", blocker.code, blocker.detail);
                if crate::merge_architect::holds(&blocker) {
                    return Verdict::Block(detail);
                }
                warnings.push(detail);
            }
            if let Some(blocker) = ctx.probes.visual_hold(&scope.cwd, facts) {
                if blocker.code == "visual_declined" {
                    let note = ctx.probes.disarm(&scope.cwd, facts);
                    eprintln!("hold: auto-merge disarm for PR {}: {note}", facts.number);
                    return Verdict::Block(format!(
                        "[fno visual-approval] {}: {}",
                        blocker.code, blocker.detail
                    ));
                }
                warnings.push(format!(
                    "[fno visual-approval] {}: {}",
                    blocker.code, blocker.detail
                ));
            }
            if let Some(warning) = ctx.probes.docs_warning(&scope.cwd, facts) {
                warnings.push(format!("{}: {}", warning.code, warning.detail));
            }
        }
        if let Some(pr) = facts.as_ref().map(|f| f.number).or(numeric) {
            let pr = pr.to_string();
            if let Some(why) = open_veto(
                scope,
                "base-lineage-check",
                &pr,
                format!("PR {pr}: base no longer leads to the default branch"),
            ) {
                return Verdict::Block(format!("[fno stacked-base] {why}"));
            }
            if let Some(why) = open_veto(
                scope,
                "coverage-check",
                &pr,
                format!("PR {pr}: review coverage refused"),
            ) {
                warnings.push(format!("[fno do review-coverage] {why}"));
            }
            if let Some(why) = review_in_flight_warning(scope) {
                warnings.push(format!("[fno review-hold] {why}"));
            }
        }
    } else {
        warnings.push("dispatch hold repository has no matching local checkout".into());
    }
    let allowed = scope
        .as_ref()
        .zip(facts.as_ref())
        .is_some_and(|(scope, facts)| two_factor_allows(facts, scope).is_some());
    if !allowed {
        let marker = merge_marker(&ctx.home);
        if !(parsed && fresh_marker(&marker) && claim_marker(&marker)) {
            return Verdict::Block(merge_deny_message(seg, &ctx.home));
        }
        if !audit(
            &format!("two-factor check failed, merge allowed by marker: {seg}"),
            ctx,
        ) {
            return Verdict::Block(unrecordable_override_message(&ctx.home));
        }
    }
    if warnings.is_empty() {
        Verdict::Allow
    } else {
        Verdict::Warn(warnings.join("; "))
    }
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
applies to the WHOLE Bash call - so `gh pr merge 1 && <anything>` would
approve the anything too. This command carries more than the one action:

  • several `gh pr merge` or `--no-verify` segments, or both kinds
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

fn unrecordable_override_message(home: &Path) -> String {
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: merge-gate override could not be recorded
{BOX_BOTTOM}

The marker was valid but the trail could not be written, and the trail
is what justifies having an override at all - so this is refused rather
than allowed unrecorded. Check that this path is an appendable file (not
a directory) with a writable parent:
  {}

The marker was consumed; fix the path, then touch it again:
  touch {}

{RULE}
",
        override_log(home).display(),
        merge_marker(home).display()
    )
}

fn merge_deny_message(command: &str, home: &Path) -> String {
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: gh pr merge (two-factor check failed)
{BOX_BOTTOM}

Command: {command}

Raw `gh pr merge` is gated at the shipping boundary. The sanctioned merge
primitive is `fno do pr merge`, which runs its own footnote-canonical guards
and is not blocked by this hook.

Auto-merge directly from Claude Code requires ALL of:
  1. The live merge switch armed: `fno config get auto_merge.enabled`
     answers true, OR the state file carries `auto_merge_source:
     env-target-auto-merge` (the run's spawn-time grant)
  2. Active target state file with `auto_merge_approved: true`
     (only the target manifest authorizes merge)

Review coverage, registered reviews and missing looks are warning evidence.
They do not replace either authority factor. A user's explicit decline
still holds the exact PR head it names.

⚠️  Operator escape hatch, when the two-factor path is genuinely broken
    (e.g. an immutable manifest carrying the wrong value):

  touch {}

    `gh pr merge` only: it cannot open a push to a protected branch and
    cannot open --no-verify. Expires in 5 minutes, consumed on use, every
    use appended to {}. An agent must not create this
    unprompted - that is a logged violation, not a silent bypass.

{RULE}
",
        merge_marker(home).display(),
        override_log(home).display()
    )
}

#[cfg(test)]
mod tests {
    use super::super::fake::{bash, Fake};
    use super::super::{judge_with, Ctx, Exec};
    use super::*;

    fn ctx<'a>(dir: &Path, fake: &'a Fake) -> Ctx<'a> {
        Ctx {
            home: dir.join("home"),
            cwd: dir.to_path_buf(),
            probes: fake,
        }
    }

    fn refuse(code: i32, line: &str) -> Exec {
        Ok((code, line.to_string(), String::new()))
    }

    /// A worktree with an active session armed for auto-merge, its
    /// external artifact recording `pr`.
    fn session(root: &Path, pr: &str, extra: &str) -> Fake {
        let fno = root.join(".fno");
        std::fs::create_dir_all(fno.join("artifacts")).unwrap();
        std::fs::write(
            fno.join("target-state.md"),
            format!(
                "---\nstatus: IN_PROGRESS\nsession_id: s1\nauto_merge_approved: true\n{extra}---\n"
            ),
        )
        .unwrap();
        std::fs::write(
            fno.join("artifacts").join("external-s1.md"),
            format!("---\nphase: external\nsession_id: s1\npr_number: {pr}\n---\n"),
        )
        .unwrap();
        Fake {
            pr_worktree: Some(root.to_path_buf()),
            roots: vec![root.to_path_buf()],
            switch: true,
            ..Fake::default()
        }
    }

    #[test]
    fn the_hold_veto_fails_closed_and_runs_first() {
        let dir = tempfile::tempdir().unwrap();
        for reason in ["PR42: plan is held", "PR42: declared hold is malformed"] {
            let fake = Fake {
                hold: Some(super::super::ProbeOutcome::Refused(reason.into())),
                ..Fake::default()
            };
            for command in [
                "gh pr merge 42",
                "gh pr merge 42 --body 'explain --repo ghost/unavailable'",
                "gh pr merge --body 'several words' 42",
                "gh pr merge 42 # --repo ghost/unavailable",
            ] {
                let r = judge_with(&bash(command), &ctx(dir.path(), &fake)).unwrap();
                assert_eq!(
                    r,
                    format!("[fno agents dispatch-hold] {reason}"),
                    "{command}"
                );
            }
            assert!(fake.calls.borrow().is_empty(), "nothing after the hold ran");
        }
        let down = Fake {
            hold: Some(super::super::ProbeOutcome::Inconclusive(
                "read timed out".into(),
            )),
            ..Fake::default()
        };
        let c = ctx(dir.path(), &down);
        std::fs::create_dir_all(&c.home).unwrap();
        for command in [
            "gh pr merge 42",
            "gh pr merge --squash",
            "gh pr merge 42 --repo o/other",
            "GH_REPO=o/other gh pr merge 42",
            "gh pr merge https://github.com/o/other/pull/42",
        ] {
            std::fs::write(merge_marker(&c.home), b"").unwrap();
            assert!(
                matches!(
                    super::super::judge_verdict(&bash(command), &c),
                    super::super::Verdict::Warn(_)
                ),
                "{command}"
            );
        }
        assert!(!down.calls.borrow().iter().any(|call| call == "hold-check"));
    }

    #[test]
    fn lineage_refuses_but_missing_coverage_warns() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let mut fake = session(&wt, "42", "");
        fake.vetoes.insert(
            "base-lineage-check",
            refuse(3, "base-lineage: REFUSED - stale"),
        );
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).unwrap();
        assert_eq!(r, "[fno stacked-base] base-lineage: REFUSED - stale");
        fake.vetoes.clear();
        fake.vetoes
            .insert("base-lineage-check", refuse(4, "unknown"));
        fake.vetoes.insert("coverage-check", refuse(3, ""));
        let verdict = super::super::judge_verdict(&bash("gh pr merge 42"), &ctx(dir.path(), &fake));
        assert!(
            matches!(verdict, super::super::Verdict::Warn(ref note) if note.contains("[fno do review-coverage]")),
            "{verdict:?}"
        );
        let mut fake = Fake::default();
        fake.vetoes.insert("coverage-check", Err("timeout".into()));
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).unwrap();
        assert!(
            r.contains("two-factor check failed"),
            "an open veto falls to the gate: {r}"
        );
    }

    #[test]
    fn review_and_missing_look_warn_but_a_user_decline_disarms() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let claims = dir.path().join(".fno").join("claims");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::write(claims.join("review%3Abranch%3Afeature%2Fx.lock"), b"").unwrap();
        let mut fake = session(&wt, "42", "");
        fake.claims_dir = Some(claims);
        let verdict = super::super::judge_verdict(&bash("gh pr merge 42"), &ctx(dir.path(), &fake));
        assert!(
            matches!(verdict, super::super::Verdict::Warn(ref note) if note.contains("[fno review-hold]")),
            "{verdict:?}"
        );
        let mut look = session(&wt, "42", "");
        look.visual = Some(super::super::Blocker::held(
            "visual_approval",
            "a look is required",
        ));
        let verdict = super::super::judge_verdict(&bash("gh pr merge 42"), &ctx(dir.path(), &look));
        assert!(
            matches!(verdict, super::super::Verdict::Warn(ref note) if note.contains("visual_approval: a look is required")),
            "{verdict:?}"
        );
        assert!(look.disarms.borrow().is_empty());
        for code in [
            "architect_misses_plan",
            "architect_off_vision",
            "architect_not_run",
            "architect_unavailable",
            "architect_granted",
        ] {
            let mut fake = session(&wt, "42", "");
            fake.architect = Some(super::super::Blocker::held(code, "cached judgment"));
            let verdict =
                super::super::judge_verdict(&bash("gh pr merge 42"), &ctx(dir.path(), &fake));
            if crate::merge_architect::holds(fake.architect.as_ref().unwrap()) {
                assert!(
                    matches!(verdict, super::super::Verdict::Block(ref detail) if detail.contains(code)),
                    "{verdict:?}"
                );
            } else {
                assert!(
                    matches!(verdict, super::super::Verdict::Warn(ref detail) if detail.contains(code)),
                    "{verdict:?}"
                );
            }
        }
        let mut declined = session(&wt, "42", "");
        declined.visual = Some(super::super::Blocker::held(
            "visual_declined",
            "the user declined this head",
        ));
        assert!(
            judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &declined))
                .unwrap()
                .contains("visual_declined")
        );
        assert_eq!(
            *declined.disarms.borrow(),
            [(
                dir.path().to_path_buf(),
                42,
                "https://github.com/o/r/pull/42".into()
            )]
        );
    }

    #[test]
    fn two_factor_allows_only_the_bound_armed_session() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let mut fake = session(&wt, "42", "");
        assert_eq!(
            judge_with(&bash("gh pr merge 42 --squash"), &ctx(dir.path(), &fake)),
            None
        );
        // A session bound to another PR, a disarmed switch and a compound
        // command never authorize.
        assert!(judge_with(&bash("gh pr merge 43"), &ctx(dir.path(), &fake)).is_some());
        let artifact = artifact_path(&wt, "s1");
        let saved = std::fs::read(&artifact).unwrap();
        std::fs::remove_file(&artifact).unwrap();
        assert_eq!(
            judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)),
            None
        );
        std::fs::write(artifact, saved).unwrap();
        assert!(judge_with(&bash("gh pr merge 42 && rm -rf x"), &ctx(dir.path(), &fake)).is_some());
        assert!(judge_with(
            &bash("gh pr merge 42 --body \"`id`\""),
            &ctx(dir.path(), &fake)
        )
        .is_some());
        fake.switch = false;
        assert!(judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).is_some());
        session(&wt, "42", "auto_merge_source: env-target-auto-merge\n");
        assert_eq!(
            judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)),
            None
        );
        fake.switch = true;
        fake.branch = Some("feature/x".into());
        fake.repo_slugs
            .insert(dir.path().to_path_buf(), "o/r".into());
        fake.repo_slugs.insert(wt.clone(), "o/r".into());
        for command in [
            "gh pr merge",
            "gh pr merge --repo o/r 42",
            "gh pr merge 42 -Ro/r",
            "gh pr merge https://github.com/o/r/pull/42",
            "GH_REPO=o/r gh pr merge 42",
        ] {
            assert_eq!(
                judge_with(&bash(command), &ctx(dir.path(), &fake)),
                None,
                "{command}"
            );
        }
        let other = dir.path().join("other");
        session(&other, "42", "");
        fake.roots.push(other.clone());
        fake.repo_slugs.insert(other.clone(), "o/other".into());
        for command in [
            "gh pr merge 42 --repo o/other",
            "gh pr merge --repo o/other 42",
            "GH_REPO=o/other gh pr merge 42",
            "gh pr merge https://github.com/o/other/pull/42",
        ] {
            fake.pr_worktree = Some(wt.clone());
            assert!(
                judge_with(&bash(command), &ctx(dir.path(), &fake))
                    .unwrap()
                    .contains("two-factor check failed"),
                "{command}"
            );
            fake.pr_worktree = Some(other.clone());
            fake.scopes.borrow_mut().clear();
            assert_eq!(
                judge_with(&bash(command), &ctx(dir.path(), &fake)),
                None,
                "{command}"
            );
            assert!(fake.scopes.borrow().iter().all(|scope| scope == &other));
        }
        fake.ambient_repo = Some("o/other".into());
        assert_eq!(
            judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)),
            None
        );
        assert_eq!(
            judge_with(
                &bash("GH_REPO=o/r GH_REPO=o/other gh pr merge 42"),
                &ctx(dir.path(), &fake)
            ),
            None
        );
        fake.pr_worktree = Some(wt.clone());
        for command in [
            "GH_REPO= gh pr merge 42",
            "env -u GH_REPO gh pr merge 42",
            "env --unset GH_REPO gh pr merge 42",
            "GH_REPO=o/other env --unset=GH_REPO gh pr merge 42",
            "env -i gh pr merge 42",
        ] {
            assert_eq!(
                judge_with(&bash(command), &ctx(dir.path(), &fake)),
                None,
                "{command}"
            );
        }
        fake.ambient_host = Some("ambient.invalid".into());
        assert!(
            judge_with(&bash("gh pr merge 42 --repo o/r"), &ctx(dir.path(), &fake))
                .unwrap()
                .contains("two-factor check failed")
        );
        for command in [
            "gh pr merge https://github.com/o/r/pull/42",
            "gh pr merge --repo github.com/o/r 42",
            "GH_HOST=github.com gh pr merge --repo o/r 42",
            "GH_HOST= GH_REPO= gh pr merge 42",
            "env -u GH_HOST -u GH_REPO gh pr merge 42",
        ] {
            assert_eq!(
                judge_with(&bash(command), &ctx(dir.path(), &fake)),
                None,
                "{command}"
            );
        }
        fake.ambient_repo = None;
        fake.ambient_host = None;
        assert_eq!(
            judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)),
            None
        );
        for command in [
            "env --chdir /other gh pr merge 42",
            "GIT_DIR=/other gh pr merge 42",
        ] {
            assert!(
                judge_with(&bash(command), &ctx(dir.path(), &fake)).is_some(),
                "{command}"
            );
        }
    }

    #[test]
    fn the_override_marker_is_single_use_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake {
            docs: Some(super::super::Blocker::held(
                "docs_line_missing",
                "name the doc",
            )),
            ..Fake::default()
        };
        let c = ctx(dir.path(), &fake);
        std::fs::create_dir_all(&c.home).unwrap();
        std::fs::write(merge_marker(&c.home), b"").unwrap();
        match super::super::judge_verdict(&bash("gh pr merge 42"), &c) {
            super::super::Verdict::Warn(note) => {
                assert!(note.contains("docs_line_missing: name the doc"))
            }
            other => panic!("expected docs warning, got {other:?}"),
        }
        assert!(!merge_marker(&c.home).exists());
        let log = std::fs::read_to_string(override_log(&c.home)).unwrap();
        assert!(
            log.contains("merge allowed by marker: gh pr merge 42"),
            "{log}"
        );
        assert!(judge_with(&bash("gh pr merge 42"), &c)
            .unwrap()
            .contains("two-factor check failed"));
        // One marker never covers two merges.
        std::fs::write(merge_marker(&c.home), b"").unwrap();
        let r = judge_with(&bash("gh pr merge 1; gh pr merge 2"), &c).unwrap();
        assert!(
            r.contains("one approval cannot authorize several actions"),
            "{r}"
        );
        // An unwritable trail refuses rather than allow unrecorded.
        std::fs::remove_file(override_log(&c.home)).unwrap();
        std::fs::create_dir_all(override_log(&c.home)).unwrap();
        let r = judge_with(&bash("gh pr merge 42"), &c).unwrap();
        assert!(r.contains("override could not be recorded"), "{r}");
    }
}
