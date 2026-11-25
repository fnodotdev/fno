//! The `resume --route` step: re-point a claude row onto a new route before
//! reentry. LIVES OUTSIDE client_commands for the same reason resume_args.rs
//! does: that file is over the 5,000-line budget and shrink-only.
//!
//! Order matters. Every refusal (harness, pane, unresolvable route,
//! config-dir move, attended session) lands BEFORE anything stops, so a
//! refused re-route leaves the row and any live session untouched. The stop
//! reuses the daemon's own bounded supervisor shellout
//! (`lifecycle_child::bounded_claude_stop`), the same primitive
//! `daemon/claude_stop.rs` wraps, so there is one kill mechanism with two
//! callers. Reentry then reads the restamped row and takes bg-resume with the
//! new `--settings` (reentry.rs:635-690) under the same session id.

use crate::claude_ask::JobListing;
use crate::harness::{AsHarness, Harness};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Re-point `entry` onto `route` (`provider/model` or `provider,model`).
/// Returns the restamped row so the caller's reentry plan is built from what
/// the registry now records; `Err` is the refusal line the caller prints.
pub(crate) fn apply(
    home: &crate::paths::AgentsHome,
    entry: &Value,
    route: &str,
) -> Result<Value, String> {
    let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
    let harness = entry
        .get("harness")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| entry.get("provider").and_then(Value::as_str))
        .unwrap_or("");
    if harness.as_harness() != Harness::Claude {
        return Err(format!(
            "resume --route re-enters a claude row on a new route; row {name:?} is {} \
             (only claude takes --route; the row's own lane bills itself)",
            if harness.is_empty() {
                "unmarked"
            } else {
                harness
            }
        ));
    }
    if pane_backed(entry) {
        return Err(format!(
            "resume --route applies to the claude re-entry door; row {name:?} is served \
             through its mux pane; drive it there, or stop the pane and re-spawn it"
        ));
    }
    // The registry key is the FULL session id, never the transport key:
    // resume_session_id answers a claude row's 8-hex short_id first, and
    // persist_overwrite matches full ids only (the same match persist uses).
    let session_id = entry
        .get("harness_session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .get("claude_session_uuid")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Err(format!(
            "resume --route: row {name:?} records no session id; nothing to re-route"
        ));
    }
    // A row that recorded no launch account is attributed from the config
    // root holding its transcript, so the route composes under the account
    // that wrote it and the restamp below closes the gap for later resumes.
    let derived_account = recorded_account(entry)
        .is_empty()
        .then(|| crate::claude_roster::account_holding_transcript(&session_id))
        .flatten();
    let mut attributed = entry.clone();
    if let (Some(account), Some(map)) = (&derived_account, attributed.as_object_mut()) {
        map.insert("launch_account".into(), Value::String(account.clone()));
    }
    let entry = &attributed;
    let cwd = Path::new(entry.get("cwd").and_then(Value::as_str).unwrap_or(""));

    // The route file first: a recorded file for this provider and model is
    // the exact artifact the previous spawn wrote, so it outranks a fresh
    // resolve. Only a route no file serves resolves through spawn's own
    // doors - and that resolution refuses before anything is stopped.
    let (provider, model) = match route.replace(',', "/").split_once('/') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (route.to_string(), String::new()),
    };
    let route_path: String = match crate::route_recovery::route_file_for(&provider, &model) {
        Some(path) => {
            // The reuse path hands back a file another session may own (a
            // digest or a twin named by its writer's session), so the
            // rerouting row publishes its own twin beside it and records a
            // path that names THIS session; the mint's join, reused.
            match crate::route_settings::publish_session_entry(&path, &session_id) {
                Ok(twin) => twin.to_string_lossy().into_owned(),
                Err(e) => {
                    eprintln!("resume --route: session twin: {e}");
                    path.to_string_lossy().into_owned()
                }
            }
        }
        None => {
            let target =
                crate::spawn_route::resolve_target(route, cwd, &|k: &str| std::env::var(k).ok())
                    .map_err(|line| format!("resume --route: {line}"))?;
            compose_route_file(&target, entry, &session_id, cwd)?
        }
    };

    // The config dir never moves. CLAUDE_CONFIG_DIR selects which config
    // claude reads, so it rides the spawn env and no settings file can
    // change it; a transcript born under another dir would come back under
    // a config that cannot see it.
    let overlay_dir = row_config_dir(entry, cwd);
    if let Some(line) = config_dir_conflict(&session_id, &overlay_dir) {
        return Err(line);
    }

    // The restamp writes by session id, so a row must be in the registry
    // before anything stops: a vanish at the write would leave a stopped
    // session behind a refusal.
    let registry = crate::state::load_registry(&home.registry_json())
        .map_err(|e| format!("resume --route: registry read failed: {e}"))?;
    if !registry
        .entries
        .iter()
        .any(|e| crate::route_recovery::row_matches_session(e, &session_id))
    {
        let names: Vec<String> = registry
            .entries
            .iter()
            .map(|e| e.name.clone())
            .take(8)
            .collect();
        return Err(format!(
            "resume --route: no registry row answers session {session_id}; registry {} \
             holds {} row(s) ({}); adopt it first (fno agent adopt)",
            home.registry_json().display(),
            registry.entries.len(),
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        ));
    }

    clear_saved_job(home, entry, &session_id, cwd)?;

    crate::route_recovery::persist_overwrite(
        &home.registry_json(),
        &session_id,
        &provider,
        &model,
        &route_path,
        derived_account.as_deref(),
    )
    .map_err(|e| format!("resume --route: row restamp failed: {e}"))?;

    let registry = crate::state::load_registry(&home.registry_json())
        .map_err(|e| format!("resume --route: registry readback failed: {e}"))?;
    let updated = registry
        .entries
        .iter()
        .find(|e| crate::route_recovery::row_matches_session(e, &session_id))
        .ok_or_else(|| {
            "resume --route: restamped row vanished from the registry; re-run by name".to_string()
        })?;
    serde_json::to_value(updated)
        .map_err(|e| format!("resume --route: row serialization failed: {e}"))
}

