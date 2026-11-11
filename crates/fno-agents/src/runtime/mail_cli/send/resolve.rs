// Ported from the single-file draft of send.rs; split by question.
use super::*;
use crate::claude_roster::ClaudeAgentRow;
use fno_mail::session::ShortForm;

/// 32-hex or dashed-uuid shape.
/// `pub(crate)`: the reply leaf's provenance arm reads it.
pub(crate) fn is_full_session_id(token: &str) -> bool {
    let stripped: String = token.chars().filter(|c| *c != '-').collect();
    if stripped.len() != 32 {
        return false;
    }
    stripped.chars().all(|c| c.is_ascii_hexdigit())
}

/// A full id or an 8-hex short handle.
pub(super) fn is_session_shaped(token: &str) -> bool {
    is_full_session_id(token) || is_unsafe_short_address(token)
}

// ---------------------------------------------------------------------------
// Address resolution: the five durable stores
// ---------------------------------------------------------------------------

/// One stored session a token can name.
/// `pub(crate)`: the reply leaf's ladder reads it.
#[derive(Clone, Debug)]
pub(crate) struct ReachableSession {
    pub(crate) session_id: String,
    pub(crate) source: &'static str,
    pub(crate) agent: String,
    pub(crate) cwd: Option<String>,
}

/// The Python `resolve_reachable` result: `Ok((hit, ambiguous))` on clean
/// reads, `Err((degraded, resolved))` when a store could not be read. A
/// degraded read with exactly one hit still carries the hit: the caller
/// refuses before any delivery side effect (unproven uniqueness).
/// `pub(crate)`: the reply leaf's ladder reads it.
pub(crate) struct ReachResult {
    pub(crate) hit: Option<ReachableSession>,
    pub(crate) ambiguous: Vec<String>,
    pub(crate) degraded: Vec<&'static str>,
}

/// Full id or 8-hex short, case-folded hex.
pub(super) fn token_matches(token: &str, session_id: &str) -> bool {
    if token.is_empty() || session_id.is_empty() {
        return false;
    }
    crate::identity::session_handle_tier(token, session_id).is_some()
}

/// Best-effort cwd from a claude projects dir
/// name. The encoding is lossy, so this stays a guess (verbatim=False).
pub(super) fn decode_project_dir(name: &str) -> Option<String> {
    if !name.starts_with('-') {
        return None;
    }
    let candidate = format!("/{}", name[1..].replace('-', "/"));
    if Path::new(&candidate).is_dir() {
        Some(candidate)
    } else {
        None
    }
}

/// `pub(crate)`: the reply leaf's transcript scan reads the same store.
pub(crate) fn projects_dir() -> PathBuf {
    // Precedence, which the delivery-matrix
    // cell 6b fixture pins: the fno override leads, and a blank value never
    // redirects the scan at a cwd-relative path.
    if let Ok(dir) = std::env::var(crate::claude_drive::PROJECTS_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("projects");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
        .join(".claude")
        .join("projects")
}

#[cfg(test)]
mod projects_dir_tests {
    use super::*;

    /// The delivery-matrix fixtures redirect the transcript store with
    /// FNO_CLAUDE_PROJECTS_DIR; a reader that misses the override scans the
    /// operator's real store and a stored-legacy reply never resolves.
    #[test]
    fn the_transcript_store_reader_honors_the_fno_projects_override() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let scratch = tempfile::tempdir().unwrap();
        let _guard = crate::claims::EnvVarGuard::set(
            crate::claude_drive::PROJECTS_DIR_ENV,
            scratch.path().to_str().unwrap(),
        );
        assert_eq!(projects_dir(), scratch.path().to_path_buf());
    }
}

pub(super) fn roster_path() -> PathBuf {
    let base = std::env::var("FNO_CLAUDE_DAEMON_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".claude")
                .join("daemon")
        });
    base.join("roster.json")
}

