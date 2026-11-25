//! PermissionRequest hook: allow a worker's EnterWorktree into its own
//! claimed worktree, and its routine file work in its own folders.
//!
//! The file-work rule ships behind `user.auto_approve_routine`, read from the
//! global config only and off by default. Off, file work keeps its prompt and
//! the own-worktree entry stays allowed.
//!
//! The hook cannot read why a prompt appeared. So it never allows a call that
//! a settings `ask` rule covers, and it never answers an attended session,
//! where fno's guards raise their own `ask` for a person to read.
//!
//! Claude Code asks before EnterWorktree moves the session to a path outside
//! its own managed worktree dir in the repo ("permission-root relocation").
//! The ask is a safety check the auto-mode classifier may not approve, and an allow rule
//! is never read for it, so an auto-mode worker that launches from the repo
//! root stops at the prompt. A PermissionRequest hook is the one answer short
//! of bypassPermissions. The claim, not the working directory, names the
//! worktree: a sibling under the same base keeps its ask.
//!
//! Paths are canonicalized before the compare, so a symlink cannot point the
//! session elsewhere. Anything else prints `{}`, which leaves the prompt in
//! place: this hook never denies.

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
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    notebook_path: String,
}

/// Entry: read one PermissionRequest payload, decide, print, always exit 0.
pub fn run(_args: &[String]) -> i32 {
    let payload: Payload = serde_json::from_str(super::read_stdin().trim()).unwrap_or_default();
    let cwd = payload_cwd(&payload);
    let rules = ask_rules(&cwd);
    let own = own_worktree(&payload.session_id, &cwd);
    let allowed = own.as_deref().is_some_and(|own| allows(&payload, own))
        && !ask_rule_hits(&rules, "EnterWorktree", None, &cwd);
    let file_ok =
        enabled() && !attended() && file_work_allowed(&payload, &cwd, own.as_deref(), &rules);
    if file_ok {
        super::emit_guard_row(&cwd, GUARD, &payload.tool_name, "allow");
    }
    if let Some(decision) = row_decision(&payload, allowed) {
        super::emit_guard_row(&cwd, GUARD, "EnterWorktree", decision);
    }
    if allowed || file_ok {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {"behavior": "allow"}
            }
        });
        println!("{out}");
        0
    } else {
        super::emit_allow()
    }
}

const GUARD: &str = "worktree-relocation-allow";

/// True while the harness marks the session as one a person watches.
fn attended() -> bool {
    std::env::var("CLAUDE_CODE_SESSION_ATTENDED").is_ok_and(|v| v == "1")
}

#[derive(Deserialize, Default)]
struct SettingsFile {
    #[serde(default)]
    permissions: SettingsPermissions,
}

#[derive(Deserialize, Default)]
struct SettingsPermissions {
    #[serde(default)]
    ask: Vec<String>,
}

/// Every `permissions.ask` rule the harness reads for this session. A settings
/// file that exists and does not parse reads as one rule that covers all.
fn ask_rules(cwd: &Path) -> Vec<String> {
    let mut files: Vec<PathBuf> = vec![
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json"),
        PathBuf::from("/etc/claude-code/managed-settings.json"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        files.push(PathBuf::from(home).join(".claude/settings.json"));
    }
    let mut roots = vec![cwd.to_path_buf()];
    roots.extend(crate::paths::canonical_repo_root(cwd));
    for root in roots {
        files.push(root.join(".claude/settings.json"));
        files.push(root.join(".claude/settings.local.json"));
    }
    let mut rules = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        match serde_json::from_str::<SettingsFile>(&text) {
            Ok(parsed) => rules.extend(parsed.permissions.ask),
            Err(_) => rules.push("*".to_string()),
        }
    }
    rules
}