/// The launch account the row records; empty when it records none.
fn recorded_account(entry: &Value) -> &str {
    entry
        .get("launch_account")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The same mux-ref predicate `run_resume` uses for `has_mux_ref`, so the
/// two doors cannot disagree about which rows take the re-entry path.
fn pane_backed(entry: &Value) -> bool {
    entry.get("mux").is_some_and(|mux| {
        mux.get("session")
            .and_then(Value::as_str)
            .is_some_and(|session| !session.is_empty())
            && mux.get("pane_id").and_then(Value::as_u64).is_some()
    })
}

/// Write a fresh settings file for `target` under the row's own account
/// overlay, the same composition spawn's thread seam runs. `session` is the
/// row's harness session id, so the fresh file lands as
/// `session-<id>.json` and the restamped row records the join by eye.
fn compose_route_file(
    target: &crate::spawn_route::Target,
    entry: &Value,
    session: &str,
    cwd: &Path,
) -> Result<String, String> {
    let route_env: serde_json::Map<String, Value> = target
        .env
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let account_env = account_overlay_env(entry, cwd)?;
    let account_map: serde_json::Map<String, Value> = account_env
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    match crate::route_settings::write_composed(
        &route_env,
        &account_map,
        None,
        None,
        (!session.is_empty()).then_some(session),
        cwd,
        None,
    ) {
        Ok(Some(path)) => Ok(path),
        Ok(None) => Err("resume --route: the route wrote no settings file".to_string()),
        Err(e) => Err(format!("resume --route: {e}")),
    }
}

/// The row's account overlay for a composition; the shared slot composes
/// with no account env, the way an unrouted spawn does.
fn account_overlay_env(entry: &Value, cwd: &Path) -> Result<BTreeMap<String, String>, String> {
    let account = recorded_account(entry);
    if account.is_empty() || account == "default" {
        return Ok(BTreeMap::new());
    }
    let overlay = crate::accounts::env::overlay(
        account,
        None,
        None,
        cwd,
        &crate::accounts::env::Sources::system(),
    )
    .map_err(|e| format!("resume --route: launch account {account:?} does not resolve: {e}"))?;
    Ok(overlay.env)
}

/// The config dir the relaunched process runs under: the row account's
/// overlay when it carries one, else the shared slot.
fn row_config_dir(entry: &Value, cwd: &Path) -> PathBuf {
    let account = recorded_account(entry);
    if account.is_empty() || account == "default" {
        return shared_config_dir();
    }
    crate::accounts::env::overlay(
        account,
        None,
        None,
        cwd,
        &crate::accounts::env::Sources::system(),
    )
    .ok()
    .and_then(|o| {
        o.env
            .get("CLAUDE_CONFIG_DIR")
            // A clear marker is no dir: the managed account rides the shared
            // slot, and PathBuf::from("") would refuse every resume.
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
    })
    .unwrap_or_else(shared_config_dir)
}

fn shared_config_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude")
}

