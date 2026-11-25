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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Seen {
    first_seen_ms: i64,
    notified: bool,
    /// The recipients a send reached, so a retry mails only the rest.
    told: Vec<String>,
    /// A deny key reached the worker; one denial per wait.
    denied: bool,
    /// When that key was typed. Any sighting after the denial finished is a
    /// new wait: the worker re-asked, or the key never took.
    denied_at_ms: i64,
    /// The prompt whose key-less refusal was already reported, so a prompt
    /// with no deny option reports once, not every beat.
    reported: Option<String>,
    /// The denial's board task is filed.
    boarded: bool,
    /// The recipients the denial mail reached.
    told_deny: Vec<String>,
    /// The board task and every denial mail landed.
    deny_done: bool,
    /// The one `--raw` follow-up turn went out after the denial.
    followed_up: bool,
    /// The deny key was confirmed. An unconfirmed key reached the worker, so
    /// the same prompt is not typed at again until the settle window ends.
    deny_clean: bool,
    /// This wait's denial count for the node and action; 0 until counted.
    follow_count: u32,
    /// Follow-up sends that failed, so a bad lane stops retrying.
    follow_attempts: u8,
    /// Hex fingerprint of the prompt on screen. A new one is a new wait.
    fingerprint: String,
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
    seen: Seen,
    /// Grants a macOS grant screen names as missing; no key answers it.
    pub grants: Vec<&'static str>,
    /// The permission menu the screen shows, so the line names what is asked.
    pub menu: Option<crate::manifest::AnswerablePrompt>,
    /// Fingerprint of that menu, hex, when the screen was read this pass.
    screen_fp: Option<String>,
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
                self.screen_fp = self.menu.as_ref().map(|m| {
                    m.fingerprint
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                });
            }
        }
    }

    pub(crate) fn overdue(&self, now: i64) -> bool {
        now.saturating_sub(self.since_ms) >= PARK_AFTER_MS
    }

    /// Past [`DENY_AFTER_MS`], or at once while the user sleeps: nothing may
    /// sit idle through the night.
    fn deny_due(&self, now: i64, asleep: bool) -> bool {
        !self.seen.denied && (asleep || now.saturating_sub(self.since_ms) >= DENY_AFTER_MS)
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
            seen,
            grants: Vec::new(),
            menu: None,
            screen_fp: None,
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
/// A prompt-wait step failed. The text is what the daemon logs.
#[derive(Debug)]
struct WaitError(String);

impl From<String> for WaitError {
    fn from(text: String) -> Self {
        Self(text)
    }
}

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

/// What the deny key did. `Typed` means a key reached the worker, whatever the
/// screen did next, so it is never typed twice. `NotTyped` leaves a retry.
#[derive(Debug, PartialEq)]
enum DenyOutcome {
    Typed(Option<String>),
    NotTyped(String),
}

fn deny_prompt(short: &str) -> DenyOutcome {
    use crate::thread_dialog::Refusal;
    match crate::thread_dialog::answer_prompt(&crate::claude_roster::daemon_dir(), short, "deny") {
        Ok(_) => DenyOutcome::Typed(None),
        Err(e @ (Refusal::StillShowing | Refusal::Unconfirmed(_))) => {
            DenyOutcome::Typed(Some(e.to_string()))
        }
        Err(e) => DenyOutcome::NotTyped(e.to_string()),
    }
}

fn run_pass_with(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    run_pass_denying(
        home,
        now,
        snapshot,
        runner,
        &Hooks {
            registry: &|home: &AgentsHome| {
                state::load_registry(&home.registry_json()).map_err(|e| WaitError(e.to_string()))
            },
            at_idle: &|short: &str| {
                crate::thread_dialog::read_thread(&crate::claude_roster::daemon_dir(), short)
                    .map(|screen| crate::thread_dialog::at_idle_prompt(&screen))
                    .unwrap_or(false)
            },
            asleep: &user_asleep,
            deny: &deny_prompt,
            dialog: &|w: &mut Waiting| w.read_dialog(),
        },
    )
    .map_err(|e| e.0)
}

/// The three seams a pass reaches the machine through, so a test drives the
/// pass with no screen, no config and no keys.
struct Hooks<'a> {
    /// The agent registry, read through the seam so a test can fail the read.
    registry: &'a dyn Fn(&AgentsHome) -> Result<Registry, WaitError>,
    /// True only while the worker's screen reads as an idle, empty prompt.
    at_idle: &'a dyn Fn(&str) -> bool,
    asleep: &'a dyn Fn(&Waiting, i64) -> bool,
    deny: &'a dyn Fn(&str) -> DenyOutcome,
    dialog: &'a dyn Fn(&mut Waiting),
}