/// True when an ask rule covers this call. A rule with a path pattern is
/// matched against the real path; a call with no path, or a rule shape this
/// cannot read, counts as covered.
fn ask_rule_hits(rules: &[String], tool: &str, path: Option<&Path>, cwd: &Path) -> bool {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    rules.iter().any(|rule| {
        let (name, spec) = match rule.split_once('(') {
            Some((name, rest)) => (name.trim(), Some(rest.trim_end().trim_end_matches(')'))),
            None => (rule.trim(), None),
        };
        let covers = name == "*"
            || name == tool
            || (name == "Edit" && matches!(tool, "Write" | "MultiEdit" | "NotebookEdit"));
        if !covers {
            return false;
        }
        match (spec, path) {
            (Some(spec), Some(path)) => spec_hits(spec, path, &home, cwd),
            _ => true,
        }
    })
}

fn spec_hits(spec: &str, path: &Path, home: &Path, cwd: &Path) -> bool {
    let spec = spec.trim();
    let mut patterns: Vec<String> = Vec::new();
    if let Some(rest) = spec.strip_prefix("~/") {
        patterns.push(home.join(rest).to_string_lossy().into_owned());
    } else if let Some(rest) = spec.strip_prefix("//") {
        patterns.push(format!("/{rest}"));
    } else if let Some(rest) = spec.strip_prefix('/') {
        patterns.push(format!("/{rest}"));
        patterns.push(cwd.join(rest).to_string_lossy().into_owned());
    } else {
        patterns.push(cwd.join(spec).to_string_lossy().into_owned());
        patterns.push(format!("**/{spec}"));
    }
    let text = path.to_string_lossy();
    patterns.iter().any(|pat| {
        glob_hit(pat.as_bytes(), text.as_bytes())
            || glob_hit(
                format!("{}/**", pat.trim_end_matches('/')).as_bytes(),
                text.as_bytes(),
            )
    })
}

fn glob_hit(pat: &[u8], text: &[u8]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) if rest.first() == Some(&b'*') => {
            let rest = rest.get(1..).unwrap_or_default();
            (0..=text.len()).any(|i| glob_hit(rest, text.get(i..).unwrap_or_default()))
        }
        Some((b'*', rest)) => {
            for i in 0..=text.len() {
                if glob_hit(rest, text.get(i..).unwrap_or_default()) {
                    return true;
                }
                if text.get(i) == Some(&b'/') || i == text.len() {
                    break;
                }
            }
            false
        }
        Some((c, rest)) => {
            text.first() == Some(c) && glob_hit(rest, text.get(1..).unwrap_or_default())
        }
    }
}

/// The one setting. Only the global config counts, so a project file cannot
/// turn the hook on for itself.
fn enabled() -> bool {
    switch_on(
        crate::agents_config::config_lookup_global(&["user", "auto_approve_routine"]).as_ref(),
    )
}

/// Only a boolean `true` turns the rule on. A missing, unreadable or
/// mistyped value reads as off.
fn switch_on(value: Option<&toml::Value>) -> bool {
    value.and_then(toml::Value::as_bool).unwrap_or(false)
}

/// The file a routine tool call touches. Reads and edits inside the worker's
/// own folders never need a person: the user ruled that routine work raises
/// no prompt. Everything else keeps its prompt.
fn file_target(payload: &Payload) -> Option<&str> {
    let input = &payload.tool_input;
    let raw = match payload.tool_name.as_str() {
        "Write" | "Edit" | "MultiEdit" | "Read" => input.file_path.as_str(),
        "NotebookEdit" => input.notebook_path.as_str(),
        _ => return None,
    };
    (!raw.is_empty()).then_some(raw)
}

