//! Where a one-shot reader runs: harness, model and route from the stage
//! table. `agents.profiles.<reader>` overlays `agents.defaults`, the same
//! keys autonomous dispatch and the spawn compose read, so a reader lands on
//! the harness the user set up and never on a hardcoded one.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::harness_capabilities::ContractError;

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

/// The one-shot reader's spawn line, shared by the judges' `default_spawn`
/// and the merge architect. The harness comes from the route, never a constant. The flags after
/// the `--` fence arrive as harness_args (not prompt words): the model, then
/// the harness row's `reader_args`, so the reader loads no settings, MCP
/// servers, tools or chrome and answers inside the bound. The spawn's own
/// --model axis is retired; a route rides `--route` and owns the model.
///
/// Every spawn-owned flag rides BEFORE the prompt and the prompt sits
/// directly before the explicit fence: the positional boundary refuses a
/// spawn-owned flag after the prompt (law d-3244), and a builder line is
/// judged at the seam on the raw spelling it emits. `extra_owned` carries a
/// caller's own pre-prompt spawn flags, already ordered as flag/value pairs.
///
/// The reader pins its own machine and names its arm. Both flags ride the
/// pre-fence spawn axis: a fleet-load placement once relayed the bounded
/// one-shot to a remote host whose lane was dead, the tick then failed for
/// an hour at a stretch, and an unnamed arm stamps its mail `unknown`. A
/// judge reads local state on an hourly cadence; it never needs a remote
/// lane.
pub(crate) fn reader_argv(
    name: &str,
    prompt: &str,
    cwd: &Path,
    timeout_secs: u64,
    route: &ReaderRoute,
    extra_owned: &[String],
) -> Vec<String> {
    let mut argv = argv_head(name, cwd, timeout_secs, route, extra_owned);
    argv.push(prompt.to_string());
    argv.push("--".to_string());
    if let Some(model) = &route.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    argv.extend(
        crate::harness_capabilities::packaged_reader_args(&route.harness).unwrap_or_default(),
    );
    argv
}

/// The tool-bearing reader's spawn line: [`reader_argv`]'s shape with the
/// harness's `reader_tools_args` row in the fence instead of the tool-less
/// form. A row without a measured tool-bearing form refuses: the reader's
/// prompt promises file reads, and the plain headless lane cannot honor
/// that promise.
pub(crate) fn reader_tools_argv(
    name: &str,
    prompt: &str,
    cwd: &Path,
    timeout_secs: u64,
    route: &ReaderRoute,
    extra_owned: &[String],
) -> Result<Vec<String>, ContractError> {
    let tools = crate::harness_capabilities::packaged_reader_tools_args(&route.harness)
        .filter(|args| !args.is_empty())
        .ok_or_else(|| {
            crate::harness_capabilities::field_error(
                &route.harness,
                "reader_tools_args",
                "no measured tool-bearing reader form; add reader_tools_args to the row",
            )
        })?;
    let mut argv = argv_head(name, cwd, timeout_secs, route, extra_owned);
    argv.push(prompt.to_string());
    argv.push("--".to_string());
    if let Some(model) = &route.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    argv.extend(tools);
    Ok(argv)
}

/// The spawn flags before the prompt: identity, machine, harness lane, and
/// the caller's own pre-prompt flags. Shared by both reader fence forms.
fn argv_head(
    name: &str,
    cwd: &Path,
    timeout_secs: u64,
    route: &ReaderRoute,
    extra_owned: &[String],
) -> Vec<String> {
    let mut argv: Vec<String> = [
        "agents",
        "spawn",
        "--name",
        name,
        "--machine",
        &crate::name_ledger::local_bus_host(),
        "--from-name",
        &format!("fno/{name}"),
        "--harness",
        &route.harness,
        "--once",
        "--cwd",
        &cwd.display().to_string(),
        "--timeout",
        &timeout_secs.to_string(),
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Some(lane) = &route.route {
        argv.extend(["--route".to_string(), lane.clone()]);
    }
    argv.extend(extra_owned.iter().cloned());
    argv
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

    #[test]
    fn reader_argv_carries_the_routed_harness_and_its_own_reader_form() {
        let split = |argv: Vec<String>| {
            let at = argv.iter().position(|t| t == "--").unwrap();
            let head: Vec<String> = argv.iter().take(at).cloned().collect();
            let tail: Vec<String> = argv.into_iter().skip(at + 1).collect();
            (head, tail)
        };
        let (head, tail) = split(reader_argv(
            "question-judge",
            "grade this",
            Path::new("/tmp"),
            60,
            &ReaderRoute::pinned("opencode", "zai/glm-5.3"),
            &[],
        ));
        assert!(head.windows(2).any(|w| w == ["--harness", "opencode"]));
        assert_eq!(head.last().map(String::as_str), Some("grade this"));
        assert_eq!(tail, ["--model", "zai/glm-5.3"]);

        // claude keeps its tool-less form, read off its capability row.
        let (_, tail) = split(reader_argv(
            "pr-steward",
            "read",
            Path::new("/tmp"),
            60,
            &ReaderRoute::pinned("claude", "sonnet"),
            &[],
        ));
        assert!(tail.starts_with(&["--model".to_string(), "sonnet".to_string()]));
        assert!(tail.iter().any(|t| t == "--strict-mcp-config"));
    }

    #[test]
    fn the_tools_form_carries_the_grant_and_an_unmeasured_row_refuses() {
        let split = |argv: Vec<String>| {
            let at = argv.iter().position(|t| t == "--").unwrap();
            let head: Vec<String> = argv.iter().take(at).cloned().collect();
            let tail: Vec<String> = argv.into_iter().skip(at + 1).collect();
            (head, tail)
        };
        let (head, tail) = split(
            reader_tools_argv(
                "golden-drafter",
                "draft this",
                Path::new("/tmp"),
                60,
                &ReaderRoute::pinned("claude", "sonnet"),
                &[],
            )
            .unwrap(),
        );
        assert!(head.windows(2).any(|w| w == ["--harness", "claude"]));
        assert_eq!(head.last().map(String::as_str), Some("draft this"));
        assert!(tail.starts_with(&["--model".to_string(), "sonnet".to_string()]));
        // The row's read-tools grant rides whole: the tools pair and the
        // allowlist pair, not the tool-less empty `--tools ""` form.
        let at = tail.iter().position(|t| t == "--tools").unwrap();
        assert_eq!(tail.get(at + 1), Some(&"Read,Grep,Glob".to_string()));
        assert!(tail
            .windows(2)
            .any(|w| w == ["--allowedTools", "Read,Grep,Glob"]));

        // A harness with no measured tool-bearing form refuses by name
        // instead of running a reader that cannot read.
        let err = reader_tools_argv(
            "golden-drafter",
            "draft this",
            Path::new("/tmp"),
            60,
            &ReaderRoute::pinned("codex", ""),
            &[],
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("codex"), "{text}");
        assert!(text.contains("reader_tools_args"), "{text}");
    }
}