fn denial_notice(w: &Waiting, now: i64, asleep: bool) -> String {
    let why = if asleep {
        "the user is away".to_string()
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

impl Waiting {
    fn persist(&self, home: &AgentsHome) -> Result<(), WaitError> {
        let seen = Seen {
            first_seen_ms: self.since_ms,
            ..self.seen.clone()
        };
        state::write_json_atomic(&dir(home).join(format!("{}.json", self.short_id)), &seen)
            .map_err(|e| WaitError(e.to_string()))
    }

    /// A finished denial followed by another sighting is a new wait, even
    /// when the re-asked prompt shows the same screen. True when it reset.
    fn reset_after_denial(&mut self, now: i64) -> bool {
        if !self.seen.denied || now <= self.seen.denied_at_ms {
            return false;
        }
        let settled = self.seen.deny_done && self.seen.followed_up && self.seen.deny_clean;
        if !settled && now - self.seen.denied_at_ms < DENIAL_SETTLE_MS {
            return false;
        }
        self.seen = Seen::default();
        self.since_ms = now;
        true
    }

    /// A deny often ends in a new prompt on the same worker. The screen's
    /// fingerprint tells the two waits apart, so the new one starts clean.
    /// True when the stored state changed.
    fn renew(&mut self, now: i64) -> bool {
        let Some(fp) = self.screen_fp.clone() else {
            return false;
        };
        if self.seen.fingerprint == fp {
            return false;
        }
        if !self.seen.fingerprint.is_empty() {
            self.seen = Seen::default();
            self.since_ms = now;
        }
        self.seen.fingerprint = fp;
        true
    }
}

/// The same action denied this many times for one node stops the follow-up
/// turns: the worker would only re-ask it.
const REPEAT_DENIALS: u32 = 2;
/// A denial still owing a board task, a mail or a follow-up turn gets this
/// long to finish before a new sighting restarts the wait.
const DENIAL_SETTLE_MS: i64 = 10 * 60_000;
/// A denial older than this no longer counts toward a repeat.
const REPEAT_WINDOW_MS: i64 = 24 * 3_600_000;

/// Count one more denial of `action` for `node` and return the total inside
/// the window. Kept per node under the agents home, outside `prompt-wait/`,
/// whose files the watch deletes when a wait ends.
fn denial_count(home: &AgentsHome, node: &str, action: &str, now: i64) -> Result<u32, WaitError> {
    let dir = home.root().join("prompt-denials");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = node
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.json"));
    let mut seen: std::collections::BTreeMap<String, (u32, i64)> = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(map) => map,
            // A counter that cannot be read reads as at the limit, so a
            // damaged file never lets follow-ups go out again.
            Err(_) => return Ok(REPEAT_DENIALS),
        },
        Err(_) => Default::default(),
    };
    seen.retain(|_, (_, first)| now - *first <= REPEAT_WINDOW_MS);
    let entry = seen.entry(action.to_string()).or_insert((0, now));
    entry.0 += 1;
    let count = entry.0;
    state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
    Ok(count)
}