/// Names a routine call never clears, compared without case. A prompt stays
/// for each.
fn blocked_name(path: &Path) -> bool {
    const DIRS: [&str; 5] = [".git", ".claude", ".ssh", ".aws", ".gnupg"];
    const FILES: [&str; 6] = [
        "credentials",
        "config.toml",
        "settings.yaml",
        "settings.json",
        "settings.local.json",
        "authorized_keys",
    ];
    if path.components().any(|c| {
        let part = c.as_os_str().to_string_lossy().to_lowercase();
        DIRS.contains(&part.as_str())
    }) {
        return true;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.starts_with(".env")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || FILES.contains(&name.as_str())
}

/// The real path of a call's target. A file that does not exist yet resolves
/// through its parent, so a new file in an allowed folder counts.
fn real_path(raw: &str, cwd: &Path) -> Option<PathBuf> {
    let joined = cwd.join(raw);
    if let Ok(real) = joined.canonicalize() {
        return Some(real);
    }
    let name = joined.file_name()?.to_owned();
    Some(joined.parent()?.canonicalize().ok()?.join(name))
}

/// The worktree this session claims: `<base>/<repo>/<node>` for the node its
/// registry row carries. No row, no node or no such folder means no claim.
fn own_worktree(session: &str, cwd: &Path) -> Option<PathBuf> {
    if session.is_empty() {
        return None;
    }
    let home = crate::paths::AgentsHome::from_env_opt()?;
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    let node = claimed_node(&registry, session)?;
    own_worktree_in(&bases(cwd), &crate::paths::canonical_repo_root(cwd)?, &node)
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

/// The checkout `<base>/<any folder>/<node>` whose `.git` file points into
/// this repo's worktree records. The folder name is never assumed.
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
            let target = pointer.trim().strip_prefix("gitdir:")?.trim();
            Path::new(target).starts_with(&records).then_some(real)
        })
}

/// The folders a worker's routine file work may touch: its own worktree, its
/// job tmp and its project's plan folder.
fn work_roots(cwd: &Path, own: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = own.map(Path::to_path_buf).into_iter().collect();
    if let Some(job) = std::env::var_os("CLAUDE_JOB_DIR") {
        roots.push(PathBuf::from(job));
    }
    roots.extend(plan_root(cwd));
    roots
        .into_iter()
        .filter_map(|r| r.canonicalize().ok())
        .collect()
}

/// The project's plan folder from the global `plans_dir` alone. The value is
/// usually a template, so `{project}` and `{vault}` resolve the way `fno plan
/// path` resolves them.
fn plan_root(cwd: &Path) -> Option<PathBuf> {
    let raw = crate::agents_config::config_lookup_global(&["plans_dir"])?
        .as_str()?
        .to_string();
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let project = crate::backlog::plans_path::project_folder(
        Some(&crate::paths::canonical_repo_root(cwd)?),
        false,
    )
    .ok()?;
    let vault = vault_dir(&home);
    let state = [
        home.join(".fno"),
        crate::paths_cli::state_root_for_worktrees(cwd),
    ];
    plan_root_from(&raw, &project, vault.as_deref(), &home, &state)
}

fn vault_dir(home: &Path) -> Option<PathBuf> {
    use crate::agents_config::config_lookup_global as global;
    if !global(&["obsidian", "enabled"])?.as_bool()? {
        return None;
    }
    let vault = global(&["obsidian", "vault"])?.as_str()?.to_string();
    let path = match vault.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if Path::new(&vault).is_absolute() => PathBuf::from(&vault),
        None => home.join(&vault),
    };
    Some(path)
}

/// Resolve a `plans_dir` value to a real folder that exists, is named
/// `plans` and sits outside every state folder. Anything else is no root.
fn plan_root_from(
    raw: &str,
    project: &str,
    vault: Option<&Path>,
    home: &Path,
    state: &[PathBuf],
) -> Option<PathBuf> {
    let mut text = raw
        .trim()
        .trim_end_matches('/')
        .replace("{project}", project);
    if text.contains("{vault}") {
        text = text.replace("{vault}", &vault?.to_string_lossy());
    }
    if text.contains('{') || text.contains('$') {
        return None;
    }
    let path = match text.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(&text),
    };
    if !path.is_absolute() {
        return None;
    }
    let real = path.canonicalize().ok()?;
    if !real.is_dir() || real.file_name()? != "plans" {
        return None;
    }
    let inside_state = state
        .iter()
        .any(|s| real.starts_with(s.canonicalize().unwrap_or_else(|_| s.clone())));
    (!inside_state).then_some(real)
}

/// The root `path` sits strictly inside, when it is a plain file path: no
/// link at any step from the root down, a missing leaf included.
fn root_without_links<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    let root = roots
        .iter()
        .find(|root| path != root.as_path() && path.starts_with(root))?;
    let rest = path.strip_prefix(root).ok()?;
    let mut step = root.clone();
    for part in rest.components() {
        step.push(part);
        let linked = std::fs::symlink_metadata(&step)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if linked {
            return None;
        }
    }
    if hard_linked(path) {
        return None;
    }
    Some(root)
}

