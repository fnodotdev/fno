//! PermissionRequest hook: answer a worker's EnterWorktree ask.
//!
//! Claude Code asks before EnterWorktree moves the session to a path outside
//! its own managed worktree dir in the repo ("permission-root relocation").
//! Only bypassPermissions skips that ask: no permission rule, allow rule or
//! add-dir grant suppresses it, so an auto-mode worker that launches from
//! the repo root stops at the prompt. A PermissionRequest hook is the one
//! programmatic answer short of bypassPermissions. The claim, not the
//! working directory, names the worktree: a sibling under the same base
//! keeps its ask.
//!
//! Three answers. Allow the session's own claimed worktree. Deny a target
//! that does not exist: nobody can enter it, so the ask has no legitimate
//! approval, and the deny message names the claimed worktree so a typo'd
//! worker retries instead of parking at a prompt it cannot answer.
//! Everything else prints `{}`, which leaves the prompt in place for the
//! human.
//!
//! The claim has three sources, tried in order. A spawned worker's registry
//! row names its node. A session that ran `fno do target init` holds a
//! target manifest instead, whose `harness_session_id` names the session;
//! an interactive session's enter is the case that used to prompt every
//! time. The third source is the claims store: a live `node:<id>` claim
//! whose holder names this session admits the entered
//! `<base>/<folder>/<id>`. Init claims as `target-session:<harness session
//! id>` on every run, so a redirected or respawned worker - whose registry
//! row names a stale node and whose manifest id the owned-identity
//! collision refused to `null` - still resolves its own tree, including one
//! the record round-trip cannot yet verify. A sibling, an outside dir, or a
//! session no source names keeps its ask.
//!
//! Paths are canonicalized before the compare, so a symlink cannot point the
//! session elsewhere.
//!
//! Routine file work is not answered here. When `user.auto_approve_routine`
//! is on, the spawn writes plain `permissions.allow` and `ask` rules into its
//! own settings file (`spawn/auto_approve.rs`), and Claude Code applies its
//! own deny, ask, allow order.

use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

/// The PermissionRequest fields this hook reads, parsed once. A payload that
/// does not parse reads as the default, which never allows.
#[derive(Deserialize, Default)]
struct Payload {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    tool_input: ToolInput,
}

#[derive(Deserialize, Default)]
struct ToolInput {
    #[serde(default)]
    path: String,
}

/// Entry: read one PermissionRequest payload, decide, print, always exit 0.
pub fn run(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let payload: Payload = serde_json::from_str(raw.trim()).unwrap_or_default();
    let cwd = payload_cwd(&payload);
    let own = own_worktree(&payload.session_id, &cwd)
        .or_else(|| claims_leg(&payload.session_id, &cwd, &payload.tool_input.path));
    let own_ref = own.as_ref().map(|(p, s)| (p.as_path(), *s));
    let Some((decision, reason)) = classify(&payload, own_ref) else {
        return super::emit_allow();
    };
    let reason = row_reason(&payload, decision, reason);
    super::emit_guard_row_reason(&cwd, GUARD, "EnterWorktree", decision, &reason);
    match decision {
        "allow" => {
            let out = json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "allow"}
                }
            });
            println!("{out}");
            0
        }
        "deny" => {
            // No prompt is left standing, so this leg skips the blocked
            // report: the worker reads the message and can retry.
            let out = json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "deny",
                        "message": deny_message(&payload, own_ref.map(|(p, _)| p))
                    }
                }
            });
            println!("{out}");
            0
        }
        _ => {
            // The prompt stays up, so this leg reports it as blocked: the
            // generic claude PermissionRequest report skips EnterWorktree.
            let footnote = std::env::var("FNO_HARNESS_NAME").is_ok_and(|h| h == "footnote");
            let whole = serde_json::from_str::<serde_json::Value>(raw.trim());
            if let (false, "EnterWorktree", Ok(whole)) =
                (footnote, payload.tool_name.as_str(), whole)
            {
                super::session_state::process("claude", "PermissionRequest", &whole);
            }
            super::emit_allow()
        }
    }
}

