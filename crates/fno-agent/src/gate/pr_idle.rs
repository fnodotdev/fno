//! The idle-PR alarm: a PR that sits green, mergeable and review-covered with
//! nobody merging it. The owning worker merges by law; this arm is the
//! backstop for the worker that left and the lead that swept only its wave.
//! Each pass lists every open PR of every registry repo. It skips drafts
//! and PRs whose body names a merge hold. It reads each other PR through
//! the status door, so a read rides the shared status cache and the account
//! poller instead of new GitHub calls. The status `ready` verdict answers
//! one question only: does the merge decision list a blocker. Missing review
//! coverage and a missing `Docs:` line are warnings there, though the merge
//! door holds on the Docs line and review law holds on coverage. The alarm
//! reads those warnings itself and stays quiet while one stands. `green`
//! keeps an unmeasured head out, because ready stays true while CI sits
//! queued (unmeasured CI warns, never blocks). The first clear sighting of a
//! head starts the clock under `pr-idle/<repo>-<pr>.json`. At 2 hours mail
//! goes to the live lead whose scope names the PR repo's project, else -
//! when no live scope covers the repo - to the live workers registered in
//! the repo, and the operator gets a phone notice; at 6 hours both are
//! raised again. A new head, a read that is held, not ready or not green, a
//! closed PR or a PR its body holds resets it.
//! Contract: docs/architecture/pr-heal.md, "The idle-PR alarm".

use crate::paths::AgentsHome;
use crate::state;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const RAISE_AFTER_MS: i64 = 2 * 3_600_000;
pub(crate) const RERAISE_AFTER_MS: i64 = 6 * 3_600_000;
const SENDER: &str = "fno/pr-idle";
/// A 2 hour threshold needs no finer beat, and a slower beat spends less of
/// the shared status cache.
const INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Seen {
    head: String,
    ready_since_ms: i64,
    /// The highest alarm level fully delivered: 0 none, 1 at 2h, 2 at 6h.
    raised: u8,
    /// The level `told` and `phoned` belong to. A delivery still pending when
    /// the next level comes due starts over for that level.
    pending: u8,
    /// The leads the pending level reached, so a retry mails only the rest.
    told: Vec<String>,
    phoned: bool,
}

/// One status-door answer for one open PR.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Read {
    pub pr: u64,
    pub head: String,
    pub ready: bool,
    pub green: bool,
    /// The alarm stays quiet though `ready` reads true: the decision warns
    /// that nobody reviewed the head, or that the merge door holds on the
    /// `Docs:` line.
    pub held: bool,
}

fn dir(home: &AgentsHome) -> PathBuf {
    home.root().join("pr-idle")
}

/// A reversible file stem: `foo-bar/r` and `foo_bar/r` must never share a
/// clock, and the escape keeps `-` free as the slug-to-PR separator.
fn stem(slug: &str) -> String {
    slug.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_string()
            } else {
                format!("_{:02x}", c as u32)
            }
        })
        .collect()
}

fn file(home: &AgentsHome, slug: &str, pr: u64) -> PathBuf {
    dir(home).join(format!("{}-{pr}.json", stem(slug)))
}

fn level_at(age_ms: i64) -> u8 {
    if age_ms >= RERAISE_AFTER_MS {
        2
    } else if age_ms >= RAISE_AFTER_MS {
        1
    } else {
        0
    }
}

