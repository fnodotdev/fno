//! Plain Claude Code permission rules for a worker's routine file work and
//! fno's own commands.
//!
//! When `user.auto_approve_routine` is on in the global config, a spawn gets
//! `permissions.allow` rules for its own worktree and its project's plan
//! folder, and `permissions.ask` rules for the names that hold secrets or
//! settings. fno's must-enable command rules ride every spawn regardless
//! (user ruling D3653), overridable per role under `[allow_rules]`. The
//! rules ride the spawn's own settings file, never a file the worker can
//! edit. Claude Code then applies its own order across every settings
//! source it loads: deny beats ask, and ask beats allow. fno reads no
//! settings file and guesses no precedence.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The allow and ask rule lists a spawn adds to its settings file.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Rules {
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
const SETTINGS_NAMES: [&str; 10] = [
    "config.toml",
    ".cargo/config",
    ".github/workflows/**",
    ".git",
    "settings.yaml",
    "settings.json",
    "settings.local.json",
    ".git/**",
    ".claude/**",
    ".fno/**",
];

/// The rules for the spawn at `cwd`: the file rules while the switch is on
/// and `cwd` is a worker's own worktree, plus fno's command rules unless the
/// `[allow_rules]` override says off for this role. `node` is the child's
/// node from the spawn payload.
pub(crate) fn rules_for(cwd: &Path, node: Option<&str>, role: Option<&str>) -> Rules {
    let mut rules = file_rules(cwd, node);
    rules.allow.extend(command_rules(role));
    rules
}

/// The file rules alone: none while the switch is off, or when `cwd` is not
/// a worker's own worktree.
fn file_rules(cwd: &Path, node: Option<&str>) -> Rules {
    if !switch_on(
        crate::agents_config::config_lookup_global(&["user", "auto_approve_routine"]).as_ref(),
    ) {
        return Rules::default();
    }
    rules_when(
        true,
        crate::hook::worktree_relocation_allow::verified_worktree(cwd, node),
        plan_root(cwd),
    )
}

/// fno's own command rules, from the vendored must-enable table. On for
/// every role by default; the config `[allow_rules] default = false` turns
/// them off for every spawn and `[allow_rules.roles] <role> = false` for
/// one role. A value that is missing or mistyped reads as on: the ruling
/// (D3653) is the default.
pub(crate) fn command_rules(role: Option<&str>) -> Vec<String> {
    let default = crate::agents_config::config_lookup_global(&["allow_rules", "default"])
        .and_then(|v| v.as_bool());
    let role_off = role
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .and_then(|role| {
            crate::agents_config::config_lookup_global(&["allow_rules", "roles", role])
                .and_then(|v| v.as_bool())
        });
    if !override_on(default, role_off) {
        return Vec::new();
    }
    table_allow_rules()
}

/// The vendored byte-copy of crate fno's must-enable table
/// (`crates/fno/src/setup/must_enable.toml`). This crate links `fno` as a
/// dev-dependency only (the publish gate holds), so the table travels as a
/// copy and `must_work_corpus` pins the two files byte-identical. Crate fno
/// vets the rows through its filter loader before they ship; this side only
/// renders the prefixes it already trusts. A copy that fails to parse rules
/// nothing, which is the safe direction: it grants no command.
const MUST_ENABLE_TABLE: &str = include_str!("must_enable.toml");

#[derive(Deserialize)]
#[serde(untagged)]
enum RuleWord {
    One(String),
    Any(Vec<String>),
}

#[derive(Deserialize)]
struct RuleRow {
    prefix: Vec<RuleWord>,
}

#[derive(Deserialize)]
struct RuleTable {
    #[serde(default)]
    rule: Vec<RuleRow>,
}

/// One `Bash(<prefix> *)` per expanded alternative, in Claude's
/// `permissions.allow` form, matching crate fno's `List::claude` render.
fn table_allow_rules() -> Vec<String> {
    let Ok(table) = toml::from_str::<RuleTable>(MUST_ENABLE_TABLE) else {
        return Vec::new();
    };
    table
        .rule
        .iter()
        .flat_map(|row| {
            let mut out: Vec<Vec<String>> = vec![Vec::new()];
            for word in &row.prefix {
                let alts: Vec<&String> = match word {
                    RuleWord::One(w) => vec![w],
                    RuleWord::Any(ws) => ws.iter().collect(),
                };
                out = out
                    .iter()
                    .flat_map(|head| {
                        alts.iter().map(move |w| {
                            let mut next = head.clone();
                            next.push((*w).clone());
                            next
                        })
                    })
                    .collect();
            }
            out
        })
        .map(|words| format!("Bash({} *)", words.join(" ")))
        .collect()
}

