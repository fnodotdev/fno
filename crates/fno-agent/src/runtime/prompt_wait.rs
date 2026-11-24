//! The thread-prompt watch: a native `claude --bg` worker parked on a
//! permission prompt. It has no mux pane, so the check-in's pane scrape never
//! sees it. `claude agents --json` names the wait but not when it began, so
//! each read records the first sighting under `prompt-wait/<short id>.json`.
//! Contract: docs/architecture/spawn-watch.md.

use crate::claude_roster::{ClaudeAgentRow, ClaudeAgentsSnapshot};
use crate::paths::AgentsHome;
use crate::state::{self, Registry};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// A person answering at the keyboard clears a prompt well inside this.
pub(crate) const PARK_AFTER_MS: i64 = 5 * 60_000;
/// A prompt nobody answered by now is denied, never allowed: the worker gets
/// the tool's refusal and takes a route that needs no approval.
pub(crate) const DENY_AFTER_MS: i64 = 30 * 60_000;
const SENDER: &str = "fno/prompt-wait";
/// The spawner walk stops here, so a lineage loop cannot hang a pass.
const MAX_HOPS: usize = 8;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Seen {
    first_seen_ms: i64,
    notified: bool,
    /// The recipients a send reached, so a retry mails only the rest.
    told: Vec<String>,
    /// The deny key landed; one denial per wait.
    denied: bool,
}

/// One worker waiting on a prompt now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Waiting {
    pub short_id: String,
    pub session_id: Option<String>,
    pub name: String,
    pub cwd: String,
    pub waiting_for: String,
    pub since_ms: i64,
    notified: bool,
    told: Vec<String>,
    denied: bool,
    /// Grants a macOS grant screen names as missing; no key answers it.
    pub grants: Vec<&'static str>,
    /// The permission menu the screen shows, so the line names what is asked.
    pub menu: Option<crate::manifest::AnswerablePrompt>,
}

impl Waiting {
    /// Read the dialog of an overdue wait. A grant screen swaps the remedy.
    pub(crate) fn read_dialog(&mut self) {
        let daemon = crate::claude_roster::daemon_dir();
        if let Ok(screen) = crate::thread_dialog::read_thread(&daemon, &self.short_id) {
            if let crate::thread_dialog::Dialog::Grant(missing) =
                crate::thread_dialog::classify(&screen)
            {
                self.grants = missing;
            } else {
                self.menu = crate::thread_dialog::read_menu(&screen);
            }
        }
    }

    pub(crate) fn overdue(&self, now: i64) -> bool {
        now.saturating_sub(self.since_ms) >= PARK_AFTER_MS
    }

    /// Past [`DENY_AFTER_MS`], or at once while the user sleeps: nothing may
    /// sit idle through the night.
    fn deny_due(&self, now: i64, asleep: bool) -> bool {
        !self.denied && (asleep || now.saturating_sub(self.since_ms) >= DENY_AFTER_MS)
    }

    fn minutes(&self, now: i64) -> i64 {
        now.saturating_sub(self.since_ms) / 60_000
    }

    /// The check-in line: the finding and the remedy in one sentence.
    pub(crate) fn line(&self, now: i64) -> String {
        if !self.grants.is_empty() {
            return format!(
                "WAITING ON GRANT: {} (claude thread {}) for {}m - {} (TTY form: claude attach {})",
                self.name,
                self.short_id,
                self.minutes(now),
                crate::thread_dialog::grant_remedy(&self.grants),
                self.short_id
            );
        }
        let shown = match &self.menu {
            Some(m) => format!(
                " asks {} with options {}",
                crate::thread_dialog::asked_line(m).unwrap_or_else(|| "'(no command line)'".into()),
                crate::thread_dialog::numbered(m)
            ),
            None => String::new(),
        };
        format!(
            "WAITING ON APPROVAL: {} (claude thread {}) {} for {}m{shown} - a person answers at: claude attach {}",
            self.name,
            self.short_id,
            self.waiting_for,
            self.minutes(now),
            self.short_id
        )
    }
}

fn dir(home: &AgentsHome) -> PathBuf {
    home.root().join("prompt-wait")
}