/// Alias map inversion plus the registry rows'
/// alias lists. `Err(())` reads degraded (a torn store is not a clean miss).
pub(crate) fn alias_to_session_ids(token: &str) -> Result<Vec<String>, ()> {
    let path = AgentsHome::from_env().root().join("name-map.json");
    let mut hits: Vec<String> = Vec::new();
    if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|_| ())?;
        let doc: Value = serde_json::from_str(&raw).map_err(|_| ())?;
        let Some(obj) = doc.as_object() else {
            return Err(());
        };
        for (sid, alias) in obj {
            if alias.as_str() == Some(token) {
                hits.push(sid.clone());
            }
        }
    }
    let registry = AgentsHome::shared_registry_json();
    if let Ok(rows) = crate::client_commands::load_registry_entries(&registry) {
        for row in &rows {
            let aliases = row.get("aliases").and_then(Value::as_array);
            if let Some(aliases) = aliases {
                if aliases.iter().any(|a| a.as_str() == Some(token)) {
                    if let Some(sid) = opt_str_field(row, "harness_session_id")
                        .or_else(|| opt_str_field(row, "short_id"))
                    {
                        if !hits.contains(&sid) {
                            hits.push(sid);
                        }
                    }
                }
            }
        }
    }
    Ok(hits)
}

pub(super) fn reachable_from_registry(token: &str) -> (Vec<ReachableSession>, bool) {
    let registry = AgentsHome::shared_registry_json();
    let rows = match crate::client_commands::load_registry_entries(&registry) {
        Ok(rows) => rows,
        Err(_) => return (Vec::new(), false),
    };
    let mut hits: Vec<ReachableSession> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    for e in &rows {
        let Some(sid) = opt_str_field(e, "harness_session_id") else {
            continue;
        };
        let harness = opt_str_field(e, "harness").unwrap_or_else(|| "claude".to_string());
        let key = (
            harness.clone(),
            crate::spawn_context::session_identity_key(&sid),
        );
        if token_matches(token, &sid) && !seen.contains(&key) {
            seen.insert(key);
            hits.push(ReachableSession {
                session_id: sid,
                source: "registry",
                agent: harness,
                cwd: opt_str_field(e, "cwd"),
            });
        }
    }
    (hits, true)
}

/// Python `_reachable_from_transcripts`: `<uuid>.jsonl` files inside the
/// claude projects subdirs. An absent root is a definitive empty; an
/// unreadable root is a failure.
pub(super) fn reachable_from_transcripts(token: &str) -> (Vec<ReachableSession>, bool) {
    let root = projects_dir();
    if !root.exists() {
        return (Vec::new(), true);
    }
    let read = match std::fs::read_dir(&root) {
        Ok(read) => read,
        Err(_) => return (Vec::new(), false),
    };
    let mut hits: Vec<ReachableSession> = Vec::new();
    for entry in read.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let files = match std::fs::read_dir(&dir) {
            Ok(files) => files,
            Err(_) => return (Vec::new(), false),
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if token_matches(token, stem) {
                hits.push(ReachableSession {
                    session_id: stem.to_string(),
                    source: "transcript",
                    agent: "claude".to_string(),
                    cwd: dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(decode_project_dir),
                });
            }
        }
    }
    (hits, true)
}

/// The claude daemon roster rows, exited
/// ones included. Absent file: real empty. Type-drifted: failure.
pub(super) fn reachable_from_roster(token: &str) -> (Vec<ReachableSession>, bool) {
    let path = roster_path();
    if !path.exists() {
        return (Vec::new(), true);
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return (Vec::new(), false),
    };
    let Ok(doc) = serde_json::from_str::<Value>(&raw) else {
        return (Vec::new(), false);
    };
    let Some(workers) = doc.get("workers").and_then(Value::as_object) else {
        return (Vec::new(), false);
    };
    let mut hits: Vec<ReachableSession> = Vec::new();
    for (_key, row) in workers {
        let Some(sid) = row.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        if token_matches(token, sid) {
            hits.push(ReachableSession {
                session_id: sid.to_string(),
                source: "roster",
                agent: "claude".to_string(),
                cwd: opt_str_field(row, "cwd"),
            });
        }
    }
    (hits, true)
}

/// Session stamps on backlog nodes. The
/// malformed-tolerant read Python used has no Rust twin, so an unreadable
/// store reads degraded, never empty.
pub(super) fn reachable_from_graph(token: &str) -> (Vec<ReachableSession>, bool) {
    let graph_path = crate::org_board::scope::graph_json_path(Path::new("."));
    let Ok(rows) = crate::backlog::read_entries(&graph_path) else {
        return (Vec::new(), false);
    };
    let mut hits: Vec<ReachableSession> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    for node in &rows {
        let Some(sessions) = node.get("sessions").and_then(Value::as_array) else {
            continue;
        };
        for entry in sessions {
            let Some(sid) = entry.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            let harness = entry
                .get("harness")
                .and_then(Value::as_str)
                .filter(|h| !h.is_empty())
                .unwrap_or("claude");
            let key = (
                harness.to_string(),
                crate::spawn_context::session_identity_key(sid),
            );
            if token_matches(token, sid) && !seen.contains(&key) {
                seen.insert(key);
                hits.push(ReachableSession {
                    session_id: sid.to_string(),
                    source: "graph",
                    agent: harness.to_string(),
                    cwd: node.get("cwd").and_then(Value::as_str).map(str::to_string),
                });
            }
        }
    }
    (hits, true)
}