/// The config-dir move refusal: a session whose transcript lives under a
/// root other than `overlay_dir` would relaunch under a config that cannot
/// see its history. `None` when no transcript answers or the roots agree.
fn config_dir_conflict(session_id: &str, overlay_dir: &Path) -> Option<String> {
    let holder = [shared_config_dir()]
        .into_iter()
        .chain(
            crate::claude_roster::isolated_account_dirs()
                .into_iter()
                .map(|(_, dir)| dir),
        )
        .find(|root| {
            crate::claude_drive::find_transcript_in(&root.join("projects"), session_id).is_some()
        })?;
    let (holder, overlay_dir) = match (
        std::fs::canonicalize(&holder),
        std::fs::canonicalize(overlay_dir),
    ) {
        (Ok(h), Ok(o)) => (h, o),
        _ => (holder, overlay_dir.to_path_buf()),
    };
    if holder == overlay_dir {
        return None;
    }
    Some(format!(
        "resume --route: session {session_id} keeps its transcript under {}, but the \
         account reads CLAUDE_CONFIG_DIR {}; a live re-route keeps the account's config \
         dir (CLAUDE_CONFIG_DIR selects which config claude reads, so it rides the spawn \
         env and no settings file can change it). Re-spawn under the other account.",
        holder.display(),
        overlay_dir.display()
    ))
}

