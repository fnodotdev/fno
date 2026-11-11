//! The client's promote door. A `--promote` spawn resolves its team, checks
//! the grantor's authority, plans occupancy and seeds the lead turn before
//! launch, on every lane, through the same kinds the Python door calls:
//! `role-resolve`, `role-grant`, `role-settle` and `spawn-team`. After a
//! launch, the `spawn-team` settle op stamps the team under the registry
//! lock. The door decides nothing new.

use crate::claims::CanonicalDisposition;
use crate::paths::AgentsHome;
use crate::spawn_context::OwnedDisposition;
use serde_json::{json, Value};

/// The Python door's one-shot refusal (cli.py), byte for byte.
pub const ONE_SHOT_REFUSAL: &str = "--promote needs a session that outlives the grant; \
     headless is a one-shot that exits after one answer, so its role would be orphaned \
     at birth. Use --as pane or --as thread.";

/// A planned promotion: the settle payload and the gate's succession facts.
#[derive(Debug, Clone)]
pub struct Promotion {
    ask: Value,
    scope: String,
    hand_off: bool,
    caller_session: Option<String>,
}

/// The calling session, read the way Python `calling_agent_row` read it: an
/// identity fno cannot place is unreadable, an identity with no row is
/// unregistered, and no identity at all is human ONLY with the positive
/// attended proof (ancestry + tty); without it the caller is unproven. A
/// harness-resolved caller whose session transcript proves the composer `!`
/// path also reads as the human (law d-c26f392e).
#[derive(Debug)]
pub(crate) enum Calling {
    Human,
    Unproven(&'static str),
    Unreadable(Option<String>),
    Unregistered(String),
    Agent(Box<crate::state::RegistryEntry>, String),
}

pub(crate) fn calling_row(home: &AgentsHome) -> Calling {
    let get = |k: &str| std::env::var(k).ok();
    let ident = crate::spawn_context::resolve_self_identity(&get, None, None, home);
    let stamp = crate::spawn_context::parse_canonical_stamp(&get).2;
    let session = ident.session_id.clone().filter(|s| !s.is_empty());
    let placed = !matches!(
        ident.disposition,
        OwnedDisposition::Ambiguous | OwnedDisposition::Invalid | OwnedDisposition::Contradiction
    ) && !(session.is_none()
        && matches!(
            stamp,
            CanonicalDisposition::NameOnly | CanonicalDisposition::Complete
        ));
    if !placed {
        return Calling::Unreadable(session);
    }
    let (Some(sid), Some(harness)) = (session, ident.harness.clone()) else {
        let stray = get("FNO_AGENT_SELF").is_some_and(|v| !v.trim().is_empty());
        return if stray {
            Calling::Unreadable(None)
        } else {
            match crate::claims::attended_human_proof() {
                Ok(()) => Calling::Human,
                Err(reason) => Calling::Unproven(reason),
            }
        };
    };
    match crate::state::load_registry(&home.registry_json()) {
        Err(_) => Calling::Unreadable(Some(sid)),
        Ok(registry) => match registry.find_by_session(&harness, &sid) {
            None => {
                if composer_grant_proof(&harness, &sid, &grant_argv_tail()) {
                    Calling::Human
                } else {
                    Calling::Unregistered(sid)
                }
            }
            Some(row) => {
                if composer_grant_proof(&harness, &sid, &grant_argv_tail()) {
                    Calling::Human
                } else {
                    Calling::Agent(Box::new(row.clone()), sid)
                }
            }
        },
    }
}

/// The argv tail the composer proof matches against. Under test the cargo
/// binary's own argv names nothing grantable, so tests pin the tail the same
/// way they pin the census and the tty.
#[cfg(test)]
static TEST_GRANT_ARGV: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_grant_argv(tail: Option<Vec<String>>) {
    *TEST_GRANT_ARGV.lock().unwrap_or_else(|e| e.into_inner()) = tail;
}

fn grant_argv_tail() -> Vec<String> {
    #[cfg(test)]
    let pinned = TEST_GRANT_ARGV
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    #[cfg(test)]
    if let Some(tail) = pinned {
        return tail;
    }
    std::env::args().skip(1).collect()
}

/// A role grant typed as a composer `!` command reads as the
/// human grantor only when the session transcript proves the composer path:
/// the transcript's newest real user row (skipping the echo rows a `!` run
/// writes after itself) is a `<bash-input>` naming this exact command.
/// Measured 2026-11-07 over 400 machine transcripts: the input row lands at
/// submit time, so the child reads its own row.
fn composer_grant_proof(harness: &str, sid: &str, argv_tail: &[String]) -> bool {
    use crate::provenance::{is_user_turn, turn_text};
    if argv_tail.is_empty() {
        return false;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let roots = crate::transcript_resolve::Roots::default();
    let found =
        crate::transcript_resolve::resolve_transcript(harness, sid, &cwd.to_string_lossy(), &roots);
    let Some(path) = found.path else {
        return false;
    };
    let ok = std::fs::File::open(&path).ok().and_then(|mut file| {
        let size = file.metadata().ok()?.len();
        let start = size.saturating_sub(256 * 1024);
        use std::io::{Read, Seek, SeekFrom};
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf);
        // A tail read that clipped mid-row drops the torn first line.
        let body = if start > 0 {
            match text.find('\n') {
                Some(i) => &text[i + 1..],
                None => return None,
            }
        } else {
            &text[..]
        };
        for line in body.lines().rev() {
            let Ok(obj) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if !is_user_turn(&obj) {
                continue;
            }
            let raw = turn_text(&obj);
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            if let Some(inner) = raw
                .strip_prefix("<bash-input>")
                .and_then(|s| s.strip_suffix("</bash-input>"))
            {
                return Some(
                    composer_row_fresh(&obj) && composer_command_matches(inner, argv_tail),
                );
            }
            if raw.starts_with("<bash-stdout>") || raw.starts_with("<bash-stderr>") {
                continue;
            }
            // Any other real user row (prose, a slash command, an interrupt)
            // decides: the grant is no longer the newest operator act.
            return Some(false);
        }
        None
    });
    ok.unwrap_or(false)
}

/// The typed `!` line names THIS command: after an optional leading `fno`
/// (the composer input says `fno agents ...`; the process argv tail is what
/// follows the binary), whitespace-normalized token equality with argv[1..].
fn composer_command_matches(inner: &str, argv_tail: &[String]) -> bool {
    let mut tokens: Vec<String> = inner.split_whitespace().map(str::to_string).collect();
    if let Some(first) = tokens.first() {
        if first == "fno" || first.ends_with("/fno") || first == "fno-agents" {
            tokens.remove(0);
        }
    }
    tokens.as_slice() == argv_tail
}

/// How long after typing the `!` child may still read its own row: the proof
/// binds the grant to the operator's immediate action, not to a command the
/// session once saw. 10 minutes absorbs a slow machine and a queued start.
const COMPOSER_PROOF_WINDOW_S: f64 = 600.0;

/// A stamped row older than the window no longer proves the composer path:
/// row order alone would let any later run of the same command in this
/// session inherit a grant typed once. An unstamped row keeps the
/// order-only semantics, so a harness that cannot stamp still proves.
fn composer_row_fresh(obj: &Value) -> bool {
    match crate::provenance::turn_ts_epoch(obj) {
        None => true,
        Some(ts) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            now - ts <= COMPOSER_PROOF_WINDOW_S
        }
    }
}