/// Python `_reachable_from_harness_stores` over
/// `store_fallback.complete_store_hits`. The per-harness store probes
/// (claude/codex/opencode JSONL scans) have no Rust twin yet, so this leg
/// reads clean and empty rather than degrading on stores it never opened;
/// the token ladder's other four legs carry resolution.
pub(super) fn reachable_from_harness_stores(_token: &str) -> (Vec<ReachableSession>, bool) {
    (Vec::new(), true)
}

fn daemon_list_rows() -> (Vec<ClaudeAgentRow>, bool) {
    use crate::claude_roster::ClaudeAgentsSnapshot;
    if std::env::var("FNO_TEST_HERMETIC").ok().as_deref() == Some("1") {
        return (Vec::new(), true);
    }
    let snapshot = crate::claude_roster::read_all_agents_union();
    // Rows from a partial read are still evidence. An empty Unknown read
    // stays a clean miss, not a degraded one: a host with no claude binary
    // reads Unknown every time, and degrading there would turn every unknown
    // name on a codex-only box into "unreadable stores".
    let (ClaudeAgentsSnapshot::Known { rows, .. } | ClaudeAgentsSnapshot::Unknown { rows, .. }) =
        snapshot;
    (rows, true)
}

pub(crate) fn daemon_hits_from_rows(token: &str, rows: &[ClaudeAgentRow]) -> Vec<ReachableSession> {
    rows.iter()
        .filter_map(|row| {
            let name = row.name.as_deref()?;
            if name != token && !name.eq_ignore_ascii_case(token) {
                return None;
            }
            let sid = row.session_id.as_deref()?;
            Some(ReachableSession {
                session_id: sid.to_string(),
                source: "daemon",
                agent: "claude".to_string(),
                cwd: row.cwd.clone(),
            })
        })
        .collect()
}

pub(crate) fn reachable_from_daemon_list(token: &str) -> (Vec<ReachableSession>, bool) {
    let (rows, read_ok) = daemon_list_rows();
    if !read_ok {
        return (Vec::new(), false);
    }
    (daemon_hits_from_rows(token, &rows), true)
}