fn load_once(
    home: &AgentsHome,
    hooks: &Hooks<'_>,
    registry: &mut Option<Registry>,
    first_error: &mut Option<String>,
) {
    if registry.is_some() {
        return;
    }
    match (hooks.registry)(home) {
        Ok(r) => *registry = Some(r),
        Err(e) => {
            first_error.get_or_insert(e.0);
        }
    }
}

/// The one-line command `--raw` types at the denied worker's idle prompt. It
/// restarts the worker's own node with the denial named, so it takes a route
/// that needs no approval or moves to other ready work.
fn follow_up_argv(w: &Waiting, node: &str) -> Vec<String> {
    let text = format!(
        "/fno:target {node} Your {} was denied while the user was away. Take a route that needs no approval, or move to other ready work, and note the item on the board.",
        w.waiting_for
    );
    [
        "fno",
        "agents",
        "mail",
        "send",
        "--raw",
        "--from-name",
        SENDER,
        "--origin",
        "scheduler",
        &w.short_id,
        &text,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Deny every wait that is due, then finish what a denial owes: one board
/// task and one mail per recipient. Each step is recorded as it lands, so a
/// failed step retries on the next pass and a landed one never repeats.
/// One `--raw` turn after a denial, so the idle worker restarts its node. It
/// waits for an idle prompt line, and the repeat guard stops a loop.
fn follow_up(
    home: &AgentsHome,
    now: i64,
    w: &mut Waiting,
    runner: crate::burn_watch::Runner<'_>,
    hooks: &Hooks<'_>,
    registry: &mut Option<Registry>,
    first_error: &mut Option<String>,
) {
    if !w.seen.denied || w.seen.followed_up {
        return;
    }
    // Claude's rejection result tells the model to stop and wait for
    // the user, so a denied worker idles. One typed command turn
    // sends it back to work, and only onto an idle prompt line.
    load_once(home, hooks, registry, first_error);
    let node = registry
        .as_ref()
        .zip(w.session_id.as_deref())
        .and_then(|(reg, sid)| {
            reg.entries
                .iter()
                .find(|e| is_session(e, sid))
                .and_then(|e| e.node.clone())
        });
    match node {
        // A registry that loaded and holds no node leaves nothing to
        // restart. A registry that failed to load stays pending.
        None if registry.is_some() => w.seen.followed_up = true,
        None => {}
        Some(node) => {
            let action = w.screen_fp.clone().unwrap_or_else(|| w.waiting_for.clone());
            if w.seen.follow_count == 0 {
                match denial_count(home, &node, &action, now) {
                    Ok(count) => w.seen.follow_count = count,
                    Err(e) => {
                        first_error.get_or_insert(e.0);
                    }
                }
            }
            let count = w.seen.follow_count;
            if count >= REPEAT_DENIALS {
                let store = crate::provider_cap::questions_path(home);
                let key = format!("prompt repeated {node} {action}");
                let text = format!(
                            "Node {node}: worker {} hit the same {} {count} times and was denied each time. No more follow-up turns go out. The worker stays idle until a person decides.",
                            w.name, w.waiting_for
                        );
                match crate::fleet_task::file_once(
                    &store,
                    SENDER,
                    &key,
                    &w.cwd,
                    &text,
                    None,
                    Some(&node),
                ) {
                    Ok(_) => w.seen.followed_up = true,
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            } else if count >= 1 && (hooks.at_idle)(&w.short_id) {
                let argv = follow_up_argv(w, &node);
                let (code, _out, err) = runner(&argv, "");
                if code == 0 {
                    w.seen.followed_up = true;
                } else {
                    w.seen.follow_attempts += 1;
                    w.seen.followed_up = w.seen.follow_attempts >= 3;
                    first_error.get_or_insert(format!(
                        "follow-up turn to {} failed: {}",
                        w.name,
                        err.trim()
                    ));
                }
            }
        }
    }
    if let Err(e) = w.persist(home) {
        first_error.get_or_insert(e.0);
    }
}

fn settle_denials(
    home: &AgentsHome,
    now: i64,
    waiting: &mut [Waiting],
    runner: crate::burn_watch::Runner<'_>,
    hooks: &Hooks<'_>,
) -> Result<(), WaitError> {
    let mut first_error: Option<String> = None;
    let mut registry: Option<Registry> = None;
    for w in waiting.iter_mut() {
        let sleeping = (hooks.asleep)(w, now);
        if w.deny_due(now, sleeping) && w.grants.is_empty() {
            match (hooks.deny)(&w.short_id) {
                DenyOutcome::NotTyped(e) => {
                    let prompt = w.screen_fp.clone().unwrap_or_default();
                    if w.seen.reported.as_deref() != Some(prompt.as_str()) {
                        first_error.get_or_insert(e);
                        w.seen.reported = Some(prompt);
                        if let Err(e) = w.persist(home) {
                            first_error.get_or_insert(e.0);
                        }
                    }
                    continue;
                }
                DenyOutcome::Typed(note) => {
                    w.seen.denied = true;
                    w.seen.denied_at_ms = now;
                    w.seen.deny_clean = note.is_none();
                    if let Some(note) = note {
                        first_error.get_or_insert(note);
                    }
                    if let Err(e) = w.persist(home) {
                        first_error.get_or_insert(e.0);
                    }
                }
            }
        }
        follow_up(
            home,
            now,
            w,
            &mut *runner,
            hooks,
            &mut registry,
            &mut first_error,
        );
        if !w.seen.denied || w.seen.deny_done {
            continue;
        }
        let text = denial_notice(w, now, sleeping);
        if !w.seen.boarded {
            let store = crate::provider_cap::questions_path(home);
            let key = format!("prompt denied {}", w.short_id);
            match crate::fleet_task::file_once(&store, SENDER, &key, &w.cwd, &text, None, None) {
                Ok(_) => w.seen.boarded = true,
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        load_once(home, hooks, &mut registry, &mut first_error);
        if let (Some(reg), Some(sid)) = (&registry, w.session_id.as_deref()) {
            let to = recipients(home, reg, sid);
            let pending: Vec<String> = to
                .iter()
                .filter(|s| !w.seen.told_deny.contains(*s))
                .cloned()
                .collect();
            for sid in pending {
                match crate::first_check::send_notice(&mut *runner, SENDER, &sid, &text) {
                    Ok(()) => w.seen.told_deny.push(sid),
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            w.seen.deny_done = w.seen.boarded && to.iter().all(|s| w.seen.told_deny.contains(s));
        }
        if let Err(e) = w.persist(home) {
            first_error.get_or_insert(e.0);
        }
    }
    first_error.map_or(Ok(()), |e| Err(WaitError(e)))
}

fn run_pass_denying(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
    runner: crate::burn_watch::Runner<'_>,
    hooks: &Hooks<'_>,
) -> Result<(), WaitError> {
    let mut waiting = observe(home, now, snapshot)?;
    for w in waiting.iter_mut() {
        let mut changed = w.reset_after_denial(now);
        // Reading a screen costs a process, so only a wait that can act on it
        // reads: one past the notice mark, or one the user's absence denies.
        if w.overdue(now) || (hooks.asleep)(w, now) {
            (hooks.dialog)(w);
            changed |= w.renew(now);
        }
        if changed {
            w.persist(home)?;
        }
    }
    let denial = settle_denials(home, now, &mut waiting, &mut *runner, hooks);
    let owes_notice = |w: &Waiting| w.overdue(now) && !w.seen.notified && !w.seen.denied;
    if !waiting.iter().any(owes_notice) {
        return denial;
    }
    let registry = (hooks.registry)(home)?;
    let mut first_error = denial.err().map(|e| e.0);
    for w in waiting.iter_mut().filter(|w| owes_notice(w)) {
        let text = notice(w, now);
        let to = w
            .session_id
            .as_deref()
            .map(|sid| recipients(home, &registry, sid))
            .unwrap_or_default();
        // With no spawner on record, one fleet task reaches the operator.
        let notified = if to.is_empty() {
            let run = format!("claude attach {}", w.short_id);
            let store = crate::provider_cap::questions_path(home);
            let key = format!("prompt wait {}", w.short_id);
            crate::fleet_task::file_once(&store, SENDER, &key, &w.cwd, &text, Some(&run), None)
                .map_err(|e| first_error.get_or_insert(e).clone())
                .is_ok()
        } else {
            let pending: Vec<String> = to
                .iter()
                .filter(|s| !w.seen.told.contains(*s))
                .cloned()
                .collect();
            for sid in pending {
                match crate::first_check::send_notice(&mut *runner, SENDER, &sid, &text) {
                    Ok(()) => w.seen.told.push(sid),
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            to.iter().all(|sid| w.seen.told.contains(sid))
        };
        w.seen.notified = notified;
        w.persist(home)?;
    }
    first_error.map_or(Ok(()), |e| Err(WaitError(e)))
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

    struct Rig {
        home: AgentsHome,
        typed: std::cell::RefCell<Vec<String>>,
        outcome: std::cell::RefCell<Option<DenyOutcome>>,
        fp: std::cell::RefCell<String>,
        asleep: std::cell::Cell<bool>,
        sent: std::cell::RefCell<Vec<Vec<String>>>,
        idle: std::cell::Cell<bool>,
        fail_raw: std::cell::Cell<u8>,
        registry_down: std::cell::Cell<bool>,
        _temp: tempfile::TempDir,
    }

    impl Rig {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = AgentsHome::at(temp.path().join("agents"));
            state::update_registry(&home.registry_json(), |r| {
                let mut row = RegistryEntry::new(Some("sid-x".into()), Lineage::unproven("test"));
                row.harness_session_id = Some("sid-x".into());
                row.name = "t-away-worker".into();
                row.harness = Some("claude".into());
                row.node = Some("x-77".into());
                r.entries.push(row);
            })
            .unwrap();
            Rig {
                home,
                typed: Default::default(),
                outcome: Default::default(),
                fp: std::cell::RefCell::new("aa".into()),
                asleep: Default::default(),
                sent: Default::default(),
                idle: std::cell::Cell::new(true),
                fail_raw: Default::default(),
                registry_down: Default::default(),
                _temp: temp,
            }
        }

        fn pass(&self, now: i64) -> Result<(), WaitError> {
            let snap = ClaudeAgentsSnapshot::known(vec![waiting_row(
                "30d6ff6a",
                "sid-x",
                "t-away-worker",
            )]);
            let mut mail = |argv: &[String], _: &str| {
                self.sent.borrow_mut().push(argv.to_vec());
                if argv.contains(&"--raw".to_string()) && self.fail_raw.get() > 0 {
                    self.fail_raw.set(self.fail_raw.get() - 1);
                    return (1, String::new(), "lane down".to_string());
                }
                (0, "accepted-new-turn".into(), String::new())
            };
            let at_idle = |_: &str| self.idle.get();
            let registry = |home: &AgentsHome| {
                if self.registry_down.get() {
                    return Err(WaitError("registry read failed".to_string()));
                }
                state::load_registry(&home.registry_json()).map_err(|e| WaitError(e.to_string()))
            };
            let asleep = |_: &Waiting, _: i64| self.asleep.get();
            let deny = |short: &str| {
                self.typed.borrow_mut().push(short.to_string());
                self.outcome
                    .borrow_mut()
                    .take()
                    .unwrap_or(DenyOutcome::Typed(None))
            };
            let dialog = |w: &mut Waiting| w.screen_fp = Some(self.fp.borrow().clone());
            run_pass_denying(
                &self.home,
                now,
                &snap,
                &mut mail,
                &Hooks {
                    registry: &registry,
                    at_idle: &at_idle,
                    asleep: &asleep,
                    deny: &deny,
                    dialog: &dialog,
                },
            )
        }

        fn board_rows(&self) -> usize {
            let store = crate::provider_cap::questions_path(&self.home);
            crate::fleet_task::open_tasks(&store)
                .unwrap()
                .iter()
                .filter(|t| t.key == "prompt denied 30d6ff6a")
                .count()
        }
    }

    #[test]
    fn an_unanswered_prompt_is_denied_after_thirty_minutes_or_at_once_while_away() {
        let day = Rig::new();
        let t0 = 1_000_000;
        day.pass(t0).unwrap();
        day.pass(t0 + DENY_AFTER_MS - 1).unwrap();
        assert!(day.typed.borrow().is_empty(), "a daytime prompt waits");
        day.pass(t0 + DENY_AFTER_MS).unwrap();
        day.pass(t0 + DENY_AFTER_MS + 60_000).unwrap();
        assert_eq!(day.typed.borrow().len(), 1, "one denial per wait");
        assert_eq!(day.board_rows(), 1, "the denied item lands on the board");

        let night = Rig::new();
        night.asleep.set(true);
        night.pass(t0).unwrap();
        assert_eq!(night.typed.borrow().len(), 1, "away holds nothing");
    }

    #[test]
    fn a_key_that_reached_the_worker_is_never_typed_twice() {
        let rig = Rig::new();
        rig.asleep.set(true);
        *rig.outcome.borrow_mut() = Some(DenyOutcome::Typed(Some("still showing".into())));
        assert!(rig.pass(1_000_000).is_err(), "the odd screen is reported");
        rig.pass(1_000_000 + 60_000).unwrap();
        assert_eq!(rig.typed.borrow().len(), 1);
        // A key that never left retries.
        let retry = Rig::new();
        retry.asleep.set(true);
        *retry.outcome.borrow_mut() = Some(DenyOutcome::NotTyped("screen changed".into()));
        assert!(retry.pass(1_000_000).is_err());
        retry.pass(1_000_000 + 60_000).unwrap();
        assert_eq!(retry.typed.borrow().len(), 2);
    }

    #[test]
    fn an_identical_re_asked_prompt_is_denied_again() {
        let rig = Rig::new();
        let t0 = 1_000_000;
        rig.pass(t0).unwrap();
        rig.pass(t0 + DENY_AFTER_MS).unwrap();
        rig.pass(t0 + DENY_AFTER_MS + 60_000).unwrap();
        rig.pass(t0 + 2 * DENY_AFTER_MS + 60_000).unwrap();
        assert_eq!(
            rig.typed.borrow().len(),
            2,
            "any sighting after a denial restarts the wait"
        );
    }

    #[test]
    fn a_refusal_that_types_nothing_reports_once_per_prompt() {
        let rig = Rig::new();
        rig.asleep.set(true);
        *rig.outcome.borrow_mut() = Some(DenyOutcome::NotTyped("no deny option".into()));
        assert!(rig.pass(1_000_000).is_err());
        *rig.outcome.borrow_mut() = Some(DenyOutcome::NotTyped("no deny option".into()));
        rig.pass(1_000_000 + 60_000).unwrap();
    }

    #[test]
    fn a_denied_worker_gets_one_raw_follow_up_turn_naming_its_node() {
        let rig = Rig::new();
        rig.asleep.set(true);
        rig.pass(1_000_000).unwrap();
        rig.pass(1_000_000 + 60_000).unwrap();
        let sent = rig.sent.borrow();
        let raw: Vec<&Vec<String>> = sent
            .iter()
            .filter(|a| a.contains(&"--raw".into()))
            .collect();
        assert_eq!(raw.len(), 1, "one follow-up per denial");
        let line = raw.first().and_then(|a| a.last()).unwrap();
        assert!(line.starts_with("/fno:target x-77 "), "{line}");
        assert!(line.contains("route that needs no approval"), "{line}");
        drop(sent);
        // A registry read that fails leaves the follow-up pending, not spent.
        let down = Rig::new();
        down.asleep.set(true);
        down.registry_down.set(true);
        let _ = down.pass(1_000_000);
        assert_eq!(raw_sends(&down), 0, "no node is known, so nothing is typed");
        down.registry_down.set(false);
        let _ = down.pass(1_000_000 + 60_000);
        assert_eq!(
            raw_sends(&down),
            1,
            "the pending follow-up goes once the read works"
        );
    }

    #[test]
    fn the_same_action_denied_twice_for_a_node_gets_no_more_follow_ups_and_one_board_task() {
        let rig = Rig::new();
        rig.asleep.set(true);
        rig.pass(1_000_000).unwrap();
        rig.pass(1_060_000).unwrap();
        rig.pass(1_120_000).unwrap();
        assert_eq!(rig.typed.borrow().len(), 3, "every wait is still denied");
        let raw = rig
            .sent
            .borrow()
            .iter()
            .filter(|a| a.contains(&"--raw".to_string()))
            .count();
        assert_eq!(raw, 1, "only the first denial restarts the worker");
        let store = crate::provider_cap::questions_path(&rig.home);
        let repeats = crate::fleet_task::open_tasks(&store)
            .unwrap()
            .iter()
            .filter(|t| t.key.starts_with("prompt repeated x-77"))
            .count();
        assert_eq!(repeats, 1, "one board task, however many repeats");
    }

    #[test]
    fn the_denial_counter_expires_after_a_day_and_reads_a_damaged_file_as_the_limit() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        assert_eq!(denial_count(&home, "x-1", "fp", 0).unwrap(), 1);
        assert_eq!(denial_count(&home, "x-1", "fp", 60_000).unwrap(), 2);
        assert_eq!(denial_count(&home, "x-1", "other", 60_000).unwrap(), 1);
        let later = REPEAT_WINDOW_MS + 120_000;
        assert_eq!(denial_count(&home, "x-1", "fp", later).unwrap(), 1);
        {
            let temp = tempfile::tempdir().unwrap();
            let home = AgentsHome::at(temp.path().join("agents"));
            let dir = home.root().join("prompt-denials");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("x-1.json"), "not json").unwrap();
            assert_eq!(denial_count(&home, "x-1", "fp", 0).unwrap(), REPEAT_DENIALS);
        }
    }

    fn raw_sends(rig: &Rig) -> usize {
        let raw = "--raw".to_string();
        rig.sent
            .borrow()
            .iter()
            .filter(|a| a.contains(&raw))
            .count()
    }

    #[test]
    fn the_follow_up_waits_for_an_idle_prompt_and_retries_a_failed_send() {
        let rig = Rig::new();
        rig.asleep.set(true);
        rig.idle.set(false);
        rig.pass(1_000_000).unwrap();
        assert_eq!(
            raw_sends(&rig),
            0,
            "a screen that is not idle gets no typed line"
        );
        rig.idle.set(true);
        rig.pass(1_000_000 + 60_000).unwrap();
        assert_eq!(raw_sends(&rig), 1);
        assert_eq!(rig.typed.borrow().len(), 1, "the wait did not restart");
        {
            let rig = Rig::new();
            rig.asleep.set(true);
            rig.fail_raw.set(1);
            assert!(rig.pass(1_000_000).is_err());
            rig.pass(1_000_000 + 60_000).unwrap();
            assert_eq!(raw_sends(&rig), 2, "the second send landed");
            assert_eq!(rig.typed.borrow().len(), 1, "no second denial");
        }
        {
            let rig = Rig::new();
            rig.asleep.set(true);
            rig.idle.set(false);
            *rig.outcome.borrow_mut() = Some(DenyOutcome::Typed(Some("unconfirmed".into())));
            assert!(rig.pass(1_000_000).is_err());
            assert_eq!(raw_sends(&rig), 0, "not idle, so nothing typed");
            rig.idle.set(true);
            let _ = rig.pass(1_000_000 + 60_000);
            assert_eq!(raw_sends(&rig), 1, "idle now, so the one follow-up goes");
        }
    }
}
