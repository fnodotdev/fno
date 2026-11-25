//! PermissionRequest hook: allow EnterWorktree into a worktree under the
//! resolved `paths.worktrees_base` or either state-root default base.
//!
//! Claude Code asks before EnterWorktree moves the session to a path outside
//! its own managed worktree dir in the repo ("permission-root relocation").
//! The ask is a safety check the auto-mode classifier may not approve, and an allow rule
//! is never read for it, so an auto-mode worker that launches from the repo
//! root stops at the prompt. A PermissionRequest hook is the one answer short
//! of bypassPermissions. Allowing only the fno base keeps the ask for every
//! other path.
//!
//! Both paths are canonicalized before the prefix test, so a symlink under
//! the base cannot point the session elsewhere. Anything else prints `{}`,
//! which leaves the prompt in place: this hook never denies.
//!
//! The base holds every repo's worktrees, yet the allow cannot carry a
//! session into another repo: Claude Code refuses any path that is not a
//! registered worktree of the session's own repo, after this hook allows.

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
    let allowed = bases(&cwd).iter().any(|base| allows(&payload, base));
    let file_ok = file_work_allowed(&payload, &cwd);
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

/// The file a routine tool call touches, and whether it writes. Reads of any
/// non-secret file and edits inside the worker's own folders never need a
/// person: the user ruled that routine work raises no prompt.
fn file_target(payload: &Payload) -> Option<(&str, bool)> {
    let input = &payload.tool_input;
    let (raw, write) = match payload.tool_name.as_str() {
        "Write" | "Edit" | "MultiEdit" => (input.file_path.as_str(), true),
        "NotebookEdit" => (input.notebook_path.as_str(), true),
        "Read" => (input.file_path.as_str(), false),
        _ => return None,
    };
    (!raw.is_empty()).then_some((raw, write))
}

/// Names a routine call never clears: git internals (hooks run code), the
/// harness's own settings, ssh and cloud credentials, env files, keys, and
/// fno's config. A prompt stays for each.
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
    if path
        .components()
        .any(|c| DIRS.iter().any(|d| c.as_os_str() == *d))
    {
        return true;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
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

/// The worker's own worktree: `<base>/<repo>/<node>` for the base `cwd` sits
/// under. A session outside every base has none, so it earns no edit rule.
fn worktree_root(cwd: &Path) -> Option<PathBuf> {
    let real = cwd.canonicalize().ok()?;
    bases(cwd).iter().find_map(|base| {
        let base = base.canonicalize().ok()?;
        let rest = real.strip_prefix(&base).ok()?;
        let mut parts = rest.components().take(2);
        let (repo, node) = (parts.next()?, parts.next()?);
        Some(base.join(repo).join(node))
    })
}

fn write_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = worktree_root(cwd).into_iter().collect();
    if let Some(job) = std::env::var_os("CLAUDE_JOB_DIR") {
        roots.push(PathBuf::from(job));
    }
    roots.extend(crate::paths_cli::state_dir_absolute(cwd).ok());
    roots.push(crate::paths_cli::state_root_for_worktrees(cwd));
    roots.extend(crate::paths_cli::plans_dir_for(cwd));
    roots
        .into_iter()
        .filter_map(|r| r.canonicalize().ok())
        .collect()
}

fn under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path != root.as_path() && path.starts_with(root))
}