/// Clear the row's claude job before the relaunch. A listed job keeps its
/// saved launch options, so `claude --bg --resume <id> --settings <new>`
/// answers with a COPY under a new id whether the job runs, sits blocked or
/// is stopped (measured on claude 2.1.325: "keeps its own saved options, so
/// the flags you passed started a copy"). A running job stops first, then
/// `claude rm` takes it off the list. rm keeps the conversation, so the
/// bg-resume continues the same session id on the new route.
fn clear_saved_job(
    home: &crate::paths::AgentsHome,
    entry: &Value,
    session_id: &str,
    cwd: &Path,
) -> Result<(), String> {
    let registry = crate::state::load_registry(&home.registry_json())
        .map_err(|e| format!("resume --route: registry read failed: {e}"))?;
    let typed = registry
        .entries
        .iter()
        .find(|e| crate::route_recovery::row_matches_session(e, session_id));
    let short = typed
        .and_then(|row| row.transport_short())
        .map(str::to_string)
        .or_else(|| {
            entry
                .get("short_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| crate::client_commands::derived_short_id(session_id));
    let account = recorded_account(entry);
    let job_root =
        (!account.is_empty() && account != "default").then(|| row_config_dir(entry, cwd));
    let listing = crate::claude_ask::ClaudeHome::from_env().listed_job(&short, job_root.as_deref());
    if listing == JobListing::Unread {
        eprintln!(
            "fno agent resume: warning: `claude agents` could not be read, so job {short} \
             is not cleared; a listed job comes back as a refused copy"
        );
    }
    let (stop, remove) = job_clearing(
        &listing,
        crate::census::live_claude_holds_session(session_id),
    );
    if stop {
        if let Some(row) = typed {
            if let Some(why) = crate::attended::stop_refusal(home, row) {
                return Err(format!(
                    "resume --route: row {} is attended ({why}); a session the user typed \
                     into is not stopped",
                    row.name
                ));
            }
        }
        // A stop that exits nonzero is judged by what still holds the
        // session below, not by its exit code.
        run_job_action("stop", &short, job_root.as_deref())
            .map_err(|e| format!("resume --route: stopping job {short} failed: {e}"))?;
        // The argv census misses a job its supervisor hosts, so a listed
        // job also answers from the roster.
        let still_listed_running = remove
            && crate::claude_ask::ClaudeHome::from_env()
                .listed_job(&short, job_root.as_deref())
                .is_running();
        if still_listed_running || crate::census::live_claude_holds_session(session_id) {
            return Err(format!(
                "resume --route: a live process still holds session {session_id} after the \
                 stop; stop it and re-run resume --route"
            ));
        }
    }
    if remove {
        let out = run_job_action("rm", &short, job_root.as_deref())
            .map_err(|e| format!("resume --route: removing job {short} failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "resume --route: removing job {short} exited {}: {}; the saved job would \
                 turn the relaunch into a copy",
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

/// What the re-route does to the row's saved job: (stop, remove). `argv_live`
/// is a live process argv naming the session.
fn job_clearing(listing: &JobListing, argv_live: bool) -> (bool, bool) {
    match listing {
        JobListing::Listed(_) => (argv_live || listing.is_running(), true),
        JobListing::Unlisted | JobListing::Unread => (argv_live, false),
    }
}

/// Run one bounded `claude <action> <short>` under the job's account root.
fn run_job_action(
    action: &str,
    short: &str,
    root: Option<&Path>,
) -> std::io::Result<std::process::Output> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(crate::lifecycle_child::bounded_claude_job_action_in(
            action,
            short,
            std::time::Duration::from_secs(15),
            root,
        ))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out after 15s"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn route_env(model: &str, provider: &str) -> Value {
        json!({
            "env": {
                "ANTHROPIC_MODEL": model,
                "FNO_ROUTE_PROVIDER": provider,
                "ANTHROPIC_BASE_URL": "https://repro.invalid/api/anthropic",
                "ANTHROPIC_AUTH_TOKEN": "secret-token",
            }
        })
    }

    fn seed_route_dir(tag: &str, files: &[(&str, Value)]) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "resume-rr-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        for (i, (name, payload)) in files.iter().enumerate() {
            std::fs::write(
                base.join(format!("{i:04}-{name}.json")),
                payload.to_string(),
            )
            .unwrap();
        }
        base
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "resume-rr-home-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_row(reg_path: &Path, sid: &str, route_path: &str, cwd: &Path) -> Value {
        crate::state::update_registry(reg_path, |reg| {
            reg.entries.push(crate::state::RegistryEntry {
                name: "rr-test".into(),
                harness: Some("claude".into()),
                harness_session_id: Some(sid.into()),
                claude_session_uuid: Some(sid.into()),
                short_id: "77770021".into(),
                provider: Some("anthropic".into()),
                requested_model: Some("claude-haiku-5-5".into()),
                route_settings_path: Some(route_path.to_string()),
                launch_account: Some("default".into()),
                cwd: cwd.to_string_lossy().into_owned(),
                ..Default::default()
            });
        })
        .unwrap();
        let registry = crate::state::load_registry(reg_path).unwrap();
        serde_json::to_value(&registry.entries[0]).unwrap()
    }

    #[test]
    fn reroute_restamps_the_row_and_reentry_carries_the_new_route() {
        // AC3-HP: a stopped claude row on route A and a recorded file for
        // route B; apply restamps the row, and the reentry plan takes
        // bg-resume with route B under the same session id.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = seed_route_dir(
            "ac3",
            &[
                ("old", route_env("claude-haiku-5-5", "anthropic")),
                ("new", route_env("glm-5.3-flash[1m]", "zai")),
            ],
        );
        let file = |name: &str| base.join(name).to_string_lossy().into_owned();
        let old = file("0000-old.json");
        let new = file("0001-new.json");
        std::env::set_var("FNO_ROUTE_SETTINGS_DIR", &base);
        let home_dir = temp_home("ac3");
        let saved_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home_dir);
        let saved_agents_home = std::env::var_os("FNO_AGENT_HOME");
        std::env::set_var("FNO_AGENT_HOME", &home_dir);
        let reg_path = home_dir.join("registry.json");
        let sid = "77770021-2222-3333-4444-555555555555";
        let entry = seed_row(&reg_path, sid, &old, &home_dir.as_path());
        let home = crate::paths::AgentsHome::from_env();

        let updated =
            apply(&home, &entry, "zai/glm-5.3-flash[1m]").expect("a recorded route file answers");
        std::env::remove_var("FNO_ROUTE_SETTINGS_DIR");
        assert_eq!(updated.get("provider").and_then(Value::as_str), Some("zai"));
        assert_eq!(
            updated.get("requested_model").and_then(Value::as_str),
            Some("glm-5.3-flash[1m]")
        );
        // The reused route-B file belongs to no session; the row records its
        // own twin, carrying route B's bytes.
        let twin = file(&format!("session-{sid}.json"));
        assert_eq!(
            updated.get("route_settings_path").and_then(Value::as_str),
            Some(twin.as_str())
        );
        assert_eq!(std::fs::read(&twin).unwrap(), std::fs::read(&new).unwrap());

        let plan = crate::reentry::resolve_reentry(
            &reg_path,
            "rr-test",
            crate::reentry::ReentryTransition::Resume,
            None,
            None,
        )
        .expect("the restamped row resolves");
        match &saved_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match &saved_agents_home {
            Some(h) => std::env::set_var("FNO_AGENT_HOME", h),
            None => std::env::remove_var("FNO_AGENT_HOME"),
        }
        assert_eq!(plan.mechanism, "bg-resume");
        for token in ["--bg", "--resume", sid, "--settings", twin.as_str()] {
            assert!(
                plan.argv.iter().any(|a| a == token),
                "{token} missing from {:?}",
                plan.argv
            );
        }
        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&home_dir).ok();
    }

    #[test]
    fn a_listed_job_is_cleared_so_the_new_route_continues_the_same_session() {
        // A listed claude job answers a flagged bg-resume with a COPY in any
        // state, so the re-route removes it; a job still running (a 429
        // stall reads blocked) stops first. An unlisted session stops only
        // when a live argv holds it, and an unread listing clears nothing.
        let listed = |state: &str| JobListing::Listed(Some(state.to_string()));
        assert_eq!(job_clearing(&listed("blocked"), false), (true, true));
        assert_eq!(job_clearing(&listed("working"), false), (true, true));
        assert_eq!(job_clearing(&listed("stopped"), false), (false, true));
        assert_eq!(job_clearing(&JobListing::Unlisted, true), (true, false));
        assert_eq!(job_clearing(&JobListing::Unlisted, false), (false, false));
        assert_eq!(job_clearing(&JobListing::Unread, false), (false, false));
    }

    #[test]
    fn config_dir_conflict_names_the_dir_and_lets_matching_roots_pass() {
        // AC4-ERR: the transcript lives under a different config dir than the
        // account reads; the refusal names CLAUDE_CONFIG_DIR. Same dir: none.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home_dir = temp_home("ac4");
        let saved_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home_dir);
        let slug = home_dir.join(".claude").join("projects").join("slug-proj");
        std::fs::create_dir_all(&slug).unwrap();
        let sid = "77770022-2222-3333-4444-555555555555";
        std::fs::write(slug.join(format!("{sid}.jsonl")), "{}").unwrap();
        let shared = shared_config_dir();
        let elsewhere = home_dir.join("iso-config");

        let refusal = config_dir_conflict(sid, &elsewhere)
            .expect("a transcript under the shared root conflicts with another dir");
        assert!(refusal.contains("CLAUDE_CONFIG_DIR"), "{refusal}");
        assert!(config_dir_conflict(sid, &shared).is_none());
        match &saved_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&home_dir).ok();
    }

    #[test]
    fn reroute_refuses_an_unresolvable_route_before_touching_anything() {
        // AC5-ERR: a route no recorded file serves and no lane resolves
        // refuses naming the route, and the row keeps its recorded route.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = seed_route_dir(
            "ac5",
            &[("old", route_env("claude-haiku-5-5", "anthropic"))],
        );
        let old = base.join("0000-old.json").to_string_lossy().into_owned();
        std::env::set_var("FNO_ROUTE_SETTINGS_DIR", &base);
        let home_dir = temp_home("ac5");
        let saved_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home_dir);
        let saved_agents_home = std::env::var_os("FNO_AGENT_HOME");
        std::env::set_var("FNO_AGENT_HOME", &home_dir);
        let reg_path = home_dir.join("registry.json");
        let sid = "77770023-2222-3333-4444-555555555555";
        let entry = seed_row(&reg_path, sid, &old, &home_dir.as_path());
        let home = crate::paths::AgentsHome::from_env();

        let err = apply(&home, &entry, "no-such-lane-x/glm-9")
            .expect_err("an unresolvable route refuses");
        match &saved_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match &saved_agents_home {
            Some(h) => std::env::set_var("FNO_AGENT_HOME", h),
            None => std::env::remove_var("FNO_AGENT_HOME"),
        }
        std::env::remove_var("FNO_ROUTE_SETTINGS_DIR");
        assert!(err.contains("no-such-lane-x"), "{err}");
        let loaded = crate::state::load_registry(&reg_path).unwrap();
        let row = &loaded.entries[0];
        assert_eq!(row.provider.as_deref(), Some("anthropic"));
        assert_eq!(row.route_settings_path.as_deref(), Some(old.as_str()));
        assert_eq!(row.requested_model.as_deref(), Some("claude-haiku-5-5"));
        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&home_dir).ok();
    }
}