/// An existing file with more than one name: an edit through it also edits
/// the other name, which may sit outside the root.
#[cfg(unix)]
fn hard_linked(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.nlink() > 1)
}

#[cfg(not(unix))]
fn hard_linked(_path: &Path) -> bool {
    false
}

/// True for a Read or an edit strictly inside the worker's own folders.
fn file_work_allowed(payload: &Payload, cwd: &Path, own: Option<&Path>, rules: &[String]) -> bool {
    let Some(raw) = file_target(payload) else {
        return false;
    };
    let Some(path) = real_path(raw, cwd) else {
        return false;
    };
    if blocked_name(&path) || ask_rule_hits(rules, &payload.tool_name, Some(&path), cwd) {
        return false;
    }
    root_without_links(&path, &work_roots(cwd, own)).is_some()
}

/// The `path` of an EnterWorktree call, the only shape that relocates. The
/// create form (`name`) stays inside the repo and never prompts.
fn path_call(payload: &Payload) -> Option<&str> {
    (payload.tool_name == "EnterWorktree" && !payload.tool_input.path.is_empty())
        .then_some(payload.tool_input.path.as_str())
}

/// The guard row a path call leaves: `allow`, or `ask` when the prompt stays.
/// Claude Code logs only a hook's decisions, so without the ask row a worker
/// parked at the prompt looks the same as one whose hook never ran.
fn row_decision(payload: &Payload, allowed: bool) -> Option<&'static str> {
    if allowed {
        return Some("allow");
    }
    path_call(payload).map(|_| "ask")
}