/// The short id is the file name, so only a plain hex id is admitted.
fn waiting_on_permission(row: &ClaudeAgentRow, carries_pids: bool) -> bool {
    let id = &row.short_id;
    let waits = row.waiting_for.as_deref().unwrap_or_default();
    row.has_live_process(carries_pids)
        && waits.to_ascii_lowercase().contains("permission")
        && !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Record the first sighting of every waiting row and return them all. A
/// clean listing also drops the file of every row that stopped waiting. A
/// partial or failed listing drops nothing: absence from it is no evidence.
pub(crate) fn observe(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
) -> Result<Vec<Waiting>, String> {
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let carries_pids = snapshot.carries_pids();
    let rows = match snapshot {
        ClaudeAgentsSnapshot::Known { rows, .. } | ClaudeAgentsSnapshot::Unknown { rows, .. } => {
            rows
        }
    };
    let mut out = Vec::new();
    let mut keep = HashSet::new();
    for row in rows {
        if !waiting_on_permission(row, carries_pids) {
            continue;
        }
        keep.insert(row.short_id.clone());
        let path = dir.join(format!("{}.json", row.short_id));
        let mut seen: Seen = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if seen.first_seen_ms <= 0 {
            seen.first_seen_ms = now;
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
        out.push(Waiting {
            short_id: row.short_id.clone(),
            session_id: row.session_id.clone(),
            name: row.name.clone().unwrap_or_else(|| row.short_id.clone()),
            cwd: row.cwd.clone().unwrap_or_default(),
            waiting_for: row.waiting_for.clone().unwrap_or_default(),
            since_ms: seen.first_seen_ms,
            notified: seen.notified,
            told: seen.told,
            denied: seen.denied,
            grants: Vec::new(),
            menu: None,
        });
    }
    if snapshot.is_known() && snapshot.warning_text().is_empty() {
        for path in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = path.path();
            let json = path.extension().is_some_and(|e| e == "json");
            let stem = path.file_stem().and_then(|s| s.to_str());
            if json && stem.is_some_and(|id| !keep.contains(id)) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A row names its session in either id field, the way burn-watch reads it.
fn is_session(r: &crate::state::RegistryEntry, sid: &str) -> bool {
    r.harness_session_id.as_deref() == Some(sid) || r.session_id.as_deref() == Some(sid)
}

fn parent_of(home: &AgentsHome, registry: &Registry, sid: &str) -> Option<String> {
    registry
        .entries
        .iter()
        .find(|r| is_session(r, sid))
        .and_then(|r| r.current_owner_session().map(str::to_string))
        .or_else(|| crate::spawn_watch::caller_of(home, sid))
        .filter(|s| !s.is_empty() && s != sid)
}

/// Who hears about this wait: the owner session, then the nearest ancestor
/// that holds a lead role. One session that is both hears it once.
fn recipients(home: &AgentsHome, registry: &Registry, sid: &str) -> Vec<String> {
    let lead = |s: &str| {
        let mut rows = registry.entries.iter();
        rows.any(|r| is_session(r, s) && r.role_level.is_some())
    };
    let mut out: Vec<String> = parent_of(home, registry, sid).into_iter().collect();
    let mut cur = out.first().cloned();
    for _ in 0..MAX_HOPS {
        let Some(s) = cur.take() else { break };
        if lead(s.as_str()) {
            if !out.contains(&s) {
                out.push(s);
            }
            break;
        }
        cur = parent_of(home, registry, &s);
    }
    out
}

fn notice(w: &Waiting, now: i64) -> String {
    if !w.grants.is_empty() {
        return format!(
            "Worker {} on {} has waited {}m: {}. Run `claude attach {}` there after the grant.",
            w.name,
            crate::session_origin::this_machine(),
            w.minutes(now),
            crate::thread_dialog::grant_remedy(&w.grants),
            w.short_id
        );
    }
    format!(
        "Worker {} has waited {}m on a {} on {}. Nothing answers it on its own. Run `claude attach {}` on that host and pick an option.",
        w.name,
        w.minutes(now),
        w.waiting_for,
        crate::session_origin::this_machine(),
        w.short_id
    )
}

pub(crate) fn run_pass(
    home: &AgentsHome,
    now: i64,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    run_pass_with(
        home,
        now,
        &crate::claude_roster::read_all_agents_union(),
        runner,
    )
}

/// Whether the user is away: outside `[user.working_hours]` for this worker's
/// project.
fn user_asleep(w: &Waiting, now: i64) -> bool {
    crate::agents_config::user_hours_or_default(std::path::Path::new(&w.cwd)).away(now / 1000)
}

fn deny_prompt(short: &str) -> Result<String, String> {
    crate::thread_dialog::answer_prompt(&crate::claude_roster::daemon_dir(), short, "deny")
        .map_err(|e| e.to_string())
}

fn run_pass_with(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    run_pass_denying(home, now, snapshot, runner, &user_asleep, &deny_prompt)
}

fn denial_notice(w: &Waiting, now: i64, asleep: bool) -> String {
    let why = if asleep {
        "the user is asleep".to_string()
    } else {
        format!("nobody answered in {}m", w.minutes(now))
    };
    format!(
        "Worker {} on {}: its {} was denied ({why}). The worker takes a route that needs no approval, or other ready work. The item is on the board for the user.",
        w.name,
        crate::session_origin::this_machine(),
        w.waiting_for
    )
}

/// Deny every wait that is due, tell its spawner and lead, and file the item
/// on the board once. A grant screen has no deny key and stays for a person.
fn deny_due_waits(
    home: &AgentsHome,
    now: i64,
    waiting: &mut [Waiting],
    runner: crate::burn_watch::Runner<'_>,
    asleep: &dyn Fn(&Waiting, i64) -> bool,
    deny: &dyn Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    let mut first_error = None;
    let mut registry = None;
    for w in waiting.iter_mut() {
        let sleeping = asleep(w, now);
        if !w.deny_due(now, sleeping) {
            continue;
        }
        w.read_dialog();
        if !w.grants.is_empty() || deny(&w.short_id).is_err() {
            continue;
        }
        w.denied = true;
        let seen = Seen {
            first_seen_ms: w.since_ms,
            notified: w.notified,
            told: w.told.clone(),
            denied: true,
        };
        state::write_json_atomic(&dir(home).join(format!("{}.json", w.short_id)), &seen)
            .map_err(|e| e.to_string())?;
        let text = denial_notice(w, now, sleeping);
        let store = crate::provider_cap::questions_path(home);
        let key = format!("prompt denied {}", w.short_id);
        if let Err(e) =
            crate::fleet_task::file_once(&store, SENDER, &key, &w.cwd, &text, None, None)
        {
            first_error.get_or_insert(e);
        }
        if registry.is_none() {
            registry =
                Some(state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?);
        }
        let to = match (&registry, w.session_id.as_deref()) {
            (Some(reg), Some(sid)) => recipients(home, reg, sid),
            _ => Vec::new(),
        };
        for sid in to {
            if let Err(e) = crate::first_check::send_notice(&mut *runner, SENDER, &sid, &text) {
                first_error.get_or_insert(e);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn run_pass_denying(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
    runner: crate::burn_watch::Runner<'_>,
    asleep: &dyn Fn(&Waiting, i64) -> bool,
    deny: &dyn Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    let mut waiting = observe(home, now, snapshot)?;
    let denial = deny_due_waits(home, now, &mut waiting, &mut *runner, asleep, deny);
    waiting.retain(|w| !w.denied);
    if !waiting.iter().any(|w| w.overdue(now) && !w.notified) {
        return denial;
    }
    let registry = state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?;
    let mut first_error = denial.err();
    for mut w in waiting
        .into_iter()
        .filter(|w| w.overdue(now) && !w.notified)
    {
        w.read_dialog();
        let w = &w;
        let text = notice(w, now);
        let to = w
            .session_id
            .as_deref()
            .map(|sid| recipients(home, &registry, sid))
            .unwrap_or_default();
        let mut told = w.told.clone();
        // With no spawner on record, one fleet task reaches the operator.
        let notified = if to.is_empty() {
            let run = format!("claude attach {}", w.short_id);
            let store = crate::provider_cap::questions_path(home);
            let key = format!("prompt wait {}", w.short_id);
            crate::fleet_task::file_once(&store, SENDER, &key, &w.cwd, &text, Some(&run), None)
                .map_err(|e| first_error.get_or_insert(e).clone())
                .is_ok()
        } else {
            let pending: Vec<String> = to.iter().filter(|s| !told.contains(*s)).cloned().collect();
            for sid in pending {
                match crate::first_check::send_notice(&mut *runner, SENDER, &sid, &text) {
                    Ok(()) => told.push(sid),
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            to.iter().all(|sid| told.contains(sid))
        };
        if notified || told != w.told {
            let seen = Seen {
                first_seen_ms: w.since_ms,
                notified,
                told,
                denied: false,
            };
            let path = dir(home).join(format!("{}.json", w.short_id));
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Lineage, RegistryEntry};

    fn waiting_row(short: &str, sid: &str, name: &str) -> ClaudeAgentRow {
        let mut row = ClaudeAgentRow::new(short, Some("blocked")).with_pid(Some(12949));
        row.session_id = Some(sid.into());
        row.name = Some(name.into());
        row.cwd = Some("/work".into());
        row.waiting_for = Some("permission prompt".into());
        row
    }

    #[test]
    fn a_thread_prompt_mails_the_spawner_and_its_lead_once_after_five_minutes() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let (lead, mid, child) = ("aaaa-lead", "bbbb-mid", "30d6ff6a-e15b");
        let row = |sid: &str, parent: Option<&str>, level: Option<u32>| {
            let mut e = RegistryEntry::new(Some(sid.into()), Lineage::unproven("test"));
            e.harness_session_id = Some(sid.into());
            e.name = format!("w-{sid}");
            e.harness = Some("claude".into());
            e.spawned_by_session = parent.map(str::to_string);
            e.role_level = level;
            e
        };
        state::update_registry(&home.registry_json(), |r| {
            r.entries.push(row(lead, None, Some(1)));
            r.entries.push(row(mid, Some(lead), None));
            r.entries.push(row(child, Some(mid), None));
        })
        .unwrap();
        let busy = ClaudeAgentRow::new("ed2495bd", Some("working")).with_pid(Some(1));
        let parked = waiting_row("30d6ff6a", child, "t-recovery-prune");
        let snap = ClaudeAgentsSnapshot::known(vec![busy, parked.clone()]);
        let sent: std::cell::RefCell<Vec<(String, String)>> = Default::default();
        let mut deliver = |argv: &[String], _: &str| {
            sent.borrow_mut().push((argv[8].clone(), argv[9].clone()));
            (0, "accepted-new-turn".into(), String::new())
        };
        let t0 = 1_000_000;
        run_pass_with(&home, t0, &snap, &mut deliver).unwrap();
        run_pass_with(&home, t0 + PARK_AFTER_MS - 1, &snap, &mut deliver).unwrap();
        assert!(sent.borrow().is_empty(), "a fresh prompt is no news");
        let mut lead_down = |argv: &[String], _: &str| {
            if argv[8] == lead {
                (1, String::new(), "transport unavailable".to_string())
            } else {
                deliver(argv, "")
            }
        };
        assert!(run_pass_with(&home, t0 + PARK_AFTER_MS, &snap, &mut lead_down).is_err());
        run_pass_with(&home, t0 + PARK_AFTER_MS, &snap, &mut deliver).unwrap();
        {
            let sent = sent.borrow();
            let to: Vec<&str> = sent.iter().map(|(to, _)| to.as_str()).collect();
            assert_eq!(to, vec![mid, lead], "spawner, then the lead, each once");
            assert!(
                sent[0].1.contains("claude attach 30d6ff6a"),
                "{}",
                sent[0].1
            );
        }
        run_pass_with(&home, t0 + 3 * PARK_AFTER_MS, &snap, &mut deliver).unwrap();
        assert_eq!(sent.borrow().len(), 2, "one notice per wait");
        // The check-in line reads the same first sighting.
        let seen = observe(&home, t0 + PARK_AFTER_MS, &snap).unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0]
            .line(t0 + PARK_AFTER_MS)
            .starts_with("WAITING ON APPROVAL: t-recovery-prune (claude thread 30d6ff6a)"));
        // A partial listing keeps the file; a clean one without the row drops
        // it, so the next prompt counts from its own start.
        let partial = ClaudeAgentsSnapshot::Unknown {
            rows: vec![],
            warnings: vec!["timed out".into()],
        };
        observe(&home, t0, &partial).unwrap();
        assert!(dir(&home).join("30d6ff6a.json").exists());
        observe(&home, t0, &ClaudeAgentsSnapshot::known(vec![])).unwrap();
        assert!(!dir(&home).join("30d6ff6a.json").exists());
        let again = observe(&home, t0 + 9 * PARK_AFTER_MS, &snap).unwrap();
        assert_eq!(again[0].since_ms, t0 + 9 * PARK_AFTER_MS);
        // A dead process keeps its last waitingFor; it is not waiting.
        let mut dead = parked;
        dead.pid = None;
        let live = ClaudeAgentRow::new("ed2495bd", Some("working")).with_pid(Some(1));
        let snap = ClaudeAgentsSnapshot::known(vec![live, dead]);
        assert!(observe(&home, t0, &snap).unwrap().is_empty());
    }

    #[test]
    fn a_grant_screen_swaps_the_remedy_for_the_missing_grant() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let snap =
            ClaudeAgentsSnapshot::known(vec![waiting_row("30d6ff6a", "sid", "t-grant-worker")]);
        let mut w = observe(&home, 1_000_000, &snap).unwrap().remove(0);
        assert!(w.line(1_000_000).starts_with("WAITING ON APPROVAL"));
        // The approval line names a person, not an answer command: a worker
        // reading the bell cannot answer someone else's prompt.
        let approval = w.line(1_000_000);
        assert!(!approval.contains("--answer"), "{approval}");
        assert!(
            approval.contains("a person answers at: claude attach"),
            "{approval}"
        );
        w.grants = vec!["Accessibility", "Screen Recording"];
        let line = w.line(1_000_000);
        assert!(
            line.starts_with("WAITING ON GRANT: t-grant-worker"),
            "{line}"
        );
        assert!(
            line.contains("Accessibility and Screen Recording"),
            "{line}"
        );
        assert!(!line.contains("--answer allow_once"), "{line}");
    }

    #[test]
    fn an_unanswered_prompt_is_denied_after_thirty_minutes_or_at_once_while_asleep() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let snap =
            ClaudeAgentsSnapshot::known(vec![waiting_row("30d6ff6a", "sid-x", "t-away-worker")]);
        let typed: std::cell::RefCell<Vec<String>> = Default::default();
        let deny = |short: &str| {
            typed.borrow_mut().push(short.to_string());
            Ok("3".to_string())
        };
        let mut mail = |_: &[String], _: &str| (0, "accepted-new-turn".into(), String::new());
        let day = |_: &Waiting, _: i64| false;
        let night = |_: &Waiting, _: i64| true;
        let t0 = 1_000_000;
        run_pass_denying(&home, t0, &snap, &mut mail, &day, &deny).unwrap();
        run_pass_denying(&home, t0 + DENY_AFTER_MS - 1, &snap, &mut mail, &day, &deny).unwrap();
        assert!(
            typed.borrow().is_empty(),
            "a daytime prompt waits for a person"
        );
        run_pass_denying(&home, t0 + DENY_AFTER_MS, &snap, &mut mail, &day, &deny).unwrap();
        run_pass_denying(
            &home,
            t0 + DENY_AFTER_MS + 60_000,
            &snap,
            &mut mail,
            &day,
            &deny,
        )
        .unwrap();
        assert_eq!(*typed.borrow(), vec!["30d6ff6a"], "one denial per wait");
        // Asleep: the first sighting is denied at once, a second wait too.
        let home2 = AgentsHome::at(temp.path().join("agents2"));
        run_pass_denying(&home2, t0, &snap, &mut mail, &night, &deny).unwrap();
        assert_eq!(typed.borrow().len(), 2, "quiet hours hold nothing");
        let store = crate::provider_cap::questions_path(&home2);
        assert!(
            crate::fleet_task::open_tasks(&store)
                .unwrap()
                .iter()
                .any(|t| t.key == "prompt denied 30d6ff6a"),
            "the denied item lands on the board"
        );
    }
}
