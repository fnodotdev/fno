//! Which pull requests fleet automation may act on.
//!
//! Law d-2191724b: no outside PR is merged, healed, bound or reviewed by
//! fleet lanes on its own, and there is no admit door. The owner's one
//! recorded act over any PR is the per-PR hold, an operator law row.

use crate::authorized_merge::{PrFacts, ProbeOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const HOLD_SUBJECT: &str = "pr-hold";
pub const HOLD_DECISION: &str = "do not merge this pull request";
/// The GitHub label a recorded hold puts on the PR, so `ci-required` fails
/// while the hold is live and D3383's ruleset requirement gates every
/// merge endpoint, outside fno included.
pub const HOLD_LABEL: &str = "pr-hold";
const INSIDE: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];
const WRITE_ACCESS: [&str; 3] = ["admin", "maintain", "write"];

/// Why a permission read gave no answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Unreadable(pub String);

impl From<Unreadable> for String {
    fn from(e: Unreadable) -> String {
        e.0
    }
}

/// The `permission` word of one account on one repo, or why it is unknown.
pub type Permission = Result<String, Unreadable>;

/// Reads one account's permission on a repo: `(slug, login)` to the
/// `permission` word of `collaborators/{login}/permission`.
pub type PermissionRead<'a> = &'a dyn Fn(&str, &str) -> Permission;

#[derive(Deserialize)]
struct PermissionBody {
    permission: String,
}

/// The REST path of one account's permission read.
pub fn permission_path(slug: &str, login: &str) -> String {
    format!("repos/{slug}/collaborators/{login}/permission")
}

/// The `permission` word of a collaborators permission body.
pub fn permission_of(raw: &str) -> Permission {
    serde_json::from_str::<PermissionBody>(raw)
        .map(|body| body.permission)
        .map_err(|e| Unreadable(format!("permission json: {e}")))
}