/// A grant refusal's words for the row a caller resolved to.
pub(crate) fn row_identity(row: &crate::state::RegistryEntry, sid: &str) -> String {
    format!(
        "registry row {} ({} session {})",
        row.name,
        row.harness.as_deref().unwrap_or("unknown"),
        sid.get(..8).unwrap_or(sid)
    )
}

/// Add the facts a grant refusal names: the resolved identity, and the `env -u`
/// proof-gated prefix that drops each identity name this shell carries.
pub(crate) fn with_identity(mut caller: Value, identity: String) -> Value {
    // The `env -u` strip is a remedy only where stripping can actually work:
    // under a harness ancestor the proof refuses the caller regardless of its
    // markers, so the refusal names the separate terminal instead (the
    // empty-strip arm of territory's as_user wording).
    let strip: String = if crate::claims::attended_human_proof().is_err() {
        String::new()
    } else {
        crate::claims::AMBIENT_IDENTITY_NAMES
            .iter()
            .filter(|name| std::env::var(name).is_ok_and(|v| !v.trim().is_empty()))
            .map(|name| format!(" -u {name}"))
            .collect()
    };
    caller["identity"] = json!(identity);
    caller["user_strip"] = json!(if strip.is_empty() {
        String::new()
    } else {
        format!("env{strip} ")
    });
    caller
}

/// An agent caller as the grant check reads it. `spawn --promote` and
/// `org promote` both send this shape, so neither can drop a field the check needs.
pub(crate) fn agent_caller(entry: &crate::state::RegistryEntry, sid: &str) -> Value {
    let row_json = serde_json::to_value(entry).unwrap_or(Value::Null);
    let mut out = json!({"kind": "agent"});
    for key in ["name", "status", "role_scope", "role_level"] {
        out[key] = row_json[key].clone();
    }
    with_identity(out, row_identity(entry, sid))
}

/// The calling session as the grant check reads it, and its session id.
fn caller(home: &AgentsHome) -> (Value, Option<String>) {
    match calling_row(home) {
        Calling::Human => (json!({"kind": "human"}), None),
        Calling::Unproven(reason) => (json!({"kind": "unproven", "reason": reason}), None),
        Calling::Unreadable(sid) => (json!({"kind": "unreadable"}), sid),
        Calling::Unregistered(sid) => (
            with_identity(json!({"kind": "unregistered"}), format!("session {sid}")),
            Some(sid),
        ),
        Calling::Agent(entry, sid) => (agent_caller(&entry, &sid), Some(sid)),
    }
}

fn refuse(text: &str) -> i32 {
    eprintln!("--promote: {text}");
    2
}

/// The raw `--role-level`/`--role-scope` pair's gate: the unguarded twin of
/// the promote road. A complete pair widens the pane posture, the codex
/// sandbox and the minted thread row, and the mint trusts the caller seam
/// to own the territory policy. `plan` runs that check for `--promote`;
/// this gate runs the same check for the raw pair, succession excluded
/// (succession is the settle's write, never a raw spawn's). `Ok(())` when
/// there is no pair to gate.
pub fn raw_role_pair_gate(params: &Value) -> Result<(), i32> {
    if !raw_role_pair_applies(params) {
        return Ok(());
    }
    let home = AgentsHome::from_env();
    let calling = calling_row(&home);
    let cwd = std::env::current_dir().unwrap_or_default();
    let entries = crate::territory::graph_entries(&cwd).unwrap_or_default();
    let projects = crate::org_board::project_map(&cwd).unwrap_or_default();
    if let Some(refusal) = raw_role_pair_checked(&calling, params, &entries, &projects) {
        return Err(gate_refuse(&refusal));
    }
    Ok(())
}