fn age_words(age_ms: i64) -> String {
    let minutes = age_ms / 60_000;
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

fn notice(slug: &str, pr: u64, age_ms: i64, level: u8) -> String {
    let again = if level >= 2 {
        " This is the second alarm."
    } else {
        ""
    };
    format!(
        "PR {pr} in {slug} has been green, mergeable and reviewed for {}, and nobody merged it.{again} Merge it from that repo: `fno do pr merge {pr}`.",
        age_words(age_ms)
    )
}

/// Fold one repo's reads into its clocks and deliver every alarm that is due.
/// `open` is the set of PRs the pass may read, or None when the listing was
/// unreadable: only a clean listing drops the clock of a PR that left the
/// set. `recipients` is the routed session list from [`route_recipients`],
/// not every live lead.
pub(crate) fn run_repo(
    home: &AgentsHome,
    now: i64,
    slug: &str,
    open: Option<&HashSet<u64>>,
    reads: &[Read],
    recipients: &[String],
    runner: crate::burn_watch::Runner<'_>,
    phone: &mut dyn FnMut(&str, &str, Option<&str>) -> bool,
) -> Result<(), String> {
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut first_error = None;
    for read in reads {
        let path = file(home, slug, read.pr);
        if !read.ready || !read.green || read.held {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let mut seen: Seen = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if seen.head != read.head || seen.ready_since_ms <= 0 {
            seen = Seen {
                head: read.head.clone(),
                ready_since_ms: now,
                ..Seen::default()
            };
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
        let age = now.saturating_sub(seen.ready_since_ms);
        let level = level_at(age);
        if level <= seen.raised {
            continue;
        }
        let text = notice(slug, read.pr, age, level);
        let before = seen.clone();
        if seen.pending != level {
            seen.pending = level;
            seen.told.clear();
            seen.phoned = false;
        }
        let pending: Vec<&String> = recipients
            .iter()
            .filter(|l| !seen.told.contains(*l))
            .collect();
        for lead in pending {
            match crate::first_check::send_notice(&mut *runner, SENDER, lead, &text) {
                Ok(()) => seen.told.push(lead.clone()),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        if !seen.phoned {
            let title = format!("PR {} green and unmerged {}", read.pr, age_words(age));
            let body = format!("{slug}: nobody merged it; run fno do pr merge {}", read.pr);
            let pointer = format!("fno do pr status {}", read.pr);
            seen.phoned = phone(&title, &body, Some(&pointer));
            if !seen.phoned {
                first_error.get_or_insert(format!("PR {}: the phone notice did not send", read.pr));
            }
        }
        if seen.phoned && recipients.iter().all(|l| seen.told.contains(l)) {
            seen.raised = level;
        }
        if seen != before {
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
    }
    if let Some(open) = open {
        let prefix = format!("{}-", stem(slug));
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let pr = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok());
            if pr.is_some_and(|pr| !open.contains(&pr)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The sessions that can act on one repo's idle PR: the live teams whose
/// scope names the repo's project (scope members are project names at rungs
/// 0/1), else - when no live scope covers the repo - the live workers
/// registered in it. A rung-2 epic scope names graph nodes, never a repo, so
/// it falls through to the workers; a lead whose scope excludes the repo
/// never hears a PR it cannot act on. An empty answer mails nobody; the
/// operator phone notice is the remaining backstop.
pub(crate) fn route_recipients(
    teams: &[crate::territory::Team],
    project: Option<&str>,
    repo_root: &Path,
    registry: &crate::state::Registry,
) -> Vec<String> {
    // A free fn, not a closure: a closure holding `out` mutably would span
    // the `out.is_empty()` gate below and fail the borrow check (E0502).
    fn push_sid(out: &mut Vec<String>, sid: Option<&String>) {
        if let Some(sid) = sid.filter(|s| !s.is_empty()) {
            if !out.contains(sid) {
                out.push(sid.clone());
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    if let Some(project) = project.map(str::trim).filter(|p| !p.is_empty()) {
        for team in teams {
            if team.scope.split(',').any(|m| m.trim() == project) {
                push_sid(&mut out, team.holder_session.as_ref());
            }
        }
    }
    if out.is_empty() {
        for row in &registry.entries {
            let is_lead = row
                .role_scope
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty());
            if is_lead
                || !crate::spawn_gate::status_is_liveish(&row.status)
                || row.harness_session_id.as_deref().is_none_or(str::is_empty)
            {
                continue;
            }
            let raw = if row.project_root.is_empty() {
                row.cwd.as_str()
            } else {
                row.project_root.as_str()
            };
            if crate::paths::canonical_repo_root(Path::new(raw)).as_deref() == Some(repo_root) {
                push_sid(&mut out, row.harness_session_id.as_ref());
            }
        }
    }
    out
}

/// The decision warnings that hold the alarm though `ready` reads true: the
/// merge door holds the first, and review law holds the two coverage codes,
/// which the merge door only warns on.
fn warning_holds(code: &str) -> bool {
    matches!(
        code,
        "docs_line_missing" | "review_coverage_uncovered" | "review_coverage_unknown"
    )
}

/// The status door's verdict for one PR, or None when the door did not answer.
fn status_read(root: &Path, pr: u64) -> Option<Read> {
    let payload = json!({"cwd": root.display().to_string(), "pr": pr});
    let (code, stdout, _) = crate::pr_status::cache::run_door("status-read", &payload);
    if code > 3 {
        return None;
    }
    read_answer(pr, &stdout)
}

/// One door answer as the alarm reads it. `held` comes from the decision's
/// own warnings, which `ready` leaves alone.
fn read_answer(pr: u64, stdout: &str) -> Option<Read> {
    #[derive(Deserialize)]
    struct Answer {
        head: String,
        ready: bool,
        green: bool,
        merge_decision: Option<Decision>,
    }
    #[derive(Deserialize)]
    struct Decision {
        #[serde(default)]
        warnings: Vec<DecisionWarning>,
    }
    #[derive(Deserialize)]
    struct DecisionWarning {
        code: String,
    }
    // An answer that does not parse is no read, and the alarm stays quiet on
    // the PR.
    let answer: Answer = serde_json::from_str(stdout.trim()).ok()?;
    // A ready answer that carries no decision is no proof of review, so it
    // holds as well.
    let held = answer.ready
        && answer
            .merge_decision
            .is_none_or(|d| d.warnings.iter().any(|w| warning_holds(&w.code)));
    Some(Read {
        pr,
        head: answer.head,
        ready: answer.ready,
        green: answer.green,
        held,
    })
}

/// One row of the open-PR listing, read once where the listing meets the
/// alarm.
#[derive(Deserialize)]
struct ListedPull {
    number: u64,
    draft: Option<bool>,
    body: Option<String>,
}

/// The open PRs the alarm may read: not a draft, and a body that names no
/// merge hold. Leaving one out drops its clock, so it starts fresh when the
/// PR returns.
fn open_prs(pulls: Vec<Value>) -> HashSet<u64> {
    pulls
        .into_iter()
        // A row that does not parse names no PR the alarm can read, so it
        // stays out and the alarm stays quiet on it.
        .filter_map(|p| serde_json::from_value::<ListedPull>(p).ok())
        .filter(|p| p.draft != Some(true) && !names_merge_hold(p.body.as_deref().unwrap_or("")))
        .map(|p| p.number)
        .collect()
}

/// Whether a PR body names a merge hold: a line that opens with the words
/// `Merge hold`, or the order "do not merge" anywhere. The body stays held
/// while it carries the words, and the author removes them when the hold
/// lifts. A comment is not read.
fn names_merge_hold(body: &str) -> bool {
    let text = body.to_lowercase().replace('\u{2019}', "'");
    text.lines().any(|line| {
        line.trim_start_matches(['#', '*', '-', '>', ' ', '\t'])
            .starts_with("merge hold")
    }) || ["do not merge", "do-not-merge", "don't merge"]
        .iter()
        .any(|order| text.contains(order))
}

pub(crate) fn run_pass(home: &AgentsHome, now: i64) -> Result<(), String> {
    let teams: Vec<crate::territory::Team> =
        match crate::territory::live_teams(&home.registry_json()) {
            Ok(teams) => teams,
            Err(e) => return Err(e.to_string()),
        };
    // The worker fallback reads the same registry file live_teams just
    // answered for, so a lost read race here degrades to no workers, never
    // to a wrong lead.
    let registry = crate::state::load_registry(&home.registry_json()).unwrap_or_default();
    let mut slugs = HashSet::new();
    let mut first_error = None;
    for root in crate::daemon::worktree_sweep::registry_repo_roots(home) {
        let root = Path::new(&root);
        // Two clones of one repo are one set of clocks.
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        let listing = crate::pr_push::gh_api_pages_conditional(
            "gh",
            root,
            "repos/{owner}/{repo}/pulls?state=open&per_page=100",
        );
        let pulls: Vec<Value> = match &listing {
            Ok(pages) => crate::heal_sweep::pull_rows(pages),
            Err(e) => {
                first_error.get_or_insert(format!("{slug}: open PR listing failed: {e}"));
                continue;
            }
        };
        // A draft is not open for merging, and neither is a PR whose body
        // names a merge hold: leaving either out of the set drops its clock,
        // so it starts a fresh one when the PR returns.
        let open = open_prs(pulls);
        let reads: Vec<Read> = open
            .iter()
            .filter_map(|pr| status_read(root, *pr))
            .collect();
        let mut phone = crate::operator_notice::notify_operator_confirmed;
        let project = crate::backlog::create_cli::detect_project(root);
        let recipients = route_recipients(&teams, project.as_deref(), root, &registry);
        if let Err(e) = run_repo(
            home,
            now,
            &slug,
            Some(&open),
            &reads,
            &recipients,
            &mut crate::burn_watch::run_command,
            &mut phone,
        ) {
            first_error.get_or_insert(format!("{slug}: {e}"));
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        let busy = last.is_some_and(|t| t.elapsed() < INTERVAL);
        if busy || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(e) = run_pass(&home, crate::claims::now_ms()) {
            eprintln!("pr-idle: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ready(pr: u64, head: &str) -> Read {
        Read {
            pr,
            head: head.into(),
            ready: true,
            green: true,
            held: false,
        }
    }

    /// The door's answer for a ready and green head that carries these
    /// warnings.
    fn answer(warnings: Value) -> String {
        json!({"head": "abc", "ready": true, "green": true, "merge_decision": {"warnings": warnings}})
            .to_string()
    }

    /// One pass of [`run_repo`] over these reads: how many lead mails and
    /// phone notices it sent.
    fn sends(home: &AgentsHome, now: i64, open: &HashSet<u64>, reads: &[Read]) -> (usize, u32) {
        let mail: RefCell<Vec<(String, String)>> = Default::default();
        let mut deliver = |argv: &[String], _: &str| {
            mail.borrow_mut().push((argv[8].clone(), argv[9].clone()));
            (0, "accepted-new-turn".into(), String::new())
        };
        let phones: RefCell<u32> = Default::default();
        let mut phone = |_: &str, _: &str, _: Option<&str>| {
            *phones.borrow_mut() += 1;
            true
        };
        run_repo(
            home,
            now,
            "o/r",
            Some(open),
            reads,
            &["lead-a".to_string()],
            &mut deliver,
            &mut phone,
        )
        .unwrap();
        (mail.into_inner().len(), phones.into_inner())
    }

    #[test]
    fn a_ready_pr_alarms_every_lead_and_the_phone_at_two_hours_and_again_at_six() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let leads = vec!["lead-a".to_string(), "lead-b".to_string()];
        let mail: RefCell<Vec<(String, String)>> = Default::default();
        let mut deliver = |argv: &[String], _: &str| {
            mail.borrow_mut().push((argv[8].clone(), argv[9].clone()));
            (0, "accepted-new-turn".into(), String::new())
        };
        let phoned: RefCell<Vec<String>> = Default::default();
        let mut phone = |title: &str, _: &str, _: Option<&str>| {
            phoned.borrow_mut().push(title.to_string());
            true
        };
        let open: HashSet<u64> = [7].into();
        let reads = [ready(7, "abc")];
        let mut pass = |now: i64| {
            run_repo(
                &home,
                now,
                "o/r",
                Some(&open),
                &reads,
                &leads,
                &mut deliver,
                &mut phone,
            )
            .unwrap()
        };
        let t0 = 1_000_000;
        pass(t0);
        pass(t0 + RAISE_AFTER_MS - 1);
        assert!(mail.borrow().is_empty(), "a fresh green PR is no news");
        pass(t0 + RAISE_AFTER_MS);
        pass(t0 + RAISE_AFTER_MS + 60_000);
        {
            let mail = mail.borrow();
            let to: Vec<&str> = mail.iter().map(|(to, _)| to.as_str()).collect();
            assert_eq!(to, vec!["lead-a", "lead-b"], "each lead once at 2h");
            assert!(mail[0].1.contains("`fno do pr merge 7`"), "{}", mail[0].1);
            assert!(mail[0].1.contains("for 2h00m"), "{}", mail[0].1);
        }
        assert_eq!(
            phoned.borrow().as_slice(),
            ["PR 7 green and unmerged 2h00m"]
        );
        pass(t0 + RERAISE_AFTER_MS);
        pass(t0 + RERAISE_AFTER_MS + 60_000);
        assert_eq!(mail.borrow().len(), 4, "each lead once more at 6h");
        assert!(mail.borrow()[3].1.contains("second alarm"));
        assert_eq!(phoned.borrow().len(), 2);
    }

    #[test]
    fn a_held_or_unmeasured_read_starts_no_clock_and_never_alarms() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let open: HashSet<u64> = [7].into();
        let t0 = 1_000_000;
        // CI never measured this head: the status door reads ready, not green.
        let unmeasured = [Read {
            green: false,
            ..ready(7, "abc")
        }];
        // Ready and green, and held all the same: the merge door holds on the
        // Docs line, or nobody reviewed the head.
        let held: Vec<Read> = [
            "docs_line_missing",
            "review_coverage_uncovered",
            "review_coverage_unknown",
        ]
        .iter()
        .map(|code| read_answer(7, &answer(json!([{ "code": code }]))).unwrap())
        .collect();
        // A ready answer that carries no decision is no proof of review.
        let undecided = [read_answer(
            7,
            &json!({"head": "abc", "ready": true, "green": true}).to_string(),
        )
        .unwrap()];
        assert!(
            held.iter()
                .chain(&undecided)
                .all(|r| r.held && r.ready && r.green),
            "{held:?}"
        );
        for now in [t0, t0 + RAISE_AFTER_MS, t0 + RAISE_AFTER_MS + 60_000] {
            assert_eq!(sends(&home, now, &open, &unmeasured), (0, 0));
            for read in held.iter().chain(&undecided) {
                assert_eq!(sends(&home, now, &open, std::slice::from_ref(read)), (0, 0));
            }
        }
        assert!(
            !file(&home, "o/r", 7).exists(),
            "no clock on a held or unmeasured read"
        );
        body_hold_drops_the_clock(&home, t0);
        // A warning that holds no merge holds no alarm.
        let look_only = read_answer(7, &answer(json!([{ "code": "visual_approval" }]))).unwrap();
        assert!(look_only.ready && look_only.green && !look_only.held);
        // An answer with no head is no read, and a PR with no read keeps its
        // clock.
        let headless = json!({"pr": "7", "verdict": "unknown", "green": false, "settled": false, "ready": false});
        assert!(read_answer(7, &headless.to_string()).is_none());
        let clock = Seen {
            head: "abc".into(),
            ready_since_ms: t0,
            ..Seen::default()
        };
        assert!(state::write_json_atomic(&file(&home, "o/r", 7), &clock).is_ok());
        assert_eq!(sends(&home, t0, &open, &[]), (0, 0));
        assert!(
            file(&home, "o/r", 7).exists(),
            "a PR with no read keeps its clock"
        );
    }

    /// A PR whose body names a merge hold leaves the open set the way a draft
    /// does, loses its clock, and starts a fresh one when the words go. The
    /// order "do not merge" holds a PR the same way. The phrase "merge hold"
    /// inside a sentence does not.
    fn body_hold_drops_the_clock(home: &AgentsHome, t0: i64) {
        let row = |number: u64, draft: bool, body: Option<&str>| json!({"number": number, "draft": draft, "body": body});
        let listing = |body: &str| {
            open_prs(vec![
                row(7, false, Some("A plain change.")),
                row(8, false, Some(body)),
                row(
                    9,
                    false,
                    Some("Docs only. Do not merge this PR before that code merges."),
                ),
                row(10, true, Some("A draft.")),
                row(
                    11,
                    false,
                    Some("A merge hold in the plan frontmatter blocks dispatch."),
                ),
                row(12, false, None),
            ])
        };
        let held_body = "## Merge hold\n\nThe merge waits for the release tag.";
        let skipped: HashSet<u64> = [7, 11, 12].into();
        assert_eq!(listing(held_body), skipped);
        assert_eq!(listing("**MERGE HOLD** until the release tag"), skipped);
        let stale = Seen {
            head: "def".into(),
            ready_since_ms: t0,
            raised: 1,
            ..Seen::default()
        };
        let path = file(home, "o/r", 8);
        assert!(state::write_json_atomic(&path, &stale).is_ok());
        assert_eq!(sends(home, t0, &skipped, &[]), (0, 0));
        assert!(!path.exists(), "a body-held PR loses its clock");
        // The author removed the words: the PR is open again, and its clock
        // starts from zero.
        let open_again: HashSet<u64> = [7, 8, 11, 12].into();
        assert_eq!(listing("A plain change too."), open_again);
        assert_eq!(
            sends(home, t0 + RERAISE_AFTER_MS, &open_again, &[ready(8, "def")]),
            (0, 0)
        );
        let seen: Seen = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            seen.ready_since_ms,
            t0 + RERAISE_AFTER_MS,
            "the clock starts from zero"
        );
    }
}