/// True for a Read of a non-secret file, or an edit strictly inside the
/// worker's worktree, its job tmp, the fno state folders or the plan vault.
fn file_work_allowed(payload: &Payload, cwd: &Path) -> bool {
    let Some((raw, write)) = file_target(payload) else {
        return false;
    };
    let Some(path) = real_path(raw, cwd) else {
        return false;
    };
    if blocked_name(&path) {
        return false;
    }
    !write || under_roots(&path, &write_roots(cwd))
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
/// sits strictly under the real base. Separated from `run` so the tests
/// exercise the same predicate with no stdin or config in the loop.
fn allows(payload: &Payload, base: &Path) -> bool {
    let Some(raw) = path_call(payload) else {
        return false;
    };
    let target = payload_cwd(payload).join(raw);
    let (Ok(target), Ok(base)) = (target.canonicalize(), base.canonicalize()) else {
        return false;
    };
    target != base && target.starts_with(&base)
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
    fn allows_only_a_real_path_strictly_under_the_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("worktrees");
        let wt = base.join("repo").join("node");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        assert!(allows(&call(&repo, &wt.to_string_lossy()), &base));
        // Relative paths resolve against the payload cwd.
        assert!(allows(&call(&base, "repo/node"), &base));
        // The base itself, an outside dir, a missing dir: the prompt stays.
        assert!(!allows(&call(&repo, &base.to_string_lossy()), &base));
        assert!(!allows(&call(&repo, &outside.to_string_lossy()), &base));
        assert!(!allows(
            &call(&repo, &base.join("gone").to_string_lossy()),
            &base
        ));
        // A `..` escape canonicalizes out of the base.
        let escape = format!("{}/repo/../../elsewhere", base.display());
        assert!(!allows(&call(&repo, &escape), &base));
        // Another tool, or the create form with no path, never allows.
        let mut other = call(&repo, &wt.to_string_lossy());
        other.tool_name = "Bash".into();
        assert!(!allows(&other, &base));
        let create: Payload = serde_json::from_value(
            json!({"tool_name": "EnterWorktree", "tool_input": {"name": "x"}}),
        )
        .unwrap();
        assert!(!allows(&create, &base));

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
    fn every_base_fno_writes_to_is_allowed_even_under_an_override() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let spaces = crate::paths_cli::default_worktrees_base(&state).join("repo/node");
        let legacy = crate::paths_cli::legacy_worktrees_base(&state).join("repo/node");
        std::fs::create_dir_all(&spaces).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        let configured = tmp.path().join("custom");
        std::fs::create_dir_all(&configured).unwrap();
        let all = bases_for(configured, &state);
        for wt in [&spaces, &legacy] {
            let p = call(tmp.path(), &wt.to_string_lossy());
            assert!(all.iter().any(|b| allows(&p, b)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_under_the_base_pointing_out_keeps_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("worktrees");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(!allows(&call(tmp.path(), &link.to_string_lossy()), &base));
    }

    #[test]
    fn routine_file_work_is_allowed_and_secrets_and_outsiders_keep_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let roots = vec![root.join("worktrees/repo/node")];
        std::fs::create_dir_all(&roots[0]).unwrap();
        let inside = roots[0].join("src/lib.rs");
        assert!(under_roots(&inside, &roots), "a file in the worktree");
        assert!(!under_roots(&roots[0], &roots), "the root itself");
        assert!(!under_roots(&root.join("elsewhere/x.rs"), &roots));
        for blocked in [
            ".git/hooks/pre-commit",
            ".claude/settings.json",
            "deploy/.env.production",
            "keys/server.pem",
            "home/.ssh/config",
            ".fno/config.toml",
        ] {
            assert!(blocked_name(Path::new(blocked)), "{blocked}");
        }
        assert!(!blocked_name(Path::new("src/runtime/prompt_wait.rs")));
        let write: Payload = serde_json::from_value(
            json!({"tool_name": "Write", "tool_input": {"file_path": "a.rs"}}),
        )
        .unwrap();
        assert_eq!(file_target(&write), Some(("a.rs", true)));
        let read: Payload = serde_json::from_value(
            json!({"tool_name": "Read", "tool_input": {"file_path": "a.rs"}}),
        )
        .unwrap();
        assert_eq!(file_target(&read), Some(("a.rs", false)));
        let bash: Payload =
            serde_json::from_value(json!({"tool_name": "Bash", "tool_input": {}})).unwrap();
        assert_eq!(file_target(&bash), None);
    }
}