/// Some(cause) when a REST pulls object is from outside; Err when the base
/// repo or the author's permission is unreadable. A null head repo is a
/// deleted fork, so it reads outside. A same-repo author whose
/// `author_association` is not owner-class is inside when they hold write
/// access: the association depends on the token's view (an App token reads
/// a private org member as CONTRIBUTOR), the permission does not.
pub fn outside_reason(pull: &Value, permission: PermissionRead) -> Result<Option<String>, String> {
    let base = pull
        .pointer("/base/repo/full_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "the pulls object named no base repo".to_string())?;
    let head = pull.pointer("/head/repo/full_name").and_then(Value::as_str);
    if head != Some(base) {
        return Ok(Some(match head {
            Some(h) => format!("head repo {h} is not the base repo {base}"),
            None => format!("a deleted fork; the base repo is {base}"),
        }));
    }
    let association = pull
        .get("author_association")
        .and_then(Value::as_str)
        .unwrap_or("NONE");
    if INSIDE.contains(&association) {
        return Ok(None);
    }
    let not_inside =
        format!("author association {association} is not one of OWNER, MEMBER, COLLABORATOR");
    let Some(login) = pull.pointer("/user/login").and_then(Value::as_str) else {
        return Ok(Some(not_inside));
    };
    let access = permission(base, login)?;
    if WRITE_ACCESS.contains(&access.as_str()) {
        return Ok(None);
    }
    Ok(Some(format!(
        "{not_inside}, and {login} holds {access} access to {base}"
    )))
}

/// The subject one per-PR hold lives at. Not head-scoped: a hold survives
/// pushes, and the push that lands is the thing being held.
pub fn hold_subject(slug: &str, pr: u64) -> String {
    format!("{HOLD_SUBJECT}:{slug}#{pr}")
}

/// True when a decision text records the hold. The specimen rows (D3376,
/// D3379) carry a rationale after the exact phrase, so the prefix decides
/// and any continuation but a new word binds: a `:`, a comma or a space is
/// a rationale, while `do not merge this pull requests` is a near spelling.
pub fn is_hold_text(decision: &str) -> bool {
    let Some(rest) = decision.trim_start().strip_prefix(HOLD_DECISION) else {
        return false;
    };
    !rest.chars().next().is_some_and(char::is_alphanumeric)
}

/// Whether the subject names one per-PR hold (`pr-hold:<slug>#<n>`): the
/// subject form the decide door's text gate keys on and `after_record`
/// acts on.
pub fn is_hold_subject(subject: &str) -> bool {
    let Some(rest) = subject
        .trim()
        .strip_prefix(HOLD_SUBJECT)
        .and_then(|r| r.strip_prefix(':'))
    else {
        return false;
    };
    let Some((slug, pr)) = rest.split_once('#') else {
        return false;
    };
    !slug.is_empty() && pr.parse::<u64>().is_ok()
}

/// True when a decision text releases a per-PR hold: the bare word, or the
/// word followed by a rationale. This is the wording `after_record_with`
/// acts on, so the decide door's text gate and the effects path read one
/// definition and a release that records is a release that works.
pub fn is_release_text(decision: &str) -> bool {
    let release = decision.trim();
    release == "release" || release.starts_with("release:") || release.starts_with("release ")
}

/// The lifecycle states that still hold. `expired` means the PR merged
/// (nothing left to protect); superseded, retracted and retired are dead
/// rows. `unscoped` means the store cannot prove the PR's closure, and
/// `unknown` means the graph read failed: both keep the hold, the same
/// fail-closed direction the probe takes on an unreadable read.
fn holds_lifecycle(lifecycle: Option<&str>) -> bool {
    match lifecycle {
        None => true,
        Some(state) => matches!(state, "live" | "unscoped" | "unknown"),
    }
}

/// One decisions row the hold reader needs, typed so the reader grows no
/// `.get("field")` string reads (docs/rust-style.md R1). Unknown fields
/// ride through.
#[derive(Deserialize)]
struct HoldRow {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    lifecycle: Option<String>,
    #[serde(default)]
    decision_id: Option<String>,
}

#[derive(Deserialize)]
struct HoldPayload {
    decisions: Vec<HoldRow>,
}

/// The rows of one `fno backlog decisions --json` payload, or `None` when
/// the payload carries no `decisions` array. The hold reader takes every
/// authority on purpose: the specimens (D3376, D3379) are coord-lane role
/// rows, the waiver reader's operator filter is its own trust boundary and
/// stays untouched.
fn hold_rows(stdout: &[u8]) -> Option<Vec<HoldRow>> {
    serde_json::from_slice::<HoldPayload>(stdout)
        .ok()
        .map(|p| p.decisions)
}

/// Ok(Some(decision_id)) when any binding row at the subject records the
/// hold. The specimens are coord-lane role rows whose text carries a
/// rationale, so no lane or authority filter applies here: the subject is
/// the identity and the release is the operator-supersede door. `None`
/// stdout (the read failed) and a malformed payload are Err, never
/// absence: the hold probe builds on this and must fail closed.
pub fn held(decisions_stdout: Option<&[u8]>) -> Result<Option<String>, String> {
    let Some(bytes) = decisions_stdout else {
        return Err("the decisions read did not answer".to_string());
    };
    let Some(rows) = hold_rows(bytes) else {
        return Err("malformed decisions payload".to_string());
    };
    for row in rows {
        if is_hold_text(row.decision.as_deref().unwrap_or(""))
            && holds_lifecycle(row.lifecycle.as_deref())
        {
            return Ok(Some(
                row.decision_id
                    .unwrap_or_else(|| "unreadable id".to_string()),
            ));
        }
    }
    Ok(None)
}

/// The per-PR origin-facts cache directory: `pr_status_cache_dir` with its
/// last segment swapped for `pr-origin`, so it follows the cache class home.
pub(crate) fn pr_origin_cache_dir(cwd: &Path) -> Option<PathBuf> {
    let mut dir = crate::agents_config::pr_status_cache_dir(cwd)?;
    dir.set_file_name("pr-origin");
    Some(dir)
}

fn cache_path(dir: &Path, slug: &str, pr: u64) -> Option<PathBuf> {
    let (owner, repo) = slug.split_once('/')?;
    Some(dir.join(format!("{owner}--{repo}--{pr}.json")))
}

/// The origin facts of one pulls object, exactly as read. `head_repo` is
/// None for a deleted fork; `author_permission` is None until a permission
/// read answered.
#[derive(Debug, Default, Serialize, Deserialize)]
struct OriginFacts {
    #[serde(default)]
    head_repo: Option<String>,
    #[serde(default)]
    base_repo: Option<String>,
    #[serde(default)]
    author_association: Option<String>,
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    author_permission: Option<String>,
}

fn facts_of(pull: &Value) -> OriginFacts {
    let text = |at: &str| pull.pointer(at).and_then(Value::as_str).map(str::to_string);
    OriginFacts {
        head_repo: text("/head/repo/full_name"),
        base_repo: text("/base/repo/full_name"),
        author_association: text("/author_association"),
        login: text("/user/login"),
        author_permission: None,
    }
}

/// Reshape cached facts into a pulls-like object, so the cache hit answers
/// through `outside_reason` and no second predicate exists.
fn pulls_of_facts(facts: &OriginFacts) -> Value {
    json!({
        "head": {"repo": {"full_name": facts.head_repo}},
        "base": {"repo": {"full_name": facts.base_repo}},
        "author_association": facts.author_association,
        "user": {"login": facts.login},
    })
}

/// Origin facts older than this read as a miss: an author's association and
/// access can change while the PR stays open.
const ORIGIN_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

fn read_cached(dir: &Path, slug: &str, pr: u64) -> Option<OriginFacts> {
    let path = crate::state_layout_home::cache_read(&cache_path(dir, slug, pr)?);
    let age = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    if age > ORIGIN_TTL {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// A failed cache write is ignored: the network read is the truth.
fn write_cached(dir: &Path, slug: &str, pr: u64, facts: &OriginFacts) {
    let Some(path) = cache_path(dir, slug, pr) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(body) = serde_json::to_string(facts) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The real fetch: one `gh api repos/{slug}/pulls/{n}`. Polled by the merge
/// decide path, so it admits against the fleet budget first; a hold refuses
/// in process with the ledger's own line.
fn real_fetch(slug: &str, pr: u64) -> Result<Value, String> {
    let path = format!("repos/{slug}/pulls/{pr}");
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["api", &path]);
    let out = crate::gh_budget::output(cmd).map_err(|e| format!("gh api spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh api repos/{slug}/pulls/{pr} exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("gh api pulls json: {e}"))
}

/// The real permission read, on the same budgeted channel as the pulls read.
fn real_permission(slug: &str, login: &str) -> Permission {
    let path = permission_path(slug, login);
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["api", &path]);
    let out = crate::gh_budget::output(cmd)
        .map_err(|e| Unreadable(format!("gh api spawn failed: {e}")))?;
    if !out.status.success() {
        return Err(Unreadable(format!(
            "gh api {path} exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    permission_of(&String::from_utf8_lossy(&out.stdout))
}

pub(crate) fn outside_pr(cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
    let dir = pr_origin_cache_dir(cwd);
    outside_pr_in(dir.as_deref(), facts, &real_fetch, &real_permission)
}

fn outside_pr_in(
    cache_dir: Option<&Path>,
    facts: &PrFacts,
    fetch: &dyn Fn(&str, u64) -> Result<Value, String>,
    permission: PermissionRead,
) -> ProbeOutcome {
    let Some(slug) = crate::merge_grant::repo_slug_from_pr_url(&facts.url) else {
        return ProbeOutcome::Inconclusive(
            "outside_pr: the PR URL named no repository".to_string(),
        );
    };
    let cached = cache_dir.and_then(|dir| read_cached(dir, &slug, facts.number));
    let fetched = cached.is_none();
    let (pull, mut origin) = match cached {
        Some(f) => (pulls_of_facts(&f), f),
        None => match fetch(&slug, facts.number) {
            Ok(pull) => {
                let origin = facts_of(&pull);
                (pull, origin)
            }
            Err(e) => return ProbeOutcome::Inconclusive(format!("outside_pr: {e}")),
        },
    };
    // A cached permission answers with no read; a fresh one joins the cache.
    let fresh = std::cell::RefCell::new(None::<String>);
    let read = |repo: &str, login: &str| -> Permission {
        if let Some(p) = &origin.author_permission {
            return Ok(p.clone());
        }
        let p = permission(repo, login)?;
        *fresh.borrow_mut() = Some(p.clone());
        Ok(p)
    };
    let verdict = outside_reason(&pull, &read);
    let fresh = fresh.into_inner();
    let refresh = fetched || fresh.is_some();
    if fresh.is_some() {
        origin.author_permission = fresh;
    }
    if let Some(dir) = cache_dir.filter(|_| refresh) {
        write_cached(dir, &slug, facts.number, &origin);
    }
    let login = pull
        .pointer("/user/login")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let association = pull
        .get("author_association")
        .and_then(Value::as_str)
        .unwrap_or("NONE");
    match verdict {
        Ok(None) => ProbeOutcome::Clear,
        Ok(Some(cause)) => ProbeOutcome::Refused(format!(
            "outside_pr: PR #{} is from {cause} by {login} ({association}); \
             fleet automation never merges, heals, binds or reviews an outside pull request. \
             Worth-pursuing work is rebuilt from its backlog node: see \
             docs/architecture/authorized-merge.md \"Outside pull requests\"",
            facts.number
        )),
        Err(e) => ProbeOutcome::Inconclusive(format!("outside_pr: {e}")),
    }
}

/// Ok((success, combined output)) for one `gh` invocation. Injectable so the
/// disarm table needs no network.
pub(crate) type GhRun<'a> = &'a dyn Fn(&[&str]) -> Result<(bool, String), String>;

fn real_gh(args: &[&str]) -> Result<(bool, String), String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(args);
    let out = crate::gh_budget::output(cmd).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

/// The read argv the hold probe takes: subject-only, no lane or state
/// filter. The lane filter is what hid the specimen rows (D3376, D3379 are
/// coord rows); the subject is the identity and `held()` classifies the
/// lifecycle per row.
pub(crate) fn hold_read_args(slug: &str, pr: u64) -> Vec<String> {
    ["backlog", "decisions", &hold_subject(slug, pr), "--json"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub(crate) fn pr_hold(cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
    let Some(slug) = crate::merge_grant::repo_slug_from_pr_url(&facts.url) else {
        return ProbeOutcome::Inconclusive("pr_hold: the PR URL named no repository".to_string());
    };
    // The cached in-process read answers the exact argv the CLI would take
    // in process; the spawn stays for an argv shape the cache declines.
    let args = hold_read_args(&slug, facts.number);
    let out = crate::backlog::decisions_cache::payload(&args);
    let stdout = match out {
        Some(crate::backlog::decisions_cache::Answer::Bytes(bytes)) => Some(bytes),
        Some(crate::backlog::decisions_cache::Answer::Unreadable(reason)) => {
            return ProbeOutcome::Inconclusive(format!(
                "pr_hold: the decisions read exited 1: {reason}"
            ))
        }
        None => {
            let spawned = std::process::Command::new(crate::scrape::fno_bin())
                .args(&args)
                .current_dir(cwd)
                .output();
            match spawned {
                Ok(o) if o.status.success() => Some(o.stdout),
                Ok(o) => {
                    return ProbeOutcome::Inconclusive(format!(
                        "pr_hold: the decisions read exited {}: {}",
                        o.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&o.stderr).trim()
                    ))
                }
                Err(e) => return ProbeOutcome::Inconclusive(format!("pr_hold: {e}")),
            }
        }
    };
    match held(stdout.as_deref()) {
        Ok(None) => ProbeOutcome::Clear,
        Ok(Some(id)) => {
            if facts.armed {
                let repo =
                    crate::merge_grant::qualified_repo_from_pr_url(&facts.url).unwrap_or_default();
                crate::merge_hold::disarm_automerge(cwd, facts.number, &repo);
            }
            ProbeOutcome::Refused(format!(
                "pr_hold: the owner holds PR #{} ({id}); release: fno backlog decide '{}' \
                 'release' --authority operator --supersedes {id}",
                facts.number,
                hold_subject(&slug, facts.number)
            ))
        }
        Err(e) => ProbeOutcome::Inconclusive(format!("pr_hold: {e}")),
    }
}

/// Called by the decide door after a ruling is recorded. A hold row (any
/// authority; the specimens are role rows) disarms an armed queue, drafts
/// the PR and labels it `pr-hold`. A `release` row restores ready, drops
/// the label and reruns the failed ci-required run at the head. Never
/// fails the record; other subjects return None with no process spawned.
pub(crate) fn after_record(
    subject: &str,
    decision: &str,
    authority: Option<&str>,
) -> Option<String> {
    after_record_with(&real_gh, subject, decision, authority)
}

/// Called by the decide door after a ruling is queued. A hold stops work
/// now. A release waits for the drain: until it is recorded the hold binds.
pub(crate) fn after_queue(
    subject: &str,
    decision: &str,
    authority: Option<&str>,
) -> Option<String> {
    after_queue_with(&real_gh, subject, decision, authority)
}

fn after_queue_with(
    gh: GhRun,
    subject: &str,
    decision: &str,
    authority: Option<&str>,
) -> Option<String> {
    if !is_hold_text(decision) {
        return None;
    }
    after_record_with(gh, subject, decision, authority)
}

pub(crate) fn after_record_with(
    gh: GhRun,
    subject: &str,
    decision: &str,
    authority: Option<&str>,
) -> Option<String> {
    let rest = subject.strip_prefix(HOLD_SUBJECT)?.strip_prefix(':')?;
    let (slug, pr) = rest.split_once('#')?;
    let pr: u64 = pr.parse().ok()?;
    if slug.is_empty() {
        return None;
    }
    if is_hold_text(decision) {
        return Some(hold_effects(gh, slug, pr));
    }
    // The release word takes a trailing rationale like the hold does. Only
    // an operator or role row can supersede a hold, so only those act.
    if authority != Some("operator") && authority != Some("role") {
        return None;
    }
    if is_release_text(decision) {
        return Some(release_effects(gh, slug, pr));
    }
    None
}

/// One best-effort `gh` call with its log line on any outcome. The manual
/// remedy names the exact command: the joined args.
fn gh_step(gh: GhRun<'_>, args: &[&str], done: String) -> String {
    let manual = format!("gh {}", args.join(" "));
    match gh(args) {
        Ok((true, _)) => done,
        Ok((false, output)) => {
            let last = output.lines().last().unwrap_or("").trim().to_string();
            format!("{last}; run {manual} by hand if auth or network failed")
        }
        Err(e) => format!("{e}; run {manual} by hand if auth or network failed"),
    }
}

/// Whether the PR carried an autoMergeRequest when the hold landed:
/// `Some(the answer)` from one gh read, `None` when the read failed. The
/// answer gates the receipt line, never the disarm: `--disable-auto` still
/// runs unarmed, so an auto-merge armed inside the read's blind second dies.
fn armed_auto_merge(gh: GhRun<'_>, slug: &str, pr: u64) -> Option<bool> {
    let n = pr.to_string();
    let (_ok, out) = gh(&[
        "pr",
        "view",
        &n,
        "--repo",
        slug,
        "--json",
        "autoMergeRequest",
    ])
    .ok()
    .filter(|(ok, _)| *ok)?;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AutoMergeView {
        #[serde(default)]
        auto_merge_request: Option<Value>,
    }
    // The gh channel concatenates stderr after stdout, so a warning on a
    // successful view would break a whole-string parse; the payload is the
    // span between the outermost braces, warnings ride after it.
    let text = out.trim();
    let Some(start) = text.find('{') else {
        return None;
    };
    let Some(end) = text.rfind('}') else {
        return None;
    };
    if end < start {
        return None;
    }
    let view: AutoMergeView = serde_json::from_str(&text[start..=end]).ok()?;
    Some(view.auto_merge_request.is_some())
}

fn hold_effects(gh: GhRun<'_>, slug: &str, pr: u64) -> String {
    let n = pr.to_string();
    // The armed claim prints only when the read saw an autoMergeRequest.
    // On 485, 522, 535 and 543 nothing was armed (the repo does not allow
    // auto-merge) and the old unconditional line lied.
    let armed = armed_auto_merge(gh, slug, pr);
    let disarm_done = match armed {
        Some(true) => format!("pr-hold: disabled an armed auto-merge on {slug}#{pr}"),
        _ => String::new(),
    };
    let mut lines = vec![gh_step(
        gh,
        &["pr", "merge", &n, "--repo", slug, "--disable-auto"],
        disarm_done,
    )];
    // The label may not exist in the repo yet; --force makes this idempotent.
    lines.push(gh_step(
        gh,
        &["label", "create", HOLD_LABEL, "--repo", slug, "--force"],
        format!("pr-hold: ensured the {HOLD_LABEL} label exists in {slug}"),
    ));
    lines.push(gh_step(
        gh,
        &["pr", "ready", &n, "--repo", slug, "--undo"],
        format!("pr-hold: converted {slug}#{pr} to draft"),
    ));
    lines.push(gh_step(
        gh,
        &["pr", "edit", &n, "--repo", slug, "--add-label", HOLD_LABEL],
        format!("pr-hold: labeled {slug}#{pr} {HOLD_LABEL}"),
    ));
    lines.retain(|line| !line.is_empty());
    lines.join("; ")
}

fn release_effects(gh: GhRun<'_>, slug: &str, pr: u64) -> String {
    let n = pr.to_string();
    let mut lines = vec![
        gh_step(
            gh,
            &["pr", "ready", &n, "--repo", slug],
            format!("pr-hold: marked {slug}#{pr} ready"),
        ),
        gh_step(
            gh,
            &[
                "pr",
                "edit",
                &n,
                "--repo",
                slug,
                "--remove-label",
                HOLD_LABEL,
            ],
            format!("pr-hold: removed {HOLD_LABEL} from {slug}#{pr}"),
        ),
    ];
    if let Some(line) = rerun_required_after_release(gh, slug, pr) {
        lines.push(line);
    }
    lines.join("; ")
}

/// Rerun the failed `ci-required` run at the released head, so the required
/// check re-reads the dropped label instead of waiting for the next push.
/// No failed run (or an unreadable head) skips quietly: the next push
/// evaluates the label anyway.
fn rerun_required_after_release(gh: GhRun<'_>, slug: &str, pr: u64) -> Option<String> {
    let n = pr.to_string();
    let (ok, out) = gh(&[
        "pr",
        "view",
        &n,
        "--repo",
        slug,
        "--json",
        "headRefOid",
        "--jq",
        ".headRefOid",
    ])
    .ok()
    .filter(|(ok, _)| *ok)?;
    if !ok {
        return None;
    }
    let head = out.trim().trim_matches('"').to_string();
    if head.is_empty() {
        return None;
    }
    let (ok, out) = gh(&[
        "run",
        "list",
        "--repo",
        slug,
        "-w",
        "ci-required",
        "-c",
        &head,
        "--limit",
        "1",
        "--json",
        "databaseId,conclusion",
    ])
    .ok()
    .filter(|(ok, _)| *ok)?;
    if !ok {
        return None;
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CiRequiredRun {
        #[serde(default)]
        database_id: Option<i64>,
        #[serde(default)]
        conclusion: Option<String>,
    }
    let runs: Vec<CiRequiredRun> = serde_json::from_str(&out).ok()?;
    let id = runs
        .iter()
        .find(|r| r.conclusion.as_deref() == Some("failure"))
        .and_then(|r| r.database_id)?;
    let id = id.to_string();
    Some(gh_step(
        gh,
        &["run", "rerun", &id, "--repo", slug, "--failed"],
        format!("pr-hold: reran ci-required ({id}) at the released head"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pull(head: Value, base: &str, association: &str) -> Value {
        json!({
            "head": {"repo": {"full_name": head}},
            "base": {"repo": {"full_name": base}},
            "author_association": association,
            "user": {"login": "someone"},
        })
    }

    #[test]
    fn the_outside_predicate_tables_fork_association_cache_and_fails_closed() {
        let base = "fnodotdev/fno";
        let unread = |_: &str, _: &str| -> Permission {
            panic!("an owner-class or fork PR reads no permission")
        };
        // Same repo plus OWNER reads inside.
        assert_eq!(
            outside_reason(&pull(json!("fnodotdev/fno"), base, "OWNER"), &unread),
            Ok(None)
        );
        // A fork reads outside, naming the head repo.
        let fork =
            outside_reason(&pull(json!("stranger/footnote"), base, "OWNER"), &unread).unwrap();
        assert!(fork.is_some());
        // A deleted fork (null head repo) reads outside.
        let deleted = outside_reason(&pull(Value::Null, base, "OWNER"), &unread).unwrap();
        assert!(deleted.is_some());
        // A same-repo author outside the owner class is judged by write
        // access, never by the token's view of the association: an App
        // token reads a private org member as CONTRIBUTOR.
        let access = |p: &'static str| move |_: &str, _: &str| -> Permission { Ok(p.to_string()) };
        let contributor = pull(json!(base), base, "CONTRIBUTOR");
        assert_eq!(outside_reason(&contributor, &access("admin")), Ok(None));
        assert_eq!(outside_reason(&contributor, &access("write")), Ok(None));
        let reader = outside_reason(&contributor, &access("read")).unwrap();
        assert!(reader.is_some_and(|c| c.contains("holds read access")));
        let nonmember = outside_reason(&pull(json!(base), base, "NONE"), &access("none")).unwrap();
        assert!(nonmember.is_some());
        // An unreadable permission is an unreadable pull, never inside.
        let down = |_: &str, _: &str| -> Permission { Err(Unreadable("gh down".to_string())) };
        assert!(outside_reason(&contributor, &down).is_err());
        assert_eq!(
            permission_of(r#"{"permission":"admin","role_name":"admin"}"#),
            Ok("admin".to_string())
        );
        // No base repo is an unreadable pull, not an outside one.
        let no_base = json!({"head": {"repo": json!(base)}, "author_association": "OWNER"});
        assert!(outside_reason(&no_base, &unread).is_err());

        // A cache hit answers with no fetch; a corrupt file reads as a miss.
        let tmp = std::env::temp_dir().join(format!("pr-admission-test-{}", std::process::id()));
        let dir = tmp.join("cache");
        let _ = std::fs::remove_dir_all(&dir);
        let pr7 = facts(7);
        // A cache hit: the counting fetch must stay at zero.
        cache_fixture(
            &dir,
            7,
            Some(&json!({
                "head_repo": "stranger/footnote",
                "base_repo": "fnodotdev/fno",
                "author_association": "OWNER",
                "login": "stranger",
            })),
        );
        let calls = std::cell::RefCell::new(0usize);
        let count = |_: &str, _: u64| -> Result<Value, String> {
            *calls.borrow_mut() += 1;
            Ok(Value::Null)
        };
        let out = outside_pr_in(Some(dir.as_path()), &pr7, &count, &unread);
        assert_eq!(calls.into_inner(), 0);
        assert!(
            matches!(out, ProbeOutcome::Refused(reason) if reason.contains("stranger/footnote"))
        );
        // A corrupt file reads as a miss: one fetch, then the facts are cached.
        std::fs::write(cache_path(&dir, "fnodotdev/fno", 7).unwrap(), "{not json").unwrap();
        let fork = pull(json!("stranger/footnote"), "fnodotdev/fno", "FIRST_TIMER");
        let out = outside_pr_in(Some(dir.as_path()), &pr7, &|_, _| Ok(fork.clone()), &unread);
        assert!(matches!(out, ProbeOutcome::Refused(_)));
        let cached = read_cached(&dir, "fnodotdev/fno", 7);
        assert_eq!(
            cached.and_then(|c| c.author_association),
            Some("FIRST_TIMER".to_string())
        );

        // Outside reads Refused whatever the law rows say; unreadable is Inconclusive.
        let tmp = std::env::temp_dir().join(format!("pr-admission-nc-{}", std::process::id()));
        let dir = tmp.join("cache");
        let _ = std::fs::remove_dir_all(&dir);
        // Any law rows at all admit nothing: outside is final.
        let out = outside_pr_in(
            Some(dir.as_path()),
            &facts(8),
            &|_, _| Ok(pull(json!("stranger/footnote"), "fnodotdev/fno", "OWNER")),
            &unread,
        );
        assert!(matches!(out, ProbeOutcome::Refused(reason) if reason.contains("never merges")));
        // A fetch that fails reads Inconclusive, never Clear.
        let out = outside_pr_in(
            Some(dir.as_path()),
            &facts(9),
            &|_, _| Err("gh down".to_string()),
            &unread,
        );
        assert!(matches!(out, ProbeOutcome::Inconclusive(e) if e.contains("gh down")));
        // A same-repo admin read as CONTRIBUTOR is Clear; the permission
        // joins the cache, so the second probe reads nothing.
        let admin = pull(json!("fnodotdev/fno"), "fnodotdev/fno", "CONTRIBUTOR");
        let out = outside_pr_in(
            Some(dir.as_path()),
            &facts(11),
            &|_, _| Ok(admin.clone()),
            &access("admin"),
        );
        assert!(matches!(out, ProbeOutcome::Clear));
        let out = outside_pr_in(
            Some(dir.as_path()),
            &facts(11),
            &|_, _| Err("no fetch".to_string()),
            &unread,
        );
        assert!(matches!(out, ProbeOutcome::Clear));
        // Facts past the TTL read as a miss, so a demotion reaches the gate.
        let aged = std::time::SystemTime::now() - 2 * ORIGIN_TTL;
        std::fs::File::options()
            .write(true)
            .open(cache_path(&dir, "fnodotdev/fno", 11).unwrap())
            .unwrap()
            .set_modified(aged)
            .unwrap();
        let out = outside_pr_in(
            Some(dir.as_path()),
            &facts(11),
            &|_, _| Err("no fetch".to_string()),
            &unread,
        );
        assert!(matches!(out, ProbeOutcome::Inconclusive(e) if e.contains("no fetch")));
        // An unreadable permission is Inconclusive, never Clear.
        let out = outside_pr_in(
            Some(dir.as_path()),
            &facts(12),
            &|_, _| Ok(admin.clone()),
            &down,
        );
        assert!(matches!(out, ProbeOutcome::Inconclusive(e) if e.contains("gh down")));
        // A URL with no repository is Inconclusive.
        let no_slug = PrFacts {
            number: 10,
            url: "not-a-pr-url".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            outside_pr_in(
                Some(dir.as_path()),
                &no_slug,
                &|_, _| Ok(Value::Null),
                &unread
            ),
            ProbeOutcome::Inconclusive(_)
        ));
    }
    fn decisions(rows: Vec<Value>) -> Vec<u8> {
        json!({"decisions": rows}).to_string().into_bytes()
    }

    fn row(decision: &str, authority: &str, id: &str) -> Value {
        json!({"decision": decision, "authority_source": authority, "decision_id": id})
    }

    fn cache_fixture(dir: &Path, pr: u64, body: Option<&Value>) -> PathBuf {
        let slug = "fnodotdev/fno";
        let path = cache_path(dir, slug, pr).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some(v) = body {
            std::fs::write(&path, v.to_string()).unwrap();
        }
        path
    }

    fn facts(pr: u64) -> PrFacts {
        PrFacts {
            number: pr,
            url: format!("https://github.com/fnodotdev/fno/pull/{pr}"),
            ..Default::default()
        }
    }

    #[test]
    fn the_pr_hold_row_reads_held_and_disarms_on_record() {
        // The read argv is subject-only: the lane filter is what hid the
        // specimen rows (D3376, D3379 are coord rows).
        assert_eq!(
            hold_read_args("o/r", 7),
            vec![
                "backlog".to_string(),
                "decisions".to_string(),
                "pr-hold:o/r#7".to_string(),
                "--json".to_string(),
            ]
        );
        assert_eq!(
            held(None),
            Err("the decisions read did not answer".to_string())
        );
        assert_eq!(
            held(Some(b"not json")),
            Err("malformed decisions payload".to_string())
        );
        assert_eq!(held(Some(&decisions(vec![]))), Ok(None));
        // Any authority at the subject holds: the specimens (D3376, D3379)
        // are coord-lane role rows. The subject is the identity.
        assert_eq!(
            held(Some(&decisions(vec![row(HOLD_DECISION, "agent", "d-1")]))),
            Ok(Some("d-1".to_string()))
        );
        assert_eq!(
            held(Some(&decisions(vec![row(
                "some other decision",
                "operator",
                "d-2"
            )]))),
            Ok(None)
        );
        assert_eq!(
            held(Some(&decisions(vec![row(
                HOLD_DECISION,
                "operator",
                "d-3"
            )]))),
            Ok(Some("d-3".to_string()))
        );

        // The specimen shapes: a role row whose text carries a rationale
        // after the hold phrase. The old exact-text, operator-only reader
        // read these as Clear, which is the bug.
        let specimen = |decision: &str| {
            decisions(vec![json!({
                "decision": decision,
                "authority_source": "role",
                "lane": "coord",
                "lifecycle": "live",
                "decision_id": "d-aa7a5d91",
            })])
        };
        assert_eq!(
            held(Some(&specimen(
                "do not merge this pull request: merging the 0.1.0 bump before a \
                 0.1.0 release lets fno doctor update build 0.1.0 from main"
            ))),
            Ok(Some("d-aa7a5d91".to_string()))
        );
        // Lifecycle table: unscoped (the store cannot prove the PR closed)
        // and unknown (the graph read failed) hold; expired, superseded and
        // retracted do not.
        let lifecycle_row = |lifecycle: &str| {
            decisions(vec![json!({
                "decision": HOLD_DECISION,
                "authority_source": "role",
                "lifecycle": lifecycle,
                "decision_id": "d-x",
            })])
        };
        assert_eq!(
            held(Some(&lifecycle_row("unscoped"))),
            Ok(Some("d-x".to_string()))
        );
        assert_eq!(
            held(Some(&lifecycle_row("unknown"))),
            Ok(Some("d-x".to_string()))
        );
        assert_eq!(held(Some(&lifecycle_row("expired"))), Ok(None));
        assert_eq!(held(Some(&lifecycle_row("superseded"))), Ok(None));
        assert_eq!(held(Some(&lifecycle_row("retracted"))), Ok(None));
        // A near spelling is not a hold.
        assert_eq!(
            held(Some(&decisions(vec![row(
                "do not merge this pull requests",
                "role",
                "d-near"
            )]))),
            Ok(None)
        );
        // A comma-joined rationale is still a hold.
        assert_eq!(
            held(Some(&specimen(
                "do not merge this pull request, the user owes the paint a look"
            ))),
            Ok(Some("d-aa7a5d91".to_string()))
        );

        // Recording a hold row at any authority disarms, drafts and
        // labels: five gh calls (the autoMergeRequest read, then the
        // disarm; the label is created first among the effects). A release
        // needs operator or role; any other decision or subject runs no
        // gh call.
        let seen = std::cell::RefCell::new(0usize);
        let gh = |args: &[&str]| -> Result<(bool, String), String> {
            *seen.borrow_mut() += 1;
            // The autoMergeRequest read answers armed for #7 (a real
            // auto-merge to disarm), unarmed for #12, and fails for #13;
            // every other pr view (the release path's headRefOid) refuses.
            if args.contains(&"view") && args.contains(&"autoMergeRequest") {
                if args.contains(&"13") {
                    return Ok((false, "gh down".to_string()));
                }
                if args.contains(&"14") {
                    // Armed, with a gh warning riding the combined output.
                    return Ok((
                        true,
                        "{\"autoMergeRequest\": {\"mergeMethod\": \"SQUASH\"}}\nwarning: slow\n"
                            .to_string(),
                    ));
                }
                if args.contains(&"12") {
                    return Ok((true, r#"{"autoMergeRequest": null}"#.to_string()));
                }
                return Ok((
                    true,
                    r#"{"autoMergeRequest": {"mergeMethod": "SQUASH"}}"#.to_string(),
                ));
            }
            if args
                .first()
                .is_some_and(|a| *a == "pr" && args.contains(&"view"))
            {
                return Ok((false, "no view in tests".to_string()));
            }
            Ok((true, String::new()))
        };
        let out = after_record_with(
            &gh,
            "pr-hold:fnodotdev/fno#7",
            HOLD_DECISION,
            Some("operator"),
        );
        assert_eq!(*seen.borrow(), 5);
        assert!(
            out.is_some_and(|s| s.contains("disabled an armed auto-merge")
                && s.contains("converted fnodotdev/fno#7 to draft")
                && s.contains("labeled fnodotdev/fno#7 pr-hold"))
        );
        // A role-authored hold with a rationale fires the same four calls,
        // and so does an agent-authored one (a lead's `fno inbox decide`).
        *seen.borrow_mut() = 0;
        let out = after_record_with(
            &gh,
            "pr-hold:fnodotdev/fno#8",
            "do not merge this pull request: waits on the user's look",
            Some("role"),
        );
        assert_eq!(*seen.borrow(), 5);
        assert!(out.is_some_and(|s| s.contains("converted fnodotdev/fno#8 to draft")));
        *seen.borrow_mut() = 0;
        let out = after_record_with(&gh, "pr-hold:o/r#8", HOLD_DECISION, Some("agent"));
        assert_eq!(*seen.borrow(), 5);
        assert!(out.is_some_and(|s| s.contains("converted o/r#8 to draft")));
        // The armed claim stays honest in the other two arms: an unarmed
        // PR (nothing was ever armed) keeps the disarm call but prints no
        // auto-merge line, and a read that cannot answer stays silent.
        // Both still disarm, draft and label, five calls each.
        *seen.borrow_mut() = 0;
        let out = after_record_with(&gh, "pr-hold:o/r#12", HOLD_DECISION, Some("operator"));
        assert_eq!(*seen.borrow(), 5);
        assert!(out
            .is_some_and(|s| s.contains("converted o/r#12 to draft") && !s.contains("auto-merge")));
        *seen.borrow_mut() = 0;
        let out = after_record_with(&gh, "pr-hold:o/r#13", HOLD_DECISION, Some("operator"));
        assert_eq!(*seen.borrow(), 5);
        assert!(out
            .is_some_and(|s| s.contains("converted o/r#13 to draft") && !s.contains("auto-merge")));
        // A gh warning appended to a successful read must not hide an armed
        // auto-merge: the payload is the brace span, warnings ride after it.
        *seen.borrow_mut() = 0;
        let out = after_record_with(&gh, "pr-hold:o/r#14", HOLD_DECISION, Some("operator"));
        assert_eq!(*seen.borrow(), 5);
        assert!(
            out.is_some_and(|s| s.contains("disabled an armed auto-merge")
                && s.contains("converted o/r#14 to draft"))
        );
        *seen.borrow_mut() = 0;
        // A release restores ready, drops the label; the head view is the
        // third call and its stub failure skips the ci-required rerun. A
        // reasoned release fires the same three.
        *seen.borrow_mut() = 0;
        let out = after_record_with(&gh, "pr-hold:o/r#9", "release", Some("operator"));
        assert_eq!(*seen.borrow(), 3);
        assert!(out.is_some_and(|s| s.contains("marked o/r#9 ready")));
        let out = after_record_with(
            &gh,
            "pr-hold:o/r#9",
            "release: the look landed",
            Some("operator"),
        );
        assert_eq!(*seen.borrow(), 6);
        assert!(out.is_some_and(|s| s.contains("marked o/r#9 ready")));
        assert!(after_record_with(&gh, "pr-hold:o/r#7", "release", Some("agent")).is_none());
        // A queued release runs no gh call: it releases only once recorded.
        assert!(after_queue_with(&gh, "pr-hold:o/r#9", "release", Some("operator")).is_none());
        assert!(after_record_with(
            &gh,
            "node-subject-that-is-not-a-hold",
            HOLD_DECISION,
            Some("operator")
        )
        .is_none());
        assert_eq!(*seen.borrow(), 6);
        // A gh failure still returns the manual lines.
        let out = after_record_with(
            &|_| Err("spawn failed".to_string()),
            "pr-hold:o/r#9",
            HOLD_DECISION,
            Some("operator"),
        );
        assert!(out
            .unwrap()
            .contains("gh pr merge 9 --repo o/r --disable-auto by hand"));
    }
}