/// Python `resolve_reachable`: the union with the same merge, ambiguity and
/// degradation rules.
/// `pub(crate)`: the reply leaf's ladder resolves through it.
pub(crate) fn resolve_reachable(token: &str) -> ReachResult {
    if token.trim().is_empty() {
        return ReachResult {
            hit: None,
            ambiguous: Vec::new(),
            degraded: Vec::new(),
        };
    }
    let mut tokens: Vec<String> = vec![token.to_string()];
    let mut degraded: Vec<&'static str> = Vec::new();
    match alias_to_session_ids(token) {
        Ok(more) => tokens.extend(more),
        Err(()) => degraded.push("registry"),
    }
    type Source = fn(&str) -> (Vec<ReachableSession>, bool);
    let sources: [(&'static str, Source); 6] = [
        ("transcript", reachable_from_transcripts),
        ("harness-store", reachable_from_harness_stores),
        ("registry", reachable_from_registry),
        ("daemon", reachable_from_daemon_list),
        ("roster", reachable_from_roster),
        ("graph", reachable_from_graph),
    ];
    let mut found: std::collections::HashMap<(String, String), ReachableSession> =
        Default::default();
    let mut cwd_verbatim: std::collections::HashMap<(String, String), bool> = Default::default();
    for (source, lookup) in sources {
        for tok in &tokens {
            let (hits, read_ok) = lookup(tok);
            if !read_ok && !degraded.contains(&source) {
                degraded.push(source);
            }
            if !read_ok {
                continue;
            }
            for hit in hits {
                let key = (
                    hit.agent.clone(),
                    crate::spawn_context::session_identity_key(&hit.session_id),
                );
                let prior = found.get(&key);
                let take_cwd = match prior {
                    None => true,
                    Some(prior) => {
                        (prior.cwd.is_none() && hit.cwd.is_some())
                            || (hit.cwd.is_some()
                                && source_matches_verbatim(source)
                                && !cwd_verbatim.get(&key).copied().unwrap_or(false))
                    }
                };
                if let std::collections::hash_map::Entry::Vacant(slot) = found.entry(key.clone()) {
                    slot.insert(hit);
                    cwd_verbatim.insert(key.clone(), source_is_verbatim(source));
                } else if take_cwd {
                    let prior = found.get_mut(&key).expect("checked above");
                    prior.cwd = hit.cwd;
                    cwd_verbatim.insert(key.clone(), source_is_verbatim(source));
                }
            }
        }
    }
    if found.len() > 1 {
        let mut ids: Vec<String> = found.values().map(|f| f.session_id.clone()).collect();
        ids.sort();
        named_ambiguity(token, &found);
        return ReachResult {
            hit: None,
            ambiguous: ids,
            degraded: Vec::new(),
        };
    }
    if found.len() == 1 {
        let hit = found.into_values().next().expect("len == 1");
        if !degraded.is_empty() {
            return ReachResult {
                hit: Some(hit),
                ambiguous: Vec::new(),
                degraded,
            };
        }
        return ReachResult {
            hit: Some(hit),
            ambiguous: Vec::new(),
            degraded: Vec::new(),
        };
    }
    ReachResult {
        hit: None,
        ambiguous: Vec::new(),
        degraded,
    }
}

pub(super) fn source_is_verbatim(source: &str) -> bool {
    matches!(source, "registry" | "roster" | "graph" | "harness-store")
}

pub(super) fn source_matches_verbatim(source: &str) -> bool {
    source_is_verbatim(source)
}

/// Names each candidate and its first source.
pub(super) fn named_ambiguity(
    token: &str,
    found: &std::collections::HashMap<(String, String), ReachableSession>,
) {
    let mut named: Vec<String> = found
        .values()
        .map(|f| format!("{} (source={}, agent={})", f.session_id, f.source, f.agent))
        .collect();
    named.sort();
    eprintln!(
        "resolve_reachable: token {} is ambiguous: {}",
        py_repr(token),
        named.join(", ")
    );
}

// ---------------------------------------------------------------------------
// Token exits (cli.py)
// ---------------------------------------------------------------------------

/// `pub(crate)`: the reply leaf mirrors the send leaf's refusal exits.
pub(crate) fn ambiguous_token_exit(name: &str, candidates: &[String]) -> i32 {
    eprintln!(
        "ambiguous session token {}: matches {}. Send to a full session id.",
        py_repr(name),
        candidates.join(", ")
    );
    2
}

pub(super) fn unreachable_token_line(name: &str, candidates: &[String]) -> String {
    let hint = if candidates.is_empty() {
        String::new()
    } else {
        format!(
            " Send to a session id. Closest live sessions: {}.",
            candidates.join(", ")
        )
    };
    format!(
        "unknown agent or live-session handle: {}.{hint}",
        py_repr(name)
    )
}

/// `pub(crate)`: the reply leaf mirrors the send leaf's refusal exits.
pub(crate) fn unreachable_token_exit(name: &str, candidates: &[String], code: i32) -> i32 {
    eprintln!("{}", unreachable_token_line(name, candidates));
    code
}

/// `pub(crate)`: the reply leaf mirrors the send leaf's refusal exits.
pub(crate) fn unavailable_token_exit(name: &str, failed: &[String], candidates: &[String]) -> i32 {
    let visible = if candidates.is_empty() {
        String::new()
    } else {
        format!(" Visible candidates: {}.", candidates.join(", "))
    };
    let line = format!(
        "cannot resolve short session token {}: unreadable stores: {}.{} Send to a full session id.",
        py_repr(name),
        failed.join(", "),
        visible
    );
    eprintln!("{line}");
    2
}

pub(crate) fn spelling_near(name: &str, token: &str) -> bool {
    let (name, token) = (name.to_lowercase(), token.to_lowercase());
    if name.starts_with(&token) || token.starts_with(&name) {
        return true;
    }
    if name.len() < 3 || token.len() < 3 {
        return name == token;
    }
    name.as_bytes()
        .windows(3)
        .any(|w| token.as_bytes().windows(3).any(|t| t == w))
}

/// The near rows over daemon rows: name spelling-neighbors, short id first.
pub(crate) fn near_from_daemon_rows(
    token: &str,
    rows: &[ClaudeAgentRow],
) -> Vec<(ShortForm, String)> {
    rows.iter()
        .filter_map(|row| {
            let name = row.name.as_deref()?;
            if !spelling_near(name, token) {
                return None;
            }
            Some((ShortForm::from_head(&row.short_id), name.to_string()))
        })
        .collect()
}

/// The near rows over registry rows: harness_title (or name) spelling
/// neighbors, short id keyed.
pub(crate) fn near_from_registry_rows(token: &str, rows: &[Value]) -> Vec<(ShortForm, String)> {
    rows.iter()
        .filter_map(|row| {
            let sid = opt_str_field(row, "short_id")?;
            let title =
                opt_str_field(row, "harness_title").or_else(|| opt_str_field(row, "name"))?;
            if !spelling_near(&title, token) {
                return None;
            }
            Some((ShortForm::from_head(&sid), title))
        })
        .collect()
}

/// The live sessions whose names or titles share a spelling with the token:
/// the daemon rows first, then registry rows, dedup by short id, at most 5.
pub(crate) fn near_sessions(token: &str) -> Vec<(ShortForm, String)> {
    let mut out: Vec<(ShortForm, String)> = Vec::new();
    let (rows, read_ok) = daemon_list_rows();
    if read_ok {
        out.extend(near_from_daemon_rows(token, &rows));
    }
    let registry = AgentsHome::shared_registry_json();
    if let Ok(entries) = crate::client_commands::load_registry_entries(&registry) {
        out.extend(near_from_registry_rows(token, &entries));
    }
    // Dedup by short id, daemon rows first.
    let mut seen: std::collections::HashSet<String> = Default::default();
    out.retain(|(sid, _)| seen.insert(sid.as_str().to_string()));
    out.truncate(5);
    out
}

/// The refusal's candidate strings: `<short id> (<name>)`.
pub(crate) fn format_near(near: &[(ShortForm, String)]) -> Vec<String> {
    near.iter()
        .map(|(sid, name)| format!("{} ({})", sid.as_str(), name))
        .collect()
}

// ---------------------------------------------------------------------------
// The name lane and the dispatch engine
// ---------------------------------------------------------------------------

/// Canonical own address after resolution.
pub(super) fn self_recipient(
    token: &str,
    resolved_session_id: Option<&str>,
    full_only: bool,
) -> Option<String> {
    let own = resolve_self_session().unwrap_or_default();
    if own.is_empty() {
        return None;
    }
    let candidate = resolved_session_id.unwrap_or(token);
    let tier = crate::identity::session_handle_tier(candidate, &own);
    let tier = tier?;
    if (resolved_session_id.is_some() && tier != 0) || (full_only && tier != 0) {
        return None;
    }
    Some(crate::identity::canonical_handle(&own))
}

/// The collision-safe sender session id, or None
/// when --from-name named a third address.
pub(super) fn reply_session_for(from_name: Option<&str>) -> Option<String> {
    let session = resolve_self_session()?;
    let named = from_name?.trim().to_string();
    if named.is_empty() {
        return Some(session);
    }
    let key = crate::spawn_context::session_identity_key(&session);
    let exact = [
        key.chars().take(8).collect::<String>(),
        key,
        session.clone(),
    ];
    if exact.contains(&named) {
        return Some(session);
    }
    let hex_fold = named.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && !named.is_empty();
    if hex_fold {
        let lowered = named.to_lowercase();
        if exact.iter().any(|c| c.to_lowercase() == lowered) {
            return Some(session);
        }
    }
    None
}

/// Reachability BEFORE the envelope is addressed.
pub(super) fn resolve_token(
    token: &str,
) -> Result<(Option<ReachableSession>, Option<String>), TokenFailure> {
    let reach = resolve_reachable(token);
    // A StoreReadError outranks ambiguity exactly as the Python exception
    // ordering does: an unreadable store with a lone hit is the unproven-
    // uniqueness case, not a clean multi-match.
    if !reach.degraded.is_empty() {
        if !is_full_session_id(token) {
            let candidates: Vec<String> = reach
                .hit
                .as_ref()
                .map(|h| vec![h.session_id.clone()])
                .unwrap_or_default();
            return Err(TokenFailure::Unavailable {
                failed: reach.degraded.iter().map(|s| s.to_string()).collect(),
                candidates,
            });
        }
        return Ok((
            reach.hit,
            Some(format!(
                "wake=stores-unreadable({})",
                reach.degraded.join(",")
            )),
        ));
    }
    if !reach.ambiguous.is_empty() {
        return Err(TokenFailure::Ambiguous {
            candidates: reach.ambiguous.clone(),
        });
    }
    Ok((reach.hit, None))
}

pub(super) enum TokenFailure {
    Ambiguous {
        candidates: Vec<String>,
    },
    Unavailable {
        failed: Vec<String>,
        candidates: Vec<String>,
    },
}

pub(super) fn is_job_token(token: &str) -> bool {
    token.starts_with("node:") || token.starts_with("pr:")
}

#[cfg(test)]
mod mail_addr_tests {
    use super::*;
    use fno_mail::directory::Resolution;
    use fno_mail::session::SessionId;

    fn daemon_row(name: &str, sid: &str) -> ClaudeAgentRow {
        let mut row = ClaudeAgentRow::new("6bf63c59", None);
        row.name = Some(name.to_string());
        row.session_id = Some(sid.to_string());
        row
    }

    fn other_row(name: &str, sid: &str) -> ClaudeAgentRow {
        let mut row = ClaudeAgentRow::new("0aa11bb2", None);
        row.name = Some(name.to_string());
        row.session_id = Some(sid.to_string());
        row
    }

    /// A daemon row named t-decisions-read-as with session 6bf63c59-... and
    /// a registry row named 6bf63c59 with no aliases: the send resolves to
    /// the daemon row's session (AC1-HP).
    #[test]
    fn mail_addr_name_resolves_through_daemon_list() {
        let rows = vec![daemon_row(
            "t-decisions-read-as",
            "6bf63c59-753c-4e01-9d5d-000000000001",
        )];
        let hits = daemon_hits_from_rows("t-decisions-read-as", &rows);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].session_id, "6bf63c59-753c-4e01-9d5d-000000000001");
        assert_eq!(hits[0].source, "daemon");
        let hits = daemon_hits_from_rows("T-DECISIONS-READ-AS", &rows);
        assert_eq!(hits.len(), 1, "case-folded match");
    }

    /// A token no store answers: the refusal names each near session as
    /// `<short id> (<name>)` (AC2-ERR).
    #[test]
    fn mail_addr_unknown_refusal_names_short_ids() {
        let rows = vec![daemon_row(
            "t-decisions-read-as",
            "6bf63c59-753c-4e01-9d5d-000000000001",
        )];
        let near = near_from_daemon_rows("t-decis", &rows);
        assert_eq!(near.len(), 1);
        let formatted = format_near(&near);
        assert_eq!(
            formatted,
            vec!["6bf63c59 (t-decisions-read-as)".to_string()]
        );
        let line = unreachable_token_line("t-decis", &formatted);
        assert!(
            line.contains("Closest live sessions: 6bf63c59 (t-decisions-read-as)."),
            "{line}"
        );
        let bare = near_from_daemon_rows("zzz", &rows);
        assert!(bare.is_empty(), "{bare:?}");
        let bare_line = unreachable_token_line("zzz", &format_near(&bare));
        assert!(!bare_line.contains("Closest live sessions"), "{bare_line}");
        // Registry rows fill near too, titled by harness_title.
        let registry_row = serde_json::json!({
            "short_id": "9aa11bb2",
            "name": "t-alpha-1",
            "harness_title": "t-alpha-1",
            "harness_session_id": "9aa11bb2-1111-2222-3333-444455556666"
        });
        let near = near_from_registry_rows("t-alp", &[registry_row]);
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].0.as_str(), "9aa11bb2");
    }

    /// Two daemon rows sharing one name resolve Ambiguous (AC3-ERR).
    #[test]
    fn mail_addr_daemon_name_two_rows_is_ambiguous() {
        let rows = vec![
            daemon_row("twin", "a1b2c3d4-1111-2222-3333-444455556666"),
            other_row("twin", "a1b2c3d4-9999-8888-7777-666655554444"),
        ];
        let hits = daemon_hits_from_rows("twin", &rows);
        assert_eq!(hits.len(), 2);
        let resolution = Resolution::from_hits(
            hits.iter()
                .filter_map(|h| SessionId::parse(&h.session_id).ok())
                .collect(),
        );
        assert!(matches!(resolution, Resolution::Ambiguous(_)));
    }
}