/// The override decision, pure for the tests: the role value wins over the
/// default, and only an explicit `false` turns the rules off.
fn override_on(default: Option<bool>, role_off: Option<bool>) -> bool {
    match role_off {
        Some(false) => false,
        _ => default.unwrap_or(true),
    }
}

/// The gate itself: no rules while the switch is off, and none without a
/// worktree that is really the worker's own.
fn rules_when(on: bool, own: Option<PathBuf>, plan: Option<PathBuf>) -> Rules {
    let Some(own) = own.filter(|_| on) else {
        return Rules::default();
    };
    let mut roots = vec![own];
    roots.extend(plan);
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
        // A glob character in the path would change what the rule matches.
        if real
            .to_string_lossy()
            .contains(['*', '?', '[', ')', '(', '\\'])
        {
            continue;
        }
        let anchor = format!("//{}", real.to_string_lossy().trim_start_matches('/'));
        rules.allow.push(format!("Read({anchor}/**)"));
        rules.allow.push(format!("Edit({anchor}/**)"));
        // In a worktree `.git` is a file at the root, so name it directly too.
        rules.ask.push(format!("Edit({anchor}/.git)"));
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
    fn the_rules_gate_on_the_switch_and_a_real_worktree_and_name_the_real_folder() {
        assert!(switch_on(Some(&toml::Value::Boolean(true))));
        for off in [
            Some(toml::Value::Boolean(false)),
            None,
            Some(toml::Value::String("true".into())),
            Some(toml::Value::Integer(1)),
        ] {
            assert!(!switch_on(off.as_ref()), "{off:?}");
        }
        // The command-rule override, both arms: only an explicit role
        // `false` turns the rules off; a missing or mistyped value reads as
        // on (D3653's law is the default).
        assert!(override_on(None, None));
        assert!(override_on(None, Some(true)));
        assert!(override_on(Some(true), Some(true)));
        assert!(!override_on(Some(false), None));
        assert!(!override_on(Some(true), Some(false)));
        assert!(
            override_on(Some(false), Some(true)),
            "an explicit role true overrides even a default off"
        );
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().canonicalize().unwrap().join("wt");
        std::fs::create_dir_all(&real).unwrap();
        assert!(rules_when(false, Some(real.clone()), None).is_empty());
        assert!(
            rules_when(true, None, None).is_empty(),
            "no worktree, no rules"
        );
        let rules = rules_when(true, Some(real.clone()), Some(tmp.path().join("gone")));
        let anchor = format!("//{}", real.to_string_lossy().trim_start_matches('/'));
        assert_eq!(rules.allow.len(), 2, "a missing folder adds nothing");
        assert!(rules.allow.iter().all(|r| r.contains(&anchor)));
        // In a worktree `.git` is a file, so the file name itself keeps its ask.
        for want in [
            format!("Edit({anchor}/.git)"),
            format!("Edit({anchor}/**/.git)"),
            format!("Edit({anchor}/**/.git/**)"),
            format!("Edit({anchor}/**/.cargo/config)"),
            format!("Edit({anchor}/**/.github/workflows/**)"),
            format!("Read({anchor}/**/.env*)"),
        ] {
            assert!(rules.ask.contains(&want), "{want}");
        }
        // A glob character in the path would change the rule's meaning.
        let odd = tmp.path().canonicalize().unwrap().join("a[b]*c");
        std::fs::create_dir_all(&odd).unwrap();
        assert!(rules_when(true, Some(odd), None).is_empty());
        let slash = tmp.path().canonicalize().unwrap().join("a\\b");
        std::fs::create_dir_all(&slash).unwrap();
        assert!(
            rules_when(true, Some(slash), None).is_empty(),
            "a backslash escapes"
        );
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let via = rules_when(true, Some(link), None);
            assert!(via.allow.iter().all(|r| r.contains(&anchor)), "real path");
        }
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