const GUARD: &str = "worktree-relocation-allow";

/// The worktree this session claims, from the first source that resolves:
/// the registry row a spawn wrote, then the target manifest init wrote for
/// the session. The tag names the source on the allow row, so a later
/// reader tells a spawn-lane allow from a manifest-lane one.
fn own_worktree(session: &str, cwd: &Path) -> Option<(PathBuf, &'static str)> {
    if session.is_empty() {
        return None;
    }
    registry_claim(session, cwd)
        .map(|p| (p, "own_worktree"))
        .or_else(|| manifest_claim(session, cwd).map(|p| (p, "own_manifest")))
}

/// The claims-store leg: the live `node:<id>` claim whose holder names this
/// session admits the entered `<base>/<folder>/<id>`. The entered path names
/// the node; the pid-probed claim only authorizes it. No record round-trip
/// here on purpose: a killed `worktree add` leaves a dir no record can
/// verify, and that construction site is exactly what a re-run worker must
/// enter. A sibling, an outside dir, or another session's claim keeps its
/// ask.
fn claims_leg(session: &str, cwd: &Path, raw: &str) -> Option<(PathBuf, &'static str)> {
    claims_leg_in(session, cwd, raw, None)
}

/// [`claims_leg`] with the claims root as a parameter, so the tests seed a
/// private store instead of the ambient one.
fn claims_leg_in(
    session: &str,
    cwd: &Path,
    raw: &str,
    root: Option<&Path>,
) -> Option<(PathBuf, &'static str)> {
    if session.is_empty() || raw.is_empty() {
        return None;
    }
    let real = cwd.join(raw).canonicalize().ok()?;
    let (candidate, node) = candidate_under(&bases(cwd), &real)?;
    let key = format!("node:{node}");
    let (state, record) = crate::claims::status(&key, root);
    if !matches!(
        state,
        crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
    ) {
        return None;
    }
    let holder = record?.holder;
    (holder == format!("target-session:{session}")).then_some((candidate, "own_claim"))
}

/// The ask row names the session and the target, so the next parked worker is
/// one events query to diagnose instead of one transcript crawl. Allow and
/// deny rows keep their bare tags: the deny already names the path in its
/// worker-facing message, and the allow tags are the source diagnosis.
fn row_reason(payload: &Payload, decision: &str, reason: &str) -> String {
    if decision != "ask" {
        return reason.to_string();
    }
    let target = path_call(payload).unwrap_or_default();
    format!(
        "{reason} session={} target={}",
        payload.session_id.chars().take(12).collect::<String>(),
        target.chars().take(120).collect::<String>()
    )
}

/// The registry-row claim: `<base>/<repo>/<node>` for the node the spawn
/// wrote. No row, no node or no such folder means no claim here.
fn registry_claim(session: &str, cwd: &Path) -> Option<PathBuf> {
    let home = crate::paths::AgentsHome::from_env_opt()?;
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    let node = claimed_node(&registry, session)?;
    own_worktree_in(&bases(cwd), &crate::paths::canonical_repo_root(cwd)?, &node)
}

/// The target-manifest claim: the spaces manifest whose
/// `harness_session_id` names this session. Init wrote it, so the session
/// id match is the binding; the record round-trip and the owner_cwd
/// equality still decide the path, the same as the registry leg.
fn manifest_claim(session: &str, cwd: &Path) -> Option<PathBuf> {
    let dir = crate::paths::space_dir_opt(cwd)?.join("worktrees");
    let bases = bases(cwd);
    let repo = crate::paths::canonical_repo_root(cwd)?;
    manifest_claim_in(session, &dir, &bases, &repo)
}