/// The gate's applicability: a complete raw pair with no promote plan. A
/// lone half is refused downstream today; a promote plan runs its own check
/// and overwrites both keys from the resolved scope, so this gate must not
/// re-fire over it.
fn raw_role_pair_applies(params: &Value) -> bool {
    !params["role_level"].is_null()
        && params["role_scope"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
        && params["promote"]
            .as_array()
            .is_none_or(|scopes| scopes.is_empty())
}

/// The caller-resolved core the tests drive with a constructed `Calling`:
/// the level-0 arm first, then the same grant check the promote door runs.
pub(crate) fn raw_role_pair_checked(
    calling: &Calling,
    params: &Value,
    entries: &[Value],
    projects: &std::collections::HashMap<String, String>,
) -> Option<String> {
    if !raw_role_pair_applies(params) {
        return None;
    }
    // A raw --role-level 0 mints a superuser grantor (territory passes a
    // stored 0 over any scope). The promote road derives the level from the
    // scope kind; the raw pair is the one caller-chosen level, so this arm
    // is its only bound: the user at a terminal or a level 0 lead stamps
    // one; a sub-level agent row cannot, however contained its scope.
    if params["role_level"].as_u64() == Some(0) {
        let super_level = match calling {
            Calling::Human => true,
            Calling::Agent(row, _) => row.role_level == Some(0),
            _ => false,
        };
        if !super_level {
            return Some(match calling {
                Calling::Agent(_, _) => format!(
                    "a raw --role-level 0 stamps a level 0 grantor row, and a level 0 \
                     row grants over any scope; only the user at a terminal or a level \
                     0 lead can stamp one. Ask your lead to grant it, or run \
                     `fno agents spawn --promote <scope>` for the derived level."
                ),
                _ => format!(
                    "a raw --role-level 0 stamps a level 0 grantor row, and a level 0 \
                     row grants over any scope; only the user at a terminal or a level \
                     0 lead can stamp one. If you are the user, open a terminal \
                     outside any harness session and rerun."
                ),
            });
        }
    }
    let scope = params["role_scope"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let caller = crate::role_grant::caller_payload(calling);
    let stray = match calling {
        Calling::Unreadable(None) => std::env::var("FNO_AGENT_SELF")
            .unwrap_or_default()
            .trim()
            .to_string(),
        _ => String::new(),
    };
    let check = match crate::territory::role_grant_kind(&json!({
        "op": "check", "requested": scope, "caller": caller,
        "allow_terminal_recovery": false, "allow_succession": false,
        "stray_agent_self": stray, "projects": projects, "entries": entries,
    })) {
        Ok(check) => check,
        Err(e) => {
            return Some(format!(
                "cannot verify the grantor's authority: the grant check did not answer ({e})"
            ))
        }
    };
    check
        .pointer("/refusal")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn gate_refuse(text: &str) -> i32 {
    eprintln!("fno agents spawn: {text}");
    2
}

/// Plan a `--promote` spawn before launch. Rewrites `params` (the seeded
/// message, the drawn name, the role the codex lane stamps at mint) and
/// `name`; a refusal prints the Python door's text and answers exit 2.
pub fn plan(
    params: &mut Value,
    name: &mut String,
    substrate: &str,
) -> Result<Option<Promotion>, i32> {
    let scopes: Vec<String> = params["promote"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if scopes.is_empty() {
        return Ok(None);
    }
    if matches!(substrate, "headless" | "cloud") {
        eprintln!("{ONE_SHOT_REFUSAL}");
        return Err(2);
    }
    // One graph and project-map read serves the resolve and the grant check.
    // An empty graph or an unreadable map is left for the resolve to read and
    // name itself.
    let cwd = std::env::current_dir().unwrap_or_default();
    let entries = crate::territory::graph_entries(&cwd).unwrap_or_default();
    let projects = crate::org_board::project_map(&cwd);
    let mut ask = json!({"scopes": scopes});
    if !entries.is_empty() {
        ask["entries"] = json!(entries);
    }
    if let Ok(map) = &projects {
        ask["projects"] = json!(map);
    }
    let resolved = crate::territory::resolve_role_kind(&ask).map_err(|e| refuse(&e))?;
    if let Some(refusal) = resolved["refusal"].as_str() {
        return Err(refuse(refusal));
    }
    let (level, scope) = (
        resolved["level"].clone(),
        resolved["scope"].as_str().unwrap_or("").to_string(),
    );
    let hand_off = params["hand_off"] == true;
    let home = AgentsHome::from_env();
    let (caller, caller_session) = caller(&home);
    let stray = if caller["kind"] == "unreadable" && caller_session.is_none() {
        std::env::var("FNO_AGENT_SELF")
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        String::new()
    };
    let check = crate::territory::role_grant_kind(&json!({
        "op": "check", "requested": scope, "caller": caller,
        "allow_terminal_recovery": true, "allow_succession": hand_off, "stray_agent_self": stray,
        "projects": projects.unwrap_or_default(), "entries": entries,
    }))
    .map_err(|e| {
        refuse(&format!(
            "cannot verify the grantor's authority: the grant check did not answer ({e})"
        ))
    })?;
    if let Some(refusal) = check["refusal"].as_str() {
        return Err(refuse(refusal));
    }
    let rows = crate::state::load_registry(&home.registry_json()).map_err(|e| {
        refuse(&format!(
            "cannot decide role occupancy: the registry could not be read ({e})"
        ))
    })?;
    let redraw = name.trim().eq_ignore_ascii_case("redraw");
    let caller_name = caller["name"].as_str().map(str::to_string);
    let mut settle = json!({
        "kind": "role-settle", "scope": scope, "succession": hand_off,
        "proposed_name": if redraw { Value::Null } else { json!(name) }, "redraw": redraw,
        "caller": match &caller_name { Some(n) => json!({"kind": "agent", "name": n}), None => json!({"kind": "human"}) },
        "exclude_name": Value::Null,
        "rows": serde_json::to_value(&rows.entries).unwrap_or_default(),
    });
    if let Some(reason) = params["take_over"].as_str() {
        settle["take_over"] = json!({"reason": reason});
    }
    let occupancy = crate::team_settle::resolve(&settle)
        .map_err(|e| refuse(&format!("cannot decide role occupancy: {e}")))?;
    if let Some(refusal) = occupancy["refusal"].as_str() {
        return Err(refuse(refusal));
    }
    // The settle draws on a hand-off, and on a fresh grant's `--name redraw`.
    if let Some(drawn) = occupancy["draw"]["name"].as_str().filter(|n| !n.is_empty()) {
        *name = drawn.to_string();
        params["name"] = json!(drawn);
    }
    let harness = params["provider"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            crate::provider::infer_dispatch_provider(|k| std::env::var(k).ok()).to_string()
        });
    let seeded = crate::team_spawn::resolve(&json!({
        "op": "seed", "message": params["message"].as_str().unwrap_or(""),
        "level": level, "scope": scope, "revive": false, "harness": harness,
    }))
    .map_err(|e| refuse(&e))?;
    params["message"] = seeded["message"].clone();
    params["role_level"] = level.clone();
    params["role_scope"] = json!(scope);
    Ok(Some(Promotion {
        ask: json!({"plan": occupancy, "caller": caller_name, "level": level, "scope": scope,
            "typed": seeded["typed"], "harness": harness}),
        scope,
        hand_off,
        caller_session,
    }))
}

/// The gate input a promoted spawn carries: the caller session, and the
/// succession scope when it hands its team off.
pub fn gate_input(promotion: Option<&Promotion>) -> crate::spawn_gate::GateInput {
    crate::spawn_gate::GateInput {
        caller_session: promotion.and_then(|p| p.caller_session.clone()),
        succession_scope: promotion.filter(|p| p.hand_off).map(|p| p.scope.clone()),
        ..Default::default()
    }
}

/// Settle a launched promoted spawn with the `spawn-team` settle op, against
/// the row the lane wrote. A failed launch settles nothing. A failure after
/// launch prints the remedy and keeps the exit: the worker runs.
pub fn settle(promotion: Option<&Promotion>, exit: i32, name: &str) -> i32 {
    let Some(promotion) = promotion.filter(|_| exit == 0) else {
        return exit;
    };
    let home = AgentsHome::from_env();
    let row = crate::state::load_registry(&home.registry_json())
        .ok()
        .and_then(|r| r.find(name).cloned());
    let mut ask = promotion.ask.clone();
    ask["op"] = json!("settle");
    ask["name"] = json!(name);
    if let Some(row) = &row {
        let session = [
            row.harness_session_id.as_deref(),
            row.cc_session_id.as_deref(),
            Some(row.short_id.as_str()),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .unwrap_or("");
        ask["session_id"] = json!(session);
        ask["cwd"] = json!(row.cwd);
        ask["grantor"] = json!(row.spawned_by_session.as_deref().unwrap_or("human"));
    }
    let notices = match row.map(|_| crate::team_spawn::resolve(&ask)) {
        Some(Ok(answer)) => answer["notices"].as_array().cloned().unwrap_or_default(),
        failed => vec![json!(format!(
            "spawn: promotion settlement failed after launch: {}; {} runs without a confirmed \
             role. Grant it with fno agents org promote.",
            match failed {
                Some(Err(e)) => e,
                _ => "no registry row was found".to_string(),
            },
            crate::claude_ask::py_repr(name)
        ))],
    };
    for note in notices.iter().filter_map(Value::as_str) {
        eprintln!("{note}");
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_shot_promote_refuses_before_any_read() {
        for substrate in ["headless", "cloud"] {
            let mut params = json!({"promote": ["x-1"]});
            let mut name = "w".to_string();
            assert_eq!(plan(&mut params, &mut name, substrate).unwrap_err(), 2);
        }
    }

    /// The env a `plan` test isolates: every ambient identity name (the
    /// caller ladder reads them) plus the state roots the plan touches.
    struct PlanEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl PlanEnvGuard {
        fn take() -> Self {
            let lock = crate::claims::test_env_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut names: Vec<&'static str> = crate::claims::AMBIENT_IDENTITY_NAMES.to_vec();
            names.extend([
                "FNO_AGENTS_HOME",
                "FNO_CONFIG",
                "FNO_HOME",
                "FNO_CLAIMS_ROOT",
                "FNO_STATE_DIR",
            ]);
            let saved = names
                .into_iter()
                .map(|var| (var, std::env::var_os(var)))
                .collect();
            Self { _lock: lock, saved }
        }

        fn clear_identity(&self) {
            for (var, _) in &self.saved {
                if crate::claims::AMBIENT_IDENTITY_NAMES.contains(var) {
                    std::env::remove_var(var);
                }
            }
        }
    }

    impl Drop for PlanEnvGuard {
        fn drop(&mut self) {
            for (var, saved) in &self.saved {
                match saved {
                    Some(v) => std::env::set_var(var, v),
                    None => std::env::remove_var(var),
                }
            }
        }
    }

    /// The granted-row staging the caller-seam tests share: a temp agents
    /// home seeded with the granted esther row (level 0 over fnh,fno,
    /// session s-granted), `HOME_ENV` pinned to it. The seed guard and the
    /// temp dir stay alive in the caller's bindings, and the parent pid
    /// seeds the census chain.
    fn granted_home_stage() -> (tempfile::TempDir, crate::claims::EnvVarGuard, u32) {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        crate::registry_store::seed_raw(
            agents.join("registry.json"),
            json!({"schema_version": 4, "agents": [
                {"name": "esther", "status": "live", "role_scope": "fnh,fno", "role_level": 0,
                 "cwd": std::env::current_dir().unwrap().display().to_string(), "harness": "claude",
                 "harness_session_id": "s-granted",
                 "created_at": "2026-11-10T06:21:00Z"}
            ]})
            .to_string(),
        );
        let ppid: u32 = unsafe { libc::getppid() } as u32;
        let home_root = crate::claims::EnvVarGuard::set(
            crate::paths::HOME_ENV,
            agents.display().to_string().as_str(),
        );
        (tmp, home_root, ppid)
    }

    /// The harness-ancestry census the granted-row caller proves: the Bash
    /// tool invocation sits under the lead's claude process, under launchd.
    fn granted_census_stage(ppid: u32) -> crate::claims::proof_tests::CensusGuard {
        crate::claims::proof_tests::CensusGuard::chain(&[
            (
                std::process::id(),
                ppid,
                "fno-agents spawn --role-level 0 --role-scope fnh,fno",
            ),
            (ppid, 1, "claude --resume /t/lead.jsonl --name lead"),
            (1, 0, "launchd"),
        ])
    }

    /// The bare command (`spawn --promote <scope> --hand-off`, no `--name`)
    /// draws the successor's name at plan. The seam mints no name for a
    /// hand-off, so the settle draw is the only name source; an empty one
    /// reached the launch lane as the "agent name must not be empty"
    /// refusal instead of drawing.
    #[test]
    fn a_bare_hand_off_draws_the_successor_name_at_plan() {
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        crate::registry_store::seed_raw(
            &agents.join("registry.json"),
            json!({"schema_version": 4, "agents": [
                {"name": "kara", "status": "live", "role_scope": "alpha", "role_level": 2,
                 "cwd": tmp.path().display().to_string(), "harness": "claude",
                 "harness_session_id": "hand-off-uuid-1",
                 "created_at": "2026-11-06T00:00:00Z"}
            ]})
            .to_string(),
        );
        std::fs::write(
            tmp.path().join("config.toml"),
            "[[work.workspaces.main.projects]]\nname = \"alpha\"\npath = \"/repo/alpha\"\n",
        )
        .unwrap();
        std::env::set_var(crate::paths::HOME_ENV, &agents);
        std::env::set_var("FNO_CONFIG", tmp.path().join("config.toml"));
        std::env::set_var("FNO_HOME", tmp.path());
        std::env::set_var("FNO_CLAIMS_ROOT", tmp.path().join("claims"));
        std::env::set_var("FNO_STATE_DIR", tmp.path().join("state"));
        std::env::set_var("FNO_HARNESS_NAME", "claude");
        std::env::set_var("FNO_HARNESS_SESSION_ID", "hand-off-uuid-1");
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "hand-off-uuid-1");

        // The exact params the client parse yields for the bare argv.
        let mut params = json!({"promote": ["alpha"], "hand_off": true});
        let mut name = String::new();
        let promotion = plan(&mut params, &mut name, "bg");

        match promotion {
            Ok(Some(promotion)) => {
                assert!(
                    crate::team_names::valid_person_name(&name),
                    "the plan drew {name:?}, not a person name"
                );
                assert_eq!(
                    params["name"],
                    json!(name),
                    "the launch params carry the draw"
                );
                assert_eq!(
                    promotion.ask["plan"]["outcome"].as_str(),
                    Some("succeeded"),
                    "a holder handing off its own scope settles as a succession"
                );
            }
            Ok(None) => panic!("the bare hand-off planned nothing: {:?}", params),
            Err(code) => panic!("the bare hand-off refused with exit {code}"),
        }
    }

    /// Pin the census chain the ambient walk reads, starting at this test
    /// process's real ppid, and clear the tty override on drop. The guards
    /// live in `claims::proof_tests` so the two test surfaces share one
    /// census pin and one tty seam.
    #[test]
    fn a_markerless_caller_without_proof_is_not_human() {
        // AC3-ERR: no identity markers, a harness ancestor in the pinned
        // census and no tty. The grant door refuses with the unproven text
        // and grants nothing.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let _pid = crate::claims::EnvVarGuard::unset("FNO_SESSION_PID");
        let _harness = crate::claims::EnvVarGuard::unset("FNO_SESSION_HARNESS");
        let ppid: u32 = unsafe { libc::getppid() } as u32;
        let _table = crate::claims::proof_tests::CensusGuard::chain(&[
            (
                std::process::id(),
                ppid,
                "fno-agents org promote w --scope e-1",
            ),
            (ppid, 1, "claude --resume /t/session.jsonl --name lead"),
            (1, 0, "launchd"),
        ]);
        let _tty = {
            crate::claims::set_test_stdin_tty(Some(false));
            crate::claims::proof_tests::TtyGuard
        };
        let home = crate::paths::AgentsHome::from_env();
        let calling = calling_row(&home);
        let Calling::Unproven(reason) = &calling else {
            panic!("the markerless caller read {calling:?}, not Unproven");
        };
        assert_eq!(*reason, "a harness process is in this call's ancestry");
        let (payload, _sid) = caller(&home);
        let denial = crate::territory::role_grant_kind(&json!({
            "op": "check", "requested": "e-1", "caller": payload,
            "allow_terminal_recovery": false, "allow_succession": true,
            "stray_agent_self": "", "projects": {}, "entries": [],
        }));
        let text = denial
            .unwrap()
            .get("refusal")
            .and_then(Value::as_str)
            .expect("the unproven caller must be refused")
            .to_string();
        assert!(
            text.starts_with("cannot verify the grantor's authority"),
            "{text}"
        );
        assert!(text.contains("not read as an attended human"), "{text}");
        assert!(text.contains("no session identity resolved"), "{text}");
    }

    #[test]
    fn a_proved_markerless_caller_still_reads_human() {
        // AC4-HP: no identity markers, no harness ancestor, tty override set.
        // The same grant reads as human and the door grants as today.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let _pid = crate::claims::EnvVarGuard::unset("FNO_SESSION_PID");
        let _harness = crate::claims::EnvVarGuard::unset("FNO_SESSION_HARNESS");
        let ppid: u32 = unsafe { libc::getppid() } as u32;
        let _table = crate::claims::proof_tests::CensusGuard::chain(&[
            (
                std::process::id(),
                ppid,
                "fno-agents org promote w --scope e-1",
            ),
            (ppid, 1, "-zsh"),
            (1, 0, "launchd"),
        ]);
        let _tty = {
            crate::claims::set_test_stdin_tty(Some(true));
            crate::claims::proof_tests::TtyGuard
        };
        let home = crate::paths::AgentsHome::from_env();
        let calling = calling_row(&home);
        assert!(matches!(calling, Calling::Human));
        let (payload, _sid) = caller(&home);
        let denial = crate::territory::role_grant_kind(&json!({
            "op": "check", "requested": "e-1", "caller": payload,
            "allow_terminal_recovery": false, "allow_succession": true,
            "stray_agent_self": "", "projects": {}, "entries": [],
        }));
        let answer = denial.unwrap();
        assert!(
            answer.get("refusal").is_none_or(Value::is_null),
            "a proved human grants: {:?}",
            answer.get("refusal")
        );
    }

    /// A claude transcript under a temp HOME, newest row last: each entry is
    /// the text of one `type:"user"` row.
    fn write_composer_transcript(home: &std::path::Path, sid: &str, rows: &[serde_json::Value]) {
        let dir = home.join(".claude").join("projects").join("proj-a");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        for row in rows {
            body.push_str(&row.to_string());
            body.push('\n');
        }
        std::fs::write(dir.join(format!("{sid}.jsonl")), body).unwrap();
    }

    fn composer_user_row(text: &str) -> serde_json::Value {
        json!({
            "type": "user",
            "message": {"role": "user", "content": text},
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        })
    }

    #[test]
    fn a_composer_bash_input_grant_reads_as_the_human() {
        // AC5-HP: the newest real user row is a `<bash-input>` naming this
        // command, so calling_row reads the registered harness session as
        // the human grantor (law d-c26f392e).
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        crate::registry_store::seed_raw(
            agents.join("registry.json"),
            json!({"schema_version": 4, "agents": [
                {"name": "carla", "status": "live", "role_scope": "alpha", "role_level": 1,
                 "cwd": tmp.path().display().to_string(), "harness": "claude",
                 "harness_session_id": "composer-sid",
                 "created_at": "2026-11-06T00:00:00Z"}
            ]})
            .to_string(),
        );
        let _home_root = crate::claims::EnvVarGuard::set(
            crate::paths::HOME_ENV,
            agents.display().to_string().as_str(),
        );
        let _home =
            crate::claims::EnvVarGuard::set("HOME", tmp.path().display().to_string().as_str());
        let _harness = crate::claims::EnvVarGuard::set("FNO_HARNESS_NAME", "claude");
        let _sid_var = crate::claims::EnvVarGuard::set("FNO_HARNESS_SESSION_ID", "composer-sid");
        // The identity ladder corroborates a Complete stamp through the
        // attester marker and the ambient walk's stamp pair; a test process
        // has no claude ancestor, so the test stamps both halves itself.
        let _attest = crate::claims::EnvVarGuard::set("CLAUDE_CODE_SESSION_ID", "composer-sid");
        let _walk_pid = crate::claims::EnvVarGuard::set(
            "FNO_SESSION_PID",
            std::process::id().to_string().as_str(),
        );
        let _walk_harness = crate::claims::EnvVarGuard::set("FNO_SESSION_HARNESS", "claude");
        let _argv = set_test_grant_argv(Some(vec![
            "agents".into(),
            "org".into(),
            "promote".into(),
            "w".into(),
            "--scope".into(),
            "e-1".into(),
        ]));
        write_composer_transcript(
            tmp.path(),
            "composer-sid",
            &[
                composer_user_row("status on your nodes?"),
                composer_user_row("<bash-input>fno agents org promote w --scope e-1</bash-input>"),
            ],
        );
        let home = crate::paths::AgentsHome::from_env();
        let calling = calling_row(&home);
        assert!(matches!(calling, Calling::Human), "{calling:?}");
        // The proof binds to the operator's immediate action: the same rows
        // with a stamp outside the window keep the session's Agent answer.
        write_composer_transcript(
            tmp.path(),
            "composer-sid",
            &[composer_user_row("status on your nodes?"), {
                let mut row = composer_user_row(
                    "<bash-input>fno agents org promote w --scope e-1</bash-input>",
                );
                *row.get_mut("timestamp")
                    .expect("the fixture row carries a timestamp") = json!("2020-01-01T00:00:00Z");
                row
            }],
        );
        let calling = calling_row(&home);
        assert!(
            matches!(calling, Calling::Agent(_, _)),
            "a stale input row read {calling:?}, not Agent"
        );
    }

    #[test]
    fn a_raw_role_pair_refuses_an_unproven_caller() {
        // The raw --role-level/--role-scope pair rides the same grant check
        // the promote door runs: census pins a harness ancestor, the tty is
        // absent, so the caller is unproven and the gate refuses before any
        // launch or row write.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let _pid = crate::claims::EnvVarGuard::unset("FNO_SESSION_PID");
        let _harness = crate::claims::EnvVarGuard::unset("FNO_SESSION_HARNESS");
        let ppid: u32 = unsafe { libc::getppid() } as u32;
        let _table = crate::claims::proof_tests::CensusGuard::chain(&[
            (
                std::process::id(),
                ppid,
                "fno-agents spawn --role-level 0 --role-scope e-1",
            ),
            (ppid, 1, "claude --resume /t/session.jsonl --name lead"),
            (1, 0, "launchd"),
        ]);
        let _tty = {
            crate::claims::set_test_stdin_tty(Some(false));
            crate::claims::proof_tests::TtyGuard
        };
        let params = json!({"role_level": json!(0), "role_scope": json!("e-1")});
        let err = raw_role_pair_gate(&params).expect_err("the unproven caller must refuse");
        assert_eq!(err, 2);
    }

    #[test]
    fn a_proved_human_raw_pair_gates_through() {
        // No identity markers, no harness ancestor, tty present: the same
        // attended-shell proof the promote door admits, so the operator door
        // stays open on the raw pair.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let _pid = crate::claims::EnvVarGuard::unset("FNO_SESSION_PID");
        let _harness = crate::claims::EnvVarGuard::unset("FNO_SESSION_HARNESS");
        let ppid: u32 = unsafe { libc::getppid() } as u32;
        let _table = crate::claims::proof_tests::CensusGuard::chain(&[
            (
                std::process::id(),
                ppid,
                "fno-agents spawn --role-level 0 --role-scope e-1",
            ),
            (ppid, 1, "-zsh"),
            (1, 0, "launchd"),
        ]);
        let _tty = {
            crate::claims::set_test_stdin_tty(Some(true));
            crate::claims::proof_tests::TtyGuard
        };
        let params = json!({"role_level": json!(0), "role_scope": json!("e-1")});
        raw_role_pair_gate(&params).expect("a proved human grants the raw pair");
    }

    #[test]
    fn a_granted_row_resolves_a_session_env_caller_under_harness_ancestry() {
        // The node's acceptance, verbatim: inside a lead session, the gates
        // resolve the session id the role grant names, driven by a
        // harness-ancestry call with the session env set. The full spawn
        // stamp rides the Bash tool env, the census pins a claude ancestor,
        // and the registry row grants that session level 0.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let (_tmp, _home_root, ppid) = granted_home_stage();
        let _self = crate::claims::EnvVarGuard::set("FNO_AGENT_SELF", "esther");
        let _session = crate::claims::EnvVarGuard::set("FNO_AGENT_SESSION", "s-granted");
        let _agent_harness = crate::claims::EnvVarGuard::set("FNO_AGENT_HARNESS", "claude");
        let _hn = crate::claims::EnvVarGuard::set("FNO_HARNESS_NAME", "claude");
        let _hsid = crate::claims::EnvVarGuard::set("FNO_HARNESS_SESSION_ID", "s-granted");
        // The harness's own attester marker rides the Bash tool env in
        // production (claude exports CLAUDE_CODE_SESSION_ID); the Complete
        // fno stamp alone refuses fail-closed, and that is the bystander
        // contract the next test pins.
        let _attest = crate::claims::EnvVarGuard::set("CLAUDE_CODE_SESSION_ID", "s-granted");
        let _spid = crate::claims::EnvVarGuard::set(
            "FNO_SESSION_PID",
            std::process::id().to_string().as_str(),
        );
        let _sharness = crate::claims::EnvVarGuard::set("FNO_SESSION_HARNESS", "claude");
        let _census = granted_census_stage(ppid);
        let home = crate::paths::AgentsHome::from_env();
        let calling = calling_row(&home);
        let Calling::Agent(row, sid) = &calling else {
            panic!("a session-env granted caller read {calling:?}, not Agent");
        };
        assert_eq!(row.role_level, Some(0), "{calling:?}");
        assert_eq!(sid, "s-granted");
        let params = json!({"role_level": json!(0), "role_scope": json!("fnh,fno")});
        assert!(
            raw_role_pair_checked(&calling, &params, &[], &std::collections::HashMap::new())
                .is_none(),
            "the granted level-0 caller passes the raw pair gate"
        );
    }

    #[test]
    fn a_markerless_claude_caller_stays_unproven_beside_a_live_row() {
        // The bystander rule holds: a markerless claude process shares its
        // cwd and its project with live neighbour rows, so the cwd-keyed
        // adoption refuses claude rows no marker names. The census proving
        // the harness family does not lift that guard - several live claude
        // sessions can serve one project, and adopting the only row would
        // sign the neighbour's mail. The unproven refusal stays verbatim.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let (_tmp, _home_root, ppid) = granted_home_stage();
        let _pid = crate::claims::EnvVarGuard::unset("FNO_SESSION_PID");
        let _walk_harness = crate::claims::EnvVarGuard::unset("FNO_SESSION_HARNESS");
        let _census = granted_census_stage(ppid);
        let home = crate::paths::AgentsHome::from_env();
        let calling = calling_row(&home);
        match &calling {
            Calling::Unproven(reason) => {
                assert_eq!(
                    *reason, "a harness process is in this call's ancestry",
                    "{calling:?}"
                );
            }
            other => panic!("a markerless caller read {other:?}, not Unproven"),
        }
    }

    #[test]
    fn a_resolved_holder_hands_off_its_own_scope() {
        // Specimens 2 and 3 of the widened acceptance: once the caller is
        // resolved (the env-set contract above), the hand-off door's grant
        // arms hold. A same-scope promote with succession intent is
        // admitted, an L0 grants over a lower scope, and the same
        // same-scope promote without succession intent still refuses as a
        // transfer.
        let holder = crate::state::RegistryEntry {
            name: "tania".into(),
            role_scope: Some("x-1,x-2".into()),
            role_level: Some(2),
            ..Default::default()
        };
        let projects: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let grant = |succession: bool| {
            crate::territory::role_grant_kind(&json!({
                "op": "check", "requested": "x-1,x-2",
                "caller": agent_caller(&holder, "s-t"),
                "allow_succession": succession,
                "projects": projects, "entries": Vec::<serde_json::Value>::new(),
            }))
            .expect("the grant check answers")
            .as_object()
            .and_then(|answer| answer.iter().find(|(k, _)| k == "refusal"))
            .and_then(|(_, v)| v.as_str())
            .map(str::to_string)
        };
        assert!(
            grant(true).is_none(),
            "hand-off admits its own scope: {:?}",
            grant(true)
        );
        let transfer = grant(false).expect("no succession intent is a transfer refusal");
        assert!(transfer.contains("transfer, not a grant"), "{transfer}");
        let l0 = crate::state::RegistryEntry {
            name: "atticus".into(),
            role_scope: Some("fnh,fno".into()),
            role_level: Some(0),
            ..Default::default()
        };
        let lower = crate::territory::role_grant_kind(&json!({
            "op": "check", "requested": "x-9",
            "caller": agent_caller(&l0, "s-a"),
            "allow_succession": false,
            "projects": projects, "entries": Vec::<serde_json::Value>::new(),
        }))
        .expect("the grant check answers")
        .as_object()
        .and_then(|answer| answer.iter().find(|(k, _)| k == "refusal"))
        .and_then(|(_, v)| v.as_str())
        .map(str::to_string);
        assert!(lower.is_none(), "an L0 grants a lower scope: {lower:?}");
    }

    #[test]
    fn a_level_1_agent_cannot_stamp_a_level_0_row() {
        // A contained scope passes the grant check, but the raw pair is the
        // only caller-chosen level: a level 1 agent row cannot mint the
        // superuser level 0 row even over its own scope. The arm runs before
        // the check, so the refusal names the level, not the scope.
        let lead = crate::state::RegistryEntry {
            name: "lead-a".into(),
            role_scope: Some("alpha".into()),
            role_level: Some(1),
            ..Default::default()
        };
        let calling = Calling::Agent(Box::new(lead), "s-1".into());
        let params = json!({"role_level": json!(0), "role_scope": json!("alpha")});
        let err = raw_role_pair_checked(&calling, &params, &[], &std::collections::HashMap::new())
            .expect("a sub-level agent cannot mint a level 0 row");
        assert!(err.contains("level 0 grantor row"), "{err}");
        // The same caller at its own delegation level is a plain grant: the
        // level arm does not fire, the check's same-scope refusal does.
        let delegated = json!({"role_level": json!(2), "role_scope": json!("alpha")});
        let transfer =
            raw_role_pair_checked(&calling, &delegated, &[], &std::collections::HashMap::new())
                .expect("same-scope is a transfer, refused by the check");
        assert!(transfer.contains("transfer, not a grant"), "{transfer}");
    }

    #[test]
    fn a_prose_turn_keeps_the_agent_answer() {
        // AC6-ERR: the newest real user row is prose, so the composer proof
        // holds nothing and the registered session keeps its Agent answer.
        let guard = PlanEnvGuard::take();
        guard.clear_identity();
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        crate::registry_store::seed_raw(
            agents.join("registry.json"),
            json!({"schema_version": 4, "agents": [
                {"name": "carla", "status": "live", "role_scope": "alpha", "role_level": 1,
                 "cwd": tmp.path().display().to_string(), "harness": "claude",
                 "harness_session_id": "composer-sid",
                 "created_at": "2026-11-06T00:00:00Z"}
            ]})
            .to_string(),
        );
        let _home_root = crate::claims::EnvVarGuard::set(
            crate::paths::HOME_ENV,
            agents.display().to_string().as_str(),
        );
        let _home =
            crate::claims::EnvVarGuard::set("HOME", tmp.path().display().to_string().as_str());
        let _harness = crate::claims::EnvVarGuard::set("FNO_HARNESS_NAME", "claude");
        let _sid_var = crate::claims::EnvVarGuard::set("FNO_HARNESS_SESSION_ID", "composer-sid");
        let _attest = crate::claims::EnvVarGuard::set("CLAUDE_CODE_SESSION_ID", "composer-sid");
        let _walk_pid = crate::claims::EnvVarGuard::set(
            "FNO_SESSION_PID",
            std::process::id().to_string().as_str(),
        );
        let _walk_harness = crate::claims::EnvVarGuard::set("FNO_SESSION_HARNESS", "claude");
        let _argv = set_test_grant_argv(Some(vec![
            "agents".into(),
            "org".into(),
            "promote".into(),
            "w".into(),
            "--scope".into(),
            "e-1".into(),
        ]));
        // Newest real row is prose; an older row carries the matching input.
        write_composer_transcript(
            tmp.path(),
            "composer-sid",
            &[
                composer_user_row("<bash-input>fno agents org promote w --scope e-1</bash-input>"),
                composer_user_row("status on your nodes?"),
            ],
        );
        let home = crate::paths::AgentsHome::from_env();
        let calling = calling_row(&home);
        let Calling::Agent(row, _) = &calling else {
            panic!("prose under the composer proof read {calling:?}, not Agent");
        };
        // The row survives untouched: the agent check (role, status) decides.
        assert_eq!(row.name, "carla");
    }
}