/// Every base fno has written checkouts to: the configured (or default) base,
/// plus both state-root defaults. An override moves new checkouts, yet
/// worktrees made before it, or by a peer repo with another config, still sit
/// under a default base and must not stop at the prompt.
fn bases(cwd: &Path) -> Vec<PathBuf> {
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

        // Every path answer leaves a row, so a stalled worker tells a
        // declined path (ask) from a hook that never answered (no row).
        let inside = call(&repo, &wt.to_string_lossy());
        let declined = call(&repo, &outside.to_string_lossy());
        assert_eq!(row_decision(&inside, true), Some("allow"));
        assert_eq!(row_decision(&declined, false), Some("ask"));
        assert_eq!(row_decision(&other, false), None);
        assert_eq!(row_decision(&create, false), None);
    }

    #[test]
    fn the_claim_names_the_worktree_by_the_repo_it_belongs_to() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        let records = repo.join(".git/worktrees/node");
        std::fs::create_dir_all(&records).unwrap();
        let state = tmp.path().join("state");
        // The folder name differs from the repo's name on purpose.
        let wt = crate::paths_cli::legacy_worktrees_base(&state).join("renamed/node");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", records.display())).unwrap();
        let foreign = crate::paths_cli::legacy_worktrees_base(&state).join("renamed/foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(
            foreign.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/foreign\n",
        )
        .unwrap();
        let configured = tmp.path().join("custom");
        std::fs::create_dir_all(&configured).unwrap();
        let all = bases_for(configured, &state);
        assert_eq!(
            own_worktree_in(&all, &repo, "node"),
            Some(wt.canonicalize().unwrap())
        );
        assert_eq!(own_worktree_in(&all, &repo, "foreign"), None);
        assert_eq!(own_worktree_in(&all, &repo, "other"), None);
    }

    #[test]
    fn only_a_true_boolean_turns_the_file_work_rule_on() {
        assert!(switch_on(Some(&toml::Value::Boolean(true))));
        assert!(!switch_on(Some(&toml::Value::Boolean(false))));
        assert!(!switch_on(None), "unset or unreadable reads as off");
        assert!(!switch_on(Some(&toml::Value::String("true".into()))));
        assert!(!switch_on(Some(&toml::Value::Integer(1))));
    }

    #[test]
    fn a_settings_ask_rule_keeps_the_prompt() {
        let cwd = Path::new("/w/repo");
        let rule = |r: &str| vec![r.to_string()];
        let inside = Path::new("/w/repo/src/lib.rs");
        assert!(ask_rule_hits(&rule("Edit"), "Write", Some(inside), cwd));
        assert!(ask_rule_hits(&rule("Read"), "Read", Some(inside), cwd));
        assert!(ask_rule_hits(
            &rule("Edit(src/**)"),
            "Edit",
            Some(inside),
            cwd
        ));
        assert!(ask_rule_hits(
            &rule("Edit(//w/repo/src)"),
            "Edit",
            Some(inside),
            cwd
        ));
        assert!(ask_rule_hits(
            &rule("EnterWorktree"),
            "EnterWorktree",
            None,
            cwd
        ));
        assert!(ask_rule_hits(&rule("*"), "Read", Some(inside), cwd));
        assert!(!ask_rule_hits(
            &rule("Edit(docs/**)"),
            "Edit",
            Some(inside),
            cwd
        ));
        assert!(!ask_rule_hits(
            &rule("Bash(rm *)"),
            "Edit",
            Some(inside),
            cwd
        ));
        assert!(!ask_rule_hits(&[], "Edit", Some(inside), cwd));
    }

    #[test]
    fn a_templated_plans_dir_resolves_and_must_sit_outside_the_state_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let vault = home.join("vault");
        let plans = vault.join("internal/proj/plans");
        std::fs::create_dir_all(&plans).unwrap();
        let in_state = home.join(".fno/internal/proj/plans");
        std::fs::create_dir_all(&in_state).unwrap();
        let state = [home.join(".fno")];
        let raw = "{vault}/internal/{project}/plans/";
        assert_eq!(
            plan_root_from(raw, "proj", Some(&vault), &home, &state),
            Some(plans.clone())
        );
        assert_eq!(plan_root_from(raw, "proj", None, &home, &state), None);
        assert_eq!(
            plan_root_from(raw, "gone", Some(&vault), &home, &state),
            None
        );
        assert_eq!(
            plan_root_from("{team}/plans", "proj", None, &home, &state),
            None
        );
        assert_eq!(
            plan_root_from(".fno/plans/", "proj", None, &home, &state),
            None
        );
        let raw_state = "~/.fno/internal/{project}/plans";
        assert_eq!(plan_root_from(raw_state, "proj", None, &home, &state), None);
    }

    #[test]
    fn file_work_is_allowed_only_inside_the_workers_own_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let wt = root.join("worktrees/repo/node");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        let roots = vec![wt.clone()];
        assert!(root_without_links(&wt.join("src/lib.rs"), &roots).is_some());
        assert!(root_without_links(&wt, &roots).is_none(), "the root itself");
        assert!(root_without_links(&root.join("elsewhere/x.rs"), &roots).is_none());
        assert!(!blocked_name(Path::new("src/runtime/prompt_wait.rs")));
        for name in [".GIT/index", ".Claude/x", "a/.ENV.local", "k/SERVER.PEM"] {
            assert!(blocked_name(Path::new(name)), "{name}");
        }
        let write: Payload = serde_json::from_value(
            json!({"tool_name": "Write", "tool_input": {"file_path": "a.rs"}}),
        )
        .unwrap();
        assert_eq!(file_target(&write), Some("a.rs"));
        let bash: Payload =
            serde_json::from_value(json!({"tool_name": "Bash", "tool_input": {}})).unwrap();
        assert_eq!(file_target(&bash), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_worktree_never_counts_as_a_plain_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let wt = root.join("worktrees/repo/node");
        std::fs::create_dir_all(&wt).unwrap();
        let roots = vec![wt.clone()];
        std::os::unix::fs::symlink(root.join("nowhere"), wt.join("dangling")).unwrap();
        assert!(root_without_links(&wt.join("dangling"), &roots).is_none());
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, wt.join("dir")).unwrap();
        assert!(root_without_links(&wt.join("dir/new.txt"), &roots).is_none());
        std::fs::write(wt.join("kept.txt"), "x").unwrap();
        std::fs::hard_link(wt.join("kept.txt"), root.join("second-name.txt")).unwrap();
        assert!(root_without_links(&wt.join("kept.txt"), &roots).is_none());
    }
}
