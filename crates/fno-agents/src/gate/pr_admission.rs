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

/// Ok(Some(decision_id)) when any live operator row at the subject carries
/// HOLD_DECISION. `None` stdout (the read failed) and a malformed payload
/// are Err, never absence: the hold probe builds on this and must fail
/// closed.
pub fn held(decisions_stdout: Option<&[u8]>) -> Result<Option<String>, String> {
    let Some(bytes) = decisions_stdout else {
        return Err("the decisions read did not answer".to_string());
    };
    let Some(rows) = crate::loopcheck::coverage_status::operator_law_rows(bytes) else {
        return Err("malformed decisions payload".to_string());
    };
    for row in rows {
        if row.get("decision").and_then(|d| d.as_str()) == Some(HOLD_DECISION) {
            return Ok(Some(
                row.get("decision_id")
                    .and_then(|d| d.as_str())
                    .unwrap_or("unreadable id")
                    .to_string(),
            ));
        }
    }
    Ok(None)
}

/// The per-PR origin-facts cache directory: `pr_status_cache_dir` with its
/// last segment swapped for `pr-origin`, so `<state>/cache/pr-origin`.
fn pr_origin_cache_dir(cwd: &Path) -> Option<PathBuf> {
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
    let path = cache_path(dir, slug, pr)?;
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

pub(crate) fn pr_hold(cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
    let Some(slug) = crate::merge_grant::repo_slug_from_pr_url(&facts.url) else {
        return ProbeOutcome::Inconclusive("pr_hold: the PR URL named no repository".to_string());
    };
    // The cached in-process read answers the exact argv the CLI would take
    // in process; the spawn stays for an argv shape the cache declines.
    let args: Vec<String> = [
        "backlog",
        "decisions",
        hold_subject(&slug, facts.number).as_str(),
        "--lane",
        "law",
        "--state",
        "live",
        "--json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
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

/// Called by the decide door after a ruling is recorded. Disarms at once for
/// an operator pr-hold row: a queue armed before the hold would otherwise
/// merge server-side with no re-check. Never fails the record; the ruling is
/// already durable. Every other subject or authority returns None with no
/// process spawned.
pub(crate) fn after_record(
    subject: &str,
    decision: &str,
    authority: Option<&str>,
) -> Option<String> {
    after_record_with(&real_gh, subject, decision, authority)
}

pub(crate) fn after_record_with(
    gh: GhRun,
    subject: &str,
    decision: &str,
    authority: Option<&str>,
) -> Option<String> {
    if authority != Some("operator") || decision != HOLD_DECISION {
        return None;
    }
    let rest = subject.strip_prefix(HOLD_SUBJECT)?.strip_prefix(':')?;
    let (slug, pr) = rest.split_once('#')?;
    let pr: u64 = pr.parse().ok()?;
    if slug.is_empty() {
        return None;
    }
    let args = [
        "pr",
        "merge",
        &pr.to_string(),
        "--repo",
        slug,
        "--disable-auto",
    ];
    match gh(&args) {
        Ok((true, _)) => Some(format!(
            "pr-hold: disabled an armed auto-merge on {slug}#{pr}"
        )),
        Ok((false, output)) => {
            let last = output.lines().last().unwrap_or("").trim().to_string();
            Some(format!(
                "pr-hold: {last}; run gh pr merge {pr} --repo {slug} --disable-auto by hand \
                 if auth or network failed"
            ))
        }
        Err(e) => Some(format!(
            "pr-hold: {e}; run gh pr merge {pr} --repo {slug} --disable-auto by hand \
             if auth or network failed"
        )),
    }
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
        assert_eq!(
            held(None),
            Err("the decisions read did not answer".to_string())
        );
        assert_eq!(
            held(Some(b"not json")),
            Err("malformed decisions payload".to_string())
        );
        assert_eq!(held(Some(&decisions(vec![]))), Ok(None));
        assert_eq!(
            held(Some(&decisions(vec![row(HOLD_DECISION, "agent", "d-1")]))),
            Ok(None)
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

        // Recording an operator pr-hold row disarms exactly once; every
        // other authority, decision or subject runs no gh call.
        let seen = std::cell::RefCell::new(0usize);
        let gh = |_: &[&str]| -> Result<(bool, String), String> {
            *seen.borrow_mut() += 1;
            Ok((true, String::new()))
        };
        let out = after_record_with(
            &gh,
            "pr-hold:fnodotdev/fno#7",
            HOLD_DECISION,
            Some("operator"),
        );
        assert_eq!(*seen.borrow(), 1);
        assert_eq!(
            out,
            Some("pr-hold: disabled an armed auto-merge on fnodotdev/fno#7".to_string())
        );
        assert!(after_record_with(&gh, "pr-hold:o/r#7", HOLD_DECISION, Some("role")).is_none());
        assert!(after_record_with(&gh, "pr-hold:o/r#7", "release", Some("operator")).is_none());
        assert!(after_record_with(
            &gh,
            "node-subject-that-is-not-a-hold",
            HOLD_DECISION,
            Some("operator")
        )
        .is_none());
        assert_eq!(*seen.borrow(), 1);
        // A gh failure still returns the manual-disarm line.
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
