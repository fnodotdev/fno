//! Plain Claude Code permission rules for a worker's routine file work.
//!
//! When `user.auto_approve_routine` is on in the global config, a spawn gets
//! `permissions.allow` rules for its own worktree and its project's plan
//! folder, and `permissions.ask` rules for the names that hold secrets or
//! settings. The rules ride the spawn's own settings file, never a file the
//! worker can edit. Claude Code then applies its own order across every
//! settings source it loads: deny beats ask, and ask beats allow.
//! fno reads no settings file and guesses no precedence.

use std::path::{Path, PathBuf};

/// The allow and ask rule lists a spawn adds to its settings file.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rules {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
}

impl Rules {
    pub(crate) fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.ask.is_empty()
    }
}

/// Names that keep their prompt for a read and an edit.
const SECRET_NAMES: [&str; 8] = [
    ".env*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    "credentials",
    "authorized_keys",
    ".ssh/**",
];
/// Folders whose contents keep their prompt for a read and an edit.
const SECRET_DIRS: [&str; 2] = [".aws/**", ".gnupg/**"];
/// Names that keep their prompt for an edit only.
const SETTINGS_NAMES: [&str; 7] = [
    "config.toml",
    "settings.yaml",
    "settings.json",
    "settings.local.json",
    ".git/**",
    ".claude/**",
    ".fno/**",
];

/// The rules for the spawn at `cwd`: none while the switch is off, or when
/// `cwd` is not a worker's own worktree.
pub(crate) fn rules_for(cwd: &Path) -> Rules {
    if !switch_on(
        crate::agents_config::config_lookup_global(&["user", "auto_approve_routine"]).as_ref(),
    ) {
        return Rules::default();
    }
    let Some(own) = worktree_root(cwd) else {
        return Rules::default();
    };
    let mut roots = vec![own];
    roots.extend(plan_root(cwd));
    rules_from(&roots)
}

/// Only a boolean `true` turns the rules on. A missing, unreadable or
/// mistyped value reads as off.
fn switch_on(value: Option<&toml::Value>) -> bool {
    value.and_then(toml::Value::as_bool).unwrap_or(false)
}

/// The rules for these real folders. A pattern starting `//` is an absolute
/// path in Claude Code's rule syntax.
fn rules_from(roots: &[PathBuf]) -> Rules {
    let mut rules = Rules::default();
    for root in roots {
        let Ok(real) = root.canonicalize() else {
            continue;
        };
        let anchor = format!("//{}", real.to_string_lossy().trim_start_matches('/'));
        rules.allow.push(format!("Read({anchor}/**)"));
        rules.allow.push(format!("Edit({anchor}/**)"));
        for name in SECRET_NAMES.iter().chain(SECRET_DIRS.iter()) {
            rules.ask.push(format!("Read({anchor}/**/{name})"));
            rules.ask.push(format!("Edit({anchor}/**/{name})"));
        }
        for name in SETTINGS_NAMES {
            rules.ask.push(format!("Edit({anchor}/**/{name})"));
        }
    }
    rules
}

/// The worker's own worktree: `<base>/<repo>/<node>` for the base `cwd` sits
/// under. A session outside every base has none, so it earns no rules.
fn worktree_root(cwd: &Path) -> Option<PathBuf> {
    let real = cwd.canonicalize().ok()?;
    crate::hook::worktree_relocation_allow::bases(cwd)
        .iter()
        .find_map(|base| {
            let base = base.canonicalize().ok()?;
            let rest = real.strip_prefix(&base).ok()?;
            let mut parts = rest.components().take(2);
            let (repo, node) = (parts.next()?, parts.next()?);
            Some(base.join(repo).join(node))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_true_boolean_turns_the_rules_on() {
        assert!(switch_on(Some(&toml::Value::Boolean(true))));
        assert!(!switch_on(Some(&toml::Value::Boolean(false))));
        assert!(!switch_on(None), "unset or unreadable reads as off");
        assert!(!switch_on(Some(&toml::Value::String("true".into()))));
        assert!(!switch_on(Some(&toml::Value::Integer(1))));
    }

    #[cfg(unix)]
    #[test]
    fn the_rules_name_the_real_folder_and_skip_one_that_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().canonicalize().unwrap().join("wt");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let rules = rules_from(&[link, tmp.path().join("gone")]);
        let anchor = format!("//{}", real.to_string_lossy().trim_start_matches('/'));
        assert_eq!(rules.allow.len(), 2, "the missing folder adds nothing");
        assert!(rules.allow.iter().all(|r| r.contains(&anchor)));
        assert!(rules.ask.contains(&format!("Edit({anchor}/**/.git/**)")));
        assert!(rules.ask.contains(&format!("Read({anchor}/**/.env*)")));
        assert!(!rules.is_empty() && Rules::default().is_empty());
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
}