/// [`manifest_claim`] with the directories as parameters, so the tests
/// exercise the predicate with no spaces root in the loop.
fn manifest_claim_in(session: &str, dir: &Path, bases: &[PathBuf], repo: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(content) = std::fs::read_to_string(entry.path().join("target-state.md")) else {
            continue;
        };
        if super::stop::first_raw_field(&content, &["harness_session_id"]).as_deref()
            != Some(session)
        {
            continue;
        }
        let node = super::stop::first_raw_field(&content, &["graph_node_id"]);
        let real = node
            .filter(|node| node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .and_then(|node| own_worktree_in(bases, repo, &node));
        if let Some(real) = real {
            let owner = super::stop::first_raw_field(&content, &["owner_cwd"])
                .and_then(|owner| PathBuf::from(owner).canonicalize().ok());
            if owner.is_some_and(|owner| owner == real) {
                return Some(real);
            }
        }
    }
    None
}

fn claimed_node(registry: &crate::state::Registry, session: &str) -> Option<String> {
    registry
        .entries
        .iter()
        .find(|e| {
            e.harness_session_id.as_deref() == Some(session)
                || e.session_id.as_deref() == Some(session)
        })
        .and_then(|e| e.node.clone())
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// The checkout `<base>/<any folder>/<node>` that belongs to this repo. Its
/// `.git` file must name a record under the repo's worktree records, and that
/// record's `gitdir` must point back at the checkout's own `.git` file. A
/// planted folder can name a record, but it cannot make the record name it.
fn own_worktree_in(bases: &[PathBuf], repo_root: &Path, node: &str) -> Option<PathBuf> {
    let records = repo_root.canonicalize().ok()?.join(".git/worktrees");
    bases
        .iter()
        .filter_map(|base| std::fs::read_dir(base).ok())
        .flatten()
        .flatten()
        .find_map(|folder| {
            let real = folder.path().join(node).canonicalize().ok()?;
            let pointer = std::fs::read_to_string(real.join(".git")).ok()?;
            let record = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
            let record = record.canonicalize().ok()?;
            if record.parent()? != records {
                return None;
            }
            let back = std::fs::read_to_string(record.join("gitdir")).ok()?;
            let back = PathBuf::from(back.trim()).canonicalize().ok()?;
            (back == real.join(".git").canonicalize().ok()?).then_some(real)
        })
}

/// `<base>/<repo>/<node>` for the base `real` (a canonical cwd) sits under,
/// with the node's name.
fn candidate_under(bases: &[PathBuf], real: &Path) -> Option<(PathBuf, String)> {
    bases.iter().find_map(|base| {
        let base = base.canonicalize().ok()?;
        let rest = real.strip_prefix(&base).ok()?;
        let mut parts = rest.components().take(2);
        let (repo, node) = (parts.next()?, parts.next()?);
        let name = node.as_os_str().to_str()?.to_string();
        Some((base.join(repo).join(node), name))
    })
}

/// A spawn that names a node must run in that node's own folder.
fn names_this_node(named: Option<&str>, folder: &str) -> bool {
    match named.filter(|n| !n.is_empty()) {
        Some(n) => n == folder,
        None => true,
    }
}

/// The worktree a spawn at `cwd` runs in, when it is a recorded worktree of
/// this repo. The registry row does not exist yet when a spawn writes its
/// settings, so the repo's worktree records stand in for the claim. `named`
/// is the node the spawn payload names for the child, never the spawner's
/// own `FNO_NODE`, which a lead passes down to every worker it starts.
pub(crate) fn verified_worktree(cwd: &Path, named: Option<&str>) -> Option<PathBuf> {
    verified_in(
        &bases(cwd),
        &crate::paths::canonical_repo_root(cwd)?,
        &cwd.canonicalize().ok()?,
        named,
    )
}

fn verified_in(all: &[PathBuf], repo: &Path, real: &Path, named: Option<&str>) -> Option<PathBuf> {
    let (candidate, node) = candidate_under(all, real)?;
    if !names_this_node(named, &node) {
        return None;
    }
    let found = own_worktree_in(all, repo, &node)?;
    (found == candidate).then_some(found)
}

/// The `path` of an EnterWorktree call, the only shape that relocates. The
/// create form (`name`) stays inside the repo and never prompts.
fn path_call(payload: &Payload) -> Option<&str> {
    (payload.tool_name == "EnterWorktree" && !payload.tool_input.path.is_empty())
        .then_some(payload.tool_input.path.as_str())
}

/// The decision a path call earns, with the reason the row carries: `allow`
/// for the session's own claimed worktree, `deny` for a target that does not
/// exist, `ask` with a reason for every prompt that stays. Claude Code logs
/// only a hook's decisions, so without the row a parked worker looks the
/// same as one whose hook never ran, and without the reason the residual
/// asks cannot be classified after the fact.
fn classify(
    payload: &Payload,
    own: Option<(&Path, &'static str)>,
) -> Option<(&'static str, &'static str)> {
    let raw = path_call(payload)?;
    if let Some((own, source)) = own {
        if allows(payload, own) {
            return Some(("allow", source));
        }
    }
    let reason = if payload_cwd(payload).join(raw).canonicalize().is_err() {
        "nonexistent"
    } else if own.is_none() {
        "own_unresolved"
    } else {
        "not_own"
    };
    let decision = if reason == "nonexistent" {
        "deny"
    } else {
        "ask"
    };
    Some((decision, reason))
}

/// Why the deny names, so a typo'd worker retries at the right path instead
/// of parking: the claimed worktree when it resolves, the unresolved claim
/// otherwise.
fn deny_message(payload: &Payload, own: Option<&Path>) -> String {
    let raw = path_call(payload).unwrap_or_default();
    match own {
        Some(own) => format!(
            "EnterWorktree target {raw} does not exist; this session's claimed worktree is {}",
            own.display()
        ),
        None => format!(
            "EnterWorktree target {raw} does not exist, and no claimed worktree resolves for this session"
        ),
    }
}

/// Every base fno has written checkouts to: the configured (or default) base,
/// plus both state-root defaults. An override moves new checkouts, yet
/// worktrees made before it, or by a peer repo with another config, still sit
/// under a default base and must not stop at the prompt.
pub(crate) fn bases(cwd: &Path) -> Vec<PathBuf> {
    let state = crate::paths_cli::state_root_for_worktrees(cwd);
    bases_for(crate::paths_cli::worktrees_base(cwd), &state)
}

fn bases_for(configured: PathBuf, state: &Path) -> Vec<PathBuf> {
    vec![
        configured,
        crate::paths_cli::default_worktrees_base(state),
        crate::paths_cli::legacy_worktrees_base(state),
    ]
}

fn payload_cwd(payload: &Payload) -> PathBuf {
    Some(payload.cwd.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// True when the payload is an EnterWorktree `path` call whose real target
/// is the worker's own claimed worktree. Separated from `run` so the tests
/// exercise the same predicate with no stdin or config in the loop.
fn allows(payload: &Payload, own: &Path) -> bool {
    let Some(raw) = path_call(payload) else {
        return false;
    };
    let target = payload_cwd(payload).join(raw);
    let (Ok(target), Ok(own)) = (target.canonicalize(), own.canonicalize()) else {
        return false;
    };
    target == own
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(cwd: &Path, path: &str) -> Payload {
        serde_json::from_value(json!({
            "hook_event_name": "PermissionRequest",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "EnterWorktree",
            "tool_input": {"path": path}
        }))
        .unwrap()
    }

    #[test]
    fn allows_only_the_workers_own_claimed_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("worktrees");
        let wt = base.join("repo").join("node");
        let sibling = base.join("repo").join("other");
        let outside = tmp.path().join("elsewhere");
        for dir in [&wt, &sibling, &outside, &tmp.path().join("repo")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let repo = tmp.path().join("repo");

        assert!(allows(&call(&repo, &wt.to_string_lossy()), &wt));
        // Relative paths resolve against the payload cwd.
        assert!(allows(&call(&base, "repo/node"), &wt));
        // A sibling under the same base, the base, an outside dir, a missing
        // dir: the prompt stays.
        assert!(!allows(&call(&repo, &sibling.to_string_lossy()), &wt));
        assert!(!allows(&call(&repo, &base.to_string_lossy()), &wt));
        assert!(!allows(&call(&repo, &outside.to_string_lossy()), &wt));
        assert!(!allows(
            &call(&repo, &base.join("gone").to_string_lossy()),
            &wt
        ));
        // A `..` escape canonicalizes out of the worktree.
        let escape = format!("{}/repo/node/../other", base.display());
        assert!(!allows(&call(&repo, &escape), &wt));
        // Another tool, or the create form with no path, never allows.
        let mut other = call(&repo, &wt.to_string_lossy());
        other.tool_name = "Bash".into();
        assert!(!allows(&other, &wt));
        let create: Payload = serde_json::from_value(
            json!({"tool_name": "EnterWorktree", "tool_input": {"name": "x"}}),
        )
        .unwrap();
        assert!(!allows(&create, &wt));

        // Every path answer leaves a row with a reason, so a stalled worker
        // tells a declined path (ask) from a hook that never answered (no
        // row), and a later reader classifies the residual asks.
        let inside = call(&repo, &wt.to_string_lossy());
        let declined = call(&repo, &outside.to_string_lossy());
        let gone = call(&repo, &base.join("gone").to_string_lossy());
        assert_eq!(
            classify(&inside, Some((wt.as_path(), "own_worktree"))),
            Some(("allow", "own_worktree"))
        );
        // The manifest source answers allow with its own tag, so a later
        // reader tells which claim source resolved.
        assert_eq!(
            classify(&inside, Some((wt.as_path(), "own_manifest"))),
            Some(("allow", "own_manifest"))
        );
        assert_eq!(
            classify(&inside, Some((wt.as_path(), "own_claim"))),
            Some(("allow", "own_claim"))
        );
        assert_eq!(
            classify(&declined, Some((wt.as_path(), "own_manifest"))),
            Some(("ask", "not_own"))
        );
        assert_eq!(classify(&declined, None), Some(("ask", "own_unresolved")));
        assert_eq!(
            classify(&gone, Some((wt.as_path(), "own_worktree"))),
            Some(("deny", "nonexistent"))
        );
        assert_eq!(classify(&gone, None), Some(("deny", "nonexistent")));
        assert_eq!(classify(&other, Some((wt.as_path(), "own_worktree"))), None);
        assert_eq!(
            classify(&create, Some((wt.as_path(), "own_worktree"))),
            None
        );
    }

    #[test]
    fn the_deny_message_names_the_claimed_worktree() {
        let repo = Path::new("/repo");
        let wt = Path::new("/base/repo/node");
        let gone = call(repo, "/base/repo/gone");
        assert_eq!(
            deny_message(&gone, Some(wt)),
            "EnterWorktree target /base/repo/gone does not exist; this session's claimed worktree is /base/repo/node"
        );
        let unresolved = deny_message(&gone, None);
        assert!(unresolved.contains("no claimed worktree resolves"));
        assert!(unresolved.contains("does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_under_the_base_pointing_out_is_not_the_claimed_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("worktrees/repo/node");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = tmp.path().join("worktrees/link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(!allows(&call(tmp.path(), &link.to_string_lossy()), &wt));
    }

    /// One checkout under `base/<folder>/<node>` whose record sits in
    /// `repo/.git/worktrees/<node>`, with the record's back-pointer set by
    /// `back`.
    fn checkout(base: &Path, repo: &Path, folder: &str, node: &str, back: bool) -> PathBuf {
        let record = repo.join(".git/worktrees").join(node);
        std::fs::create_dir_all(&record).unwrap();
        let wt = base.join(folder).join(node);
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", record.display())).unwrap();
        if back {
            let git = wt.canonicalize().unwrap().join(".git");
            std::fs::write(record.join("gitdir"), format!("{}\n", git.display())).unwrap();
        }
        wt
    }

    #[test]
    fn the_claim_names_the_worktree_by_the_repo_and_its_back_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        let state = tmp.path().join("state");
        let base = crate::paths_cli::legacy_worktrees_base(&state);
        // The folder name differs from the repo's name on purpose.
        let wt = checkout(&base, &repo, "renamed", "node", true);
        // A planted folder names a real record, but the record does not name it.
        let planted = base.join("renamed/planted");
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::write(
            planted.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/node").display()),
        )
        .unwrap();
        // A checkout of a different repo.
        let foreign = base.join("renamed/foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(
            foreign.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/foreign\n",
        )
        .unwrap();
        let unrecorded = checkout(&base, &repo, "renamed", "unrecorded", false);
        let configured = tmp.path().join("custom");
        std::fs::create_dir_all(&configured).unwrap();
        let all = bases_for(configured, &state);
        assert_eq!(
            own_worktree_in(&all, &repo, "node"),
            Some(wt.canonicalize().unwrap())
        );
        // A spawn's cwd names a worktree only through the same record check.
        let real = wt.canonicalize().unwrap();
        assert_eq!(
            candidate_under(&all, &real.join("src")),
            Some((real.clone(), "node".to_string()))
        );
        assert_eq!(candidate_under(&all, tmp.path()), None);
        assert!(names_this_node(None, "node") && names_this_node(Some(""), "node"));
        assert!(names_this_node(Some("node"), "node"));
        assert!(!names_this_node(Some("other"), "node"));
        // The child's node comes from the spawn payload: a worker spawned into
        // `node` gets its folder whatever the spawner's own env names.
        assert_eq!(
            verified_in(&all, &repo, &real, Some("node")),
            Some(real.clone())
        );
        assert_eq!(verified_in(&all, &repo, &real, None), Some(real.clone()));
        assert_eq!(verified_in(&all, &repo, &real, Some("other")), None);
        assert_eq!(own_worktree_in(&all, &repo, "planted"), None);
        assert_eq!(own_worktree_in(&all, &repo, "foreign"), None);
        assert_eq!(own_worktree_in(&all, &repo, "unrecorded"), None);
        assert!(unrecorded.exists());
    }

    /// One manifest in a spaces worktrees dir, with the fields the claim
    /// reads. The real manifest carries frontmatter and a body; the reader
    /// scans lines, so a flat body stands in for both.
    fn manifest(dir: &Path, name: &str, session: &str, owner: &str, node: &str) {
        let wt_dir = dir.join(name);
        std::fs::create_dir_all(&wt_dir).unwrap();
        std::fs::write(
            wt_dir.join("target-state.md"),
            format!(
                "harness_session_id: {session}\nowner_cwd: \"{owner}\"\ngraph_node_id: {node}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn the_session_manifest_claims_its_worktree_through_the_records() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        let state = tmp.path().join("state");
        let base = crate::paths_cli::legacy_worktrees_base(&state);
        let wt = checkout(&base, &repo, "renamed", "node", true);
        let real = wt.canonicalize().unwrap();
        let all = vec![base.clone()];

        // Admit arm: the session's own manifest naming the recorded
        // worktree in owner_cwd resolves the claim.
        let spaces = tmp.path().join("spaces-a");
        manifest(&spaces, "x-node", "s-a", &real.to_string_lossy(), "node");
        assert_eq!(
            manifest_claim_in("s-a", &spaces, &all, &repo),
            Some(real.clone())
        );

        // Withhold arm, foreign session: another caller with no manifest of
        // its own is not admitted by one naming a different session.
        assert_eq!(manifest_claim_in("s-b", &spaces, &all, &repo), None);

        // Withhold arm, planted owner_cwd: the node resolves to the real
        // worktree, but owner_cwd names an unrecorded existing dir, so the
        // equality check refuses.
        let planted = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&planted).unwrap();
        let spaces = tmp.path().join("spaces-c");
        manifest(&spaces, "x-c", "s-c", &planted.to_string_lossy(), "node");
        assert_eq!(manifest_claim_in("s-c", &spaces, &all, &repo), None);

        // Withhold arm, unrecorded node: the record round-trip fails, so an
        // existing but unrecorded checkout admits nothing.
        let unrecorded = checkout(&base, &repo, "renamed", "unrecorded2", false);
        let spaces = tmp.path().join("spaces-d");
        manifest(
            &spaces,
            "x-d",
            "s-d",
            &unrecorded.canonicalize().unwrap().to_string_lossy(),
            "unrecorded2",
        );
        assert_eq!(manifest_claim_in("s-d", &spaces, &all, &repo), None);

        // Withhold arm, charset: a graph_node_id with a path separator is
        // skipped before any record probe.
        let spaces = tmp.path().join("spaces-e");
        manifest(
            &spaces,
            "x-e",
            "s-e",
            &real.to_string_lossy(),
            "node/../../escape",
        );
        assert_eq!(manifest_claim_in("s-e", &spaces, &all, &repo), None);

        // Withhold arm, no manifest at all.
        let spaces = tmp.path().join("spaces-f");
        std::fs::create_dir_all(&spaces).unwrap();
        assert_eq!(manifest_claim_in("s-a", &spaces, &all, &repo), None);
    }

    /// One claim in a private claims root, held by `holder` for `ttl_ms`
    /// (negative reads as already expired). The pid is this test process, so
    /// a live verdict is deterministic. Claims live in the store the read
    /// path opens, so the seed goes through the store's own acquire, never a
    /// hand-written lockfile.
    fn seed_claim(root: &Path, node: &str, holder: &str, ttl_ms: i64) {
        let opts = crate::claims::AcquireOpts {
            pid: Some(std::process::id()),
            pid_unavailable: false,
            ttl_ms: Some(ttl_ms),
            reason: Some("test window".into()),
            root: Some(root.to_path_buf()),
            ..Default::default()
        };
        assert!(matches!(
            crate::claims::acquire(&format!("node:{node}"), holder, opts),
            crate::claims::AcquireOutcome::Acquired(_)
        ));
    }

    #[test]
    fn the_live_claim_admits_the_entered_tree_for_its_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        let state = tmp.path().join("state");
        let base = crate::paths_cli::legacy_worktrees_base(&state);
        let wt = checkout(&base, &repo, "renamed", "x-claim", true);
        let real = wt.canonicalize().unwrap();
        let root = tmp.path().join("claims-root");

        // Admit arm: the holder's own live claim admits the recorded tree.
        seed_claim(&root, "x-claim", "target-session:s-claim", 900_000);
        assert_eq!(
            claims_leg_in("s-claim", &repo, &wt.to_string_lossy(), Some(&root)),
            Some((real, "own_claim"))
        );

        // The half-laid arm is the point: no .git file, no record, a bare
        // dir a killed worktree add left behind - the claim still admits it.
        let half = base.join("renamed").join("x-half");
        std::fs::create_dir_all(&half).unwrap();
        seed_claim(&root, "x-half", "target-session:s-claim", 900_000);
        assert_eq!(
            claims_leg_in("s-claim", &repo, &half.to_string_lossy(), Some(&root)),
            Some((half.canonicalize().unwrap(), "own_claim"))
        );

        // Withhold arm, another session's claim: the holder must name the
        // asking session.
        seed_claim(&root, "x-claim", "target-session:s-other", 900_000);
        assert_eq!(
            claims_leg_in("s-claim", &repo, &wt.to_string_lossy(), Some(&root)),
            None
        );

        // Withhold arm, expired claim: a stale TTL admits nobody.
        seed_claim(&root, "x-claim", "target-session:s-claim", -1);
        assert_eq!(
            claims_leg_in("s-claim", &repo, &wt.to_string_lossy(), Some(&root)),
            None
        );

        // Withhold arm, no claim at all.
        assert_eq!(
            claims_leg_in("s-unclaimed", &repo, &wt.to_string_lossy(), Some(&root)),
            None
        );

        // Withhold arms, empty session or empty path: never admit.
        assert_eq!(
            claims_leg_in("", &repo, &wt.to_string_lossy(), Some(&root)),
            None
        );
        assert_eq!(claims_leg_in("s-claim", &repo, "", Some(&root)), None);

        // Withhold arm, a path outside any base: no candidate, no claim ask.
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        seed_claim(&root, "x-claim", "target-session:s-claim", 900_000);
        assert_eq!(
            claims_leg_in("s-claim", &repo, &outside.to_string_lossy(), Some(&root)),
            None
        );
    }
}
