//! Where a one-shot reader runs: harness, model and route from the stage
//! table. `agents.profiles.<reader>` overlays `agents.defaults`, the same
//! keys autonomous dispatch and the spawn compose read, so a reader lands on
//! the harness the user set up and never on a hardcoded one.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The resolved coordinate for one reader spawn. A `route` owns the model,
/// the same way an explicit `--route` does on the spawn door, so the two are
/// never both set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderRoute {
    pub harness: String,
    pub model: Option<String>,
    pub route: Option<String>,
}

impl ReaderRoute {
    /// A route its caller already resolved (a lane walk).
    pub(crate) fn pinned(harness: &str, model: &str) -> Self {
        Self {
            harness: harness.to_string(),
            model: Some(model.to_string()).filter(|m| !m.is_empty()),
            route: None,
        }
    }

    /// The model a receipt or ledger row names: the fenced model, else the
    /// route, else the harness's own default.
    pub(crate) fn label(&self) -> String {
        self.model
            .clone()
            .or_else(|| self.route.clone())
            .unwrap_or_else(|| format!("{} default", self.harness))
    }
}

/// A retired per-reader model key, read beneath the profile for one release.
pub(crate) struct Legacy<'a> {
    pub keys: &'a [&'a str],
}

/// Resolve the reader's route from config under `cwd`. `claude_model` is the
/// reader's built-in pick, used only when the reader lands on claude with no
/// configured model; any other harness then runs its own default.
pub(crate) fn resolve(
    cwd: &Path,
    profile: &str,
    legacy: Option<Legacy>,
    claude_model: &str,
) -> ReaderRoute {
    let read = |keys: &[&str]| -> Option<String> {
        crate::agents_config::config_lookup(cwd, keys)
            .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
    };
    let (route, note) = resolve_with(&read, profile, legacy, claude_model);
    if let Some(note) = note {
        warn_once(&note);
    }
    route
}

/// The resolution over a key reader, plus the deprecation line a set legacy
/// key earns.
pub(crate) fn resolve_with(
    read: &dyn Fn(&[&str]) -> Option<String>,
    profile: &str,
    legacy: Option<Legacy>,
    claude_model: &str,
) -> (ReaderRoute, Option<String>) {
    let own = |field: &str| read(&["agents", "profiles", profile, field]);
    let base = |field: &str| read(&["agents", "defaults", field]);
    // The spawn door lowercases the harness; the capability row lookup and the
    // claude default must read the same spelling.
    let harness_of = |v: String| v.to_lowercase();
    let base_harness = base("provider").map_or_else(|| "claude".to_string(), harness_of);
    let harness = own("provider").map_or_else(|| base_harness.clone(), harness_of);
    // The defaults' model and route were written for the defaults' harness;
    // on another harness they would name a model that CLI cannot launch.
    let base_applies = harness == base_harness;
    let legacy_hit = legacy.and_then(|l| read(l.keys).map(|v| (l.keys.join("."), v)));
    let note = legacy_hit.as_ref().map(|(key, _)| {
        format!(
            "fno: {key} is deprecated; set agents.profiles.{profile}.model instead \
             (the old key still reads for one release)"
        )
    });
    let route = own("route").or_else(|| base_applies.then(|| base("route")).flatten());
    let model = if route.is_some() {
        None
    } else {
        own("model")
            .or_else(|| legacy_hit.map(|(_, v)| v))
            .or_else(|| base_applies.then(|| base("model")).flatten())
            .or_else(|| (harness == "claude").then(|| claude_model.to_string()))
    };
    (
        ReaderRoute {
            harness,
            model,
            route,
        },
        note,
    )
}

fn warn_once(note: &str) {
    static SEEN: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if seen.insert(note.to_string()) {
        eprintln!("{note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn table(rows: &[(&str, &str)]) -> impl Fn(&[&str]) -> Option<String> {
        let map: BTreeMap<String, String> = rows
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |keys: &[&str]| map.get(&keys.join(".")).cloned()
    }

    #[test]
    fn stage_table_picks_harness_and_model_with_legacy_and_defaults_fallbacks() {
        // Nothing set: claude with the reader's own pick.
        let (r, note) = resolve_with(&table(&[]), "question-judge", None, "sonnet");
        assert_eq!(r, ReaderRoute::pinned("claude", "sonnet"));
        assert!(note.is_none());

        // An opencode-first user: the setup default carries the reader, which
        // runs that harness's own default model, never claude's pick.
        let (r, _) = resolve_with(
            &table(&[("agents.defaults.provider", "OpenCode")]),
            "question-judge",
            None,
            "sonnet",
        );
        assert_eq!((r.harness.as_str(), r.model), ("opencode", None));

        // The profile wins; a defaults model for another harness never rides.
        let (r, _) = resolve_with(
            &table(&[
                ("agents.defaults.provider", "claude"),
                ("agents.defaults.model", "opus"),
                ("agents.profiles.question-judge.provider", "codex"),
            ]),
            "question-judge",
            None,
            "sonnet",
        );
        assert_eq!((r.harness.as_str(), r.model), ("codex", None));

        // The retired key reads beneath the profile model and names the new key.
        let legacy = || {
            Some(Legacy {
                keys: &["questions", "judge", "model"],
            })
        };
        let (r, note) = resolve_with(
            &table(&[("questions.judge.model", "haiku")]),
            "question-judge",
            legacy(),
            "sonnet",
        );
        assert_eq!(r.model.as_deref(), Some("haiku"));
        assert!(note
            .unwrap()
            .contains("agents.profiles.question-judge.model"));
        let (r, _) = resolve_with(
            &table(&[
                ("questions.judge.model", "haiku"),
                ("agents.profiles.question-judge.model", "opus"),
            ]),
            "question-judge",
            legacy(),
            "sonnet",
        );
        assert_eq!(r.model.as_deref(), Some("opus"));

        // A route owns the model.
        let (r, _) = resolve_with(
            &table(&[("agents.profiles.pr-steward.route", "zai/glm-5.3")]),
            "pr-steward",
            None,
            "sonnet",
        );
        assert_eq!((r.route.as_deref(), r.model), (Some("zai/glm-5.3"), None));
    }
}
