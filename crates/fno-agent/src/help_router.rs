//! The help router: a help tag maps to its next step and the run
//! takes that step, in-session or off-session. The tag stays the event
//! with its provenance; the router is the follow-through, so a distress
//! is never again a row the world admires and nobody acts on.
//!
//! One session carries two ids: the harness session id and, under a
//! manifest, a run id. The stop hook files a help under the first and
//! loop-check under the second, while the records a route reads (the claim
//! holder, a team holder) carry the first. The router takes both.
//!
//! A route ends in a row the sweep leaves alone. A holder that is the
//! sender records `holder-self`. A fired timer records `fired` with the leg
//! that landed, and a proven read records `read`. A holder the records
//! cannot name takes the lead ladder, so the user page is never its first
//! stop.
//!
//! Route vocabulary: in-session classes block at the stop hook (wave 3);
//! off-session classes mail through burn_watch::wake_with_text (wrapped
//! mail, never --raw); timer classes fire from the daemon arm; the
//! question ladder climbs the owner ladder with a 10 minute
//! lease from READ (Q10).

use crate::burn_watch;
use crate::distress::HelpClass;
use crate::owner_ladder::{resolve, Ask, Rung, World};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
pub(crate) const SENDER: &str = "fno/help-router";

#[derive(Debug, PartialEq)]
pub(crate) enum Route {
    InSession(String),
    OffSession { to: Recipient, text: String },
    Timer { backoff_secs: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recipient {
    Holder,
    Lead,
    Ladder,
    EvidenceHolder,
}

pub(crate) const IN_SESSION_CAP: u64 = 2;

/// The route table: one exhaustive match, no `_` arm, so a new class
/// fails to compile until it has a route.
pub(crate) fn route(class: HelpClass, rung: u64) -> Route {
    match class {
        HelpClass::StalePlan => in_or_escalate(rung, STALE_PLAN_TEXT),
        HelpClass::MissingPrereq => in_or_escalate(rung, MISSING_PREREQ_TEXT),
        HelpClass::CiRed => in_or_escalate(rung, CI_RED_TEXT),
        HelpClass::Stuck => in_or_escalate(rung, STUCK_TEXT),
        HelpClass::Held => Route::OffSession { to: Recipient::Holder, text: format!("A claim on this node is held while the holder is unreachable (help {rung}). Mail the holder or release the claim.") },
        HelpClass::Wait => Route::Timer { backoff_secs: match rung { 0 => 300, 1 => 600, _ => 900 } },
        HelpClass::EnvDenied | HelpClass::GateUnsatisfiable => Route::OffSession { to: Recipient::Lead, text: format!("The run hit an environment or gate refusal it cannot clear (help {rung}). Evidence carries the receipt. A lead decides.") },
        HelpClass::GateDeadlock => Route::OffSession { to: Recipient::EvidenceHolder, text: format!("Two gates wait on each other (help {rung}). The evidence names the holder to break the deadlock. The stop allows as Interrupted.") },
        HelpClass::Question => Route::OffSession { to: Recipient::Ladder, text: format!("A session asks a question (help {rung}). Answer by mail or record a ruling with fno inbox decide. 10 minutes without a read climbs the ladder.") },
        HelpClass::Budget => match rung {
            0 => Route::Timer { backoff_secs: 300 },
            _ => Route::OffSession { to: Recipient::Lead, text: format!("Budget hit twice (help {rung}). The evidence names the cap axis and value. A lead re-scopes or raises it.") },
        },
        HelpClass::Unclassified => Route::OffSession { to: Recipient::Lead, text: format!("An unclassified help (help {rung}). No route matched. A lead triages it, else it climbs the question ladder.") },
        }
}

/// Shared tail of the in-session classes: two in-session blocks per
/// run and class, then the class goes off-session to the lead.
fn in_or_escalate(rung: u64, text: &'static str) -> Route {
    if rung < IN_SESSION_CAP {
        Route::InSession(text.to_string())
    } else {
        Route::OffSession {
            to: Recipient::Lead,
            text: format!("{text} The in-session route is spent at rung {rung}. A lead decides."),
        }
    }
}

/// The in-session route texts (plan task 3.1), kept as constants so the
/// generated routing page and the stop hook quote the same words.
pub(crate) const STALE_PLAN_TEXT: &str =
    "run the architect pass in place: /fno:blueprint rewrite <plan>";
pub(crate) const MISSING_PREREQ_TEXT: &str = "file it with fno backlog idea \"<prereq>\" --wave-of <this node> --difficulty <band>, build it first";
pub(crate) const CI_RED_TEXT: &str = "run /fno:fix";
pub(crate) const STUCK_TEXT: &str = "consult one planner subagent";

/// Execute the route for a just-emitted distress row. Called from
/// `emit_help_distress_blocked` after the row lands, so every emitter path
/// (loopcheck inline scan, stop-hook every-fire scan, distress-scan CLI)
/// routes without a second implementation. `harness_session` is the
/// sender's harness session id, the one a route compares and skips by; `run`
/// alone may be a manifest's run id. Best-effort throughout: a routing
/// failure logs one stderr note and never changes the verdict.
pub(crate) fn route_emitted_distress(
    cwd: &Path,
    run: &str,
    harness_session: Option<&str>,
    node: Option<&str>,
    class: HelpClass,
    reason: &str,
    evidence: Option<&str>,
    rung: u64,
    turn_key: &str,
) {
    let step = route(class, rung);
    match step {
        Route::InSession(text) => {
            emit_help_route(
                cwd,
                run,
                node,
                class,
                reason,
                rung,
                "in-session",
                "self",
                None,
                None,
                0,
                turn_key,
            );
            let _ = text;
        }
        Route::Timer { backoff_secs } => {
            // The row's `to` is the wake target the sweep fires at, so it
            // names the emitting session by the id a wake resolves, not by
            // the literal word "self".
            emit_help_route(
                cwd,
                run,
                node,
                class,
                reason,
                rung,
                "timer",
                sender_id(run, harness_session),
                None,
                Some(backoff_secs),
                0,
                turn_key,
            );
        }
        Route::OffSession { to, text } => {
            deliver_off_session(
                cwd,
                run,
                harness_session,
                node,
                class,
                reason,
                evidence,
                rung,
                to,
                &text,
                turn_key,
            );
        }
    }
}

/// The id a route addresses the sender by: the harness session id, else the
/// run id. The claim and team records carry the first, and a route compares
/// by it.
fn sender_id<'a>(run: &'a str, harness_session: Option<&'a str>) -> &'a str {
    harness_session
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(run)
}

/// A live decision row names this node: the lead already ruled on the
/// escalation, so the route sends nothing and records `ruled` instead. A
/// re-emitted help and a lease climb both fire without a read proof the
/// transcript never carried, and the store, not the transcript, is the
/// ruling's address. No node, or a store the read cannot open, rules
/// nothing out: the route still sends.
fn node_ruled(node: Option<&str>) -> bool {
    let Some(node) = node.map(str::trim).filter(|n| !n.is_empty()) else {
        return false;
    };
    let graph = crate::graph_get::default_graph_path();
    let jsonl = crate::decision_index::default_state_path("decisions.jsonl");
    let Ok((rows, _)) = crate::decision_index::read_store_rows(&graph, &jsonl) else {
        return false;
    };
    ruled_in_store(rows, node)
}

/// The pure half of `node_ruled`: the live-decisions listing read for one
/// subject, non-empty when a row names the node.
fn ruled_in_store(rows: Vec<Value>, node: &str) -> bool {
    let mut entries: Option<Vec<Value>> = None;
    !crate::backlog::decisions_cli::live_rows_over(rows, &mut entries, node).is_empty()
}

/// A live ruling on the node holds the lead legs: the escalation was
/// answered, and re-emitting the help must not re-ask it. True when the
/// route is over and its `ruled` row is written.
#[allow(clippy::too_many_arguments)]
fn route_held_by_ruling(
    cwd: &Path,
    run: &str,
    node: Option<&str>,
    class: HelpClass,
    reason: &str,
    rung: u64,
    to: Recipient,
    turn_key: &str,
) -> bool {
    if !matches!(to, Recipient::Lead | Recipient::Ladder) || !node_ruled(node) {
        return false;
    }
    emit_help_route(
        cwd,
        run,
        node,
        class,
        reason,
        rung,
        "ruled",
        "decision-store",
        None,
        None,
        0,
        turn_key,
    );
    true
}

/// Resolve and deliver one off-session route. The provenance states follow
/// Q10: queued (durable receipt, no live lane), handed (a live lane took
/// it), read (proven in the recipient transcript at the sweep). An
/// unresolvable recipient never skips the leads: a holder the records
/// cannot name takes the ladder from its first rung, and a route that
/// climbs past every lead rung files on the user page. A live ruling on
/// the node holds the lead legs (see `route_held_by_ruling`). A holder that
/// is the sender is no recipient: the route records `holder-self` and sends
/// nothing.
#[allow(clippy::too_many_arguments)]
fn deliver_off_session(
    cwd: &Path,
    run: &str,
    harness_session: Option<&str>,
    node: Option<&str>,
    class: HelpClass,
    reason: &str,
    evidence: Option<&str>,
    rung: u64,
    mut to: Recipient,
    text: &str,
    turn_key: &str,
) {
    // A live holder that reports its own node held has nobody to mail, and
    // a mail to itself comes back as a resume typed over its own turn.
    if to == Recipient::Holder && holder_is_sender(node, run, harness_session) {
        emit_help_route(
            cwd,
            run,
            node,
            class,
            reason,
            rung,
            "holder-self",
            "self",
            None,
            None,
            0,
            turn_key,
        );
        return;
    }
    // A holder or evidence-holder leg is an operational ask no ruling
    // answers, so it still routes.
    if route_held_by_ruling(cwd, run, node, class, reason, rung, to, turn_key) {
        return;
    }
    // The ladder skips the sender by the id a team's holder record carries,
    // the harness session id. A manifest's run id never matches it.
    let from = sender_id(run, harness_session);
    let mut ladder_pos: u64 = 0;
    loop {
        let target = resolve_recipient(to, node, from, evidence, ladder_pos, cwd);
        match target {
            Target::Session(sid) => {
                let body = route_body(run, class, turn_key, text, node, reason, evidence);
                let (delivered, leg) = wake(&sid, &body);
                // A durable receipt is the bus: queued, not failed. The
                // DND the recipient armed only delays a bus poll, so the
                // lease runs from READ at the sweep, never from send.
                let (kind, leg_s) = if delivered {
                    ("handed", leg)
                } else {
                    ("queued", leg)
                };
                emit_help_route(
                    cwd,
                    run,
                    node,
                    class,
                    reason,
                    rung,
                    "off-session",
                    &sid,
                    Some((kind, leg_s)),
                    None,
                    ladder_pos,
                    turn_key,
                );
                return;
            }
            Target::UserPage => {
                file_user_question(
                    cwd, run, node, class, reason, evidence, rung, ladder_pos, turn_key,
                );
                return;
            }
            Target::Unresolved => match unresolved_step(to, ladder_pos) {
                // A holder the records cannot name is no rung, so the route
                // takes the ladder from its first rung.
                UnresolvedStep::TakeLadder => {
                    to = Recipient::Ladder;
                    if route_held_by_ruling(cwd, run, node, class, reason, rung, to, turn_key) {
                        return;
                    }
                }
                // Never a skip: climb one rung and try again.
                UnresolvedStep::Climb => ladder_pos += 1,
                UnresolvedStep::Page => {
                    file_user_question(
                        cwd,
                        run,
                        node,
                        class,
                        reason,
                        evidence,
                        rung,
                        LADDER_TOP + 1,
                        turn_key,
                    );
                    return;
                }
            },
        }
    }
}

/// The top ladder position before the user page: NodeLead 0, HigherUp 1,
/// LeastLoaded 2; 3 reads as the user page.
const LADDER_TOP: u64 = 2;

/// The mail body: the route text, the node, and the evidence receipt, so
/// the recipient can act without opening the row. The last line is the
/// read proof: it lands in the recipient's transcript with the mail, where
/// the sweep looks for it.
fn route_body(
    run: &str,
    class: HelpClass,
    turn_key: &str,
    text: &str,
    node: Option<&str>,
    reason: &str,
    evidence: Option<&str>,
) -> String {
    let mut body = format!("help-router: {text}");
    if let Some(n) = node {
        body.push_str(&format!(" (node {n}"));
        if !reason.trim().is_empty() {
            body.push_str(&format!(", reason: {reason}"));
        }
        body.push(')');
    } else if !reason.trim().is_empty() {
        body.push_str(&format!(" (reason: {reason})"));
    }
    if let Some(ev) = evidence.filter(|e| !e.trim().is_empty()) {
        body.push_str(&format!("\nevidence: {ev}"));
    }
    body.push_str(&format!(
        "\nread-proof: {}",
        read_proof(run, class, turn_key)
    ));
    body
}

/// The text the sweep takes as proof that a recipient read one route. It
/// names the route and not only its sender, so the proof of an earlier route
/// never reads a later one.
fn read_proof(run: &str, class: HelpClass, turn_key: &str) -> String {
    format!("{SENDER}|{run}|{}|{turn_key}", class.as_str())
}

/// Whether the node's holder is the sender.
fn holder_is_sender(node: Option<&str>, run: &str, harness_session: Option<&str>) -> bool {
    holder_session(node).is_some_and(|sid| is_sender(&sid, run, harness_session))
}

/// Whether `sid` names the sender. Both ids have to match: the holder and
/// team records carry the harness session id, and a row may carry the
/// manifest's run id instead.
fn is_sender(sid: &str, run: &str, harness_session: Option<&str>) -> bool {
    let sid = sid.trim();
    !sid.is_empty() && (sid == run.trim() || harness_session.is_some_and(|h| h.trim() == sid))
}

enum Target {
    Session(String),
    UserPage,
    Unresolved,
}

/// What the route loop does with a recipient that did not resolve at a
/// ladder position.
#[derive(Debug, PartialEq)]
enum UnresolvedStep {
    /// A direct recipient the records cannot name is no rung: take the
    /// ladder from its first rung, so a lead hears it before the user does.
    TakeLadder,
    /// Climb one rung and try again.
    Climb,
    /// No rung is left: the user page.
    Page,
}

fn unresolved_step(to: Recipient, ladder_pos: u64) -> UnresolvedStep {
    if ladder_pos == 0 && matches!(to, Recipient::Holder | Recipient::EvidenceHolder) {
        UnresolvedStep::TakeLadder
    } else if ladder_pos >= LADDER_TOP {
        UnresolvedStep::Page
    } else {
        UnresolvedStep::Climb
    }
}

fn resolve_recipient(
    to: Recipient,
    node: Option<&str>,
    from_session: &str,
    evidence: Option<&str>,
    ladder_pos: u64,
    config_cwd: &Path,
) -> Target {
    match to {
        Recipient::Holder => holder_session(node)
            .map(Target::Session)
            .unwrap_or(Target::Unresolved),
        Recipient::Lead | Recipient::Ladder => {
            ladder_at(node, from_session, ladder_pos, config_cwd)
        }
        Recipient::EvidenceHolder => evidence
            .and_then(holder_from_evidence)
            .map(Target::Session)
            .unwrap_or(Target::Unresolved),
    }
}

/// The ladder at one position over the landed owner_ladder: NodeLead,
/// HigherUp, LeastLoaded, then the user page. skip-self rides inside
/// resolve through from_session; a world read that fails still reaches the
/// user page, never a silent skip.
fn ladder_at(node: Option<&str>, from_session: &str, pos: u64, config_cwd: &Path) -> Target {
    let Some(start) = ladder_pos_rung(pos) else {
        return Target::UserPage;
    };
    let home = crate::paths::AgentsHome::from_env();
    let w: World = match crate::owner_ladder::world(&home, config_cwd) {
        Ok(w) => w,
        Err(_) => return Target::UserPage,
    };
    let owner = resolve(
        &Ask {
            node,
            from_session: Some(from_session),
            start,
        },
        &w,
    );
    if let Some(sid) = owner.session {
        return Target::Session(sid);
    }
    if owner.rung == Rung::User {
        return Target::UserPage;
    }
    Target::Unresolved
}

/// The landed owner_ladder rung a ladder position names; None is the user
/// page.
fn ladder_pos_rung(pos: u64) -> Option<Rung> {
    match pos {
        0 => Some(Rung::NodeLead),
        1 => Some(Rung::HigherUp),
        2 => Some(Rung::LeastLoaded),
        _ => None,
    }
}

/// The node claim's holder session, else the dispatch record on the claim.
fn holder_session(node: Option<&str>) -> Option<String> {
    let node = node?;
    let (state, rec) = crate::claims::status(&format!("node:{node}"), None);
    if state == crate::claims::ClaimState::Corrupted {
        return None;
    }
    let rec = rec?;
    rec.session_id.filter(|s| !s.trim().is_empty()).or_else(|| {
        rec.metadata
            .get("dispatched_by_session")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    })
}

/// A session id inside the evidence text (the deadlock names its holder).
fn holder_from_evidence(evidence: &str) -> Option<String> {
    let token = evidence
        .split_whitespace()
        .find(|t| t.len() >= 8 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))?;
    Some(token.trim_end_matches(&[',', ';', ':'][..]).to_string())
}

/// A test's stand-in for the `fno` children a route step runs, so a unit
/// test answers them itself and no mail or resume reaches a live session.
#[cfg(test)]
static STUBBED_RUN: std::sync::Mutex<Option<fn(&[String], &str) -> (i32, String, String)>> =
    std::sync::Mutex::new(None);

/// One `fno` child through the shared bounded runner.
fn run_fno(argv: &[String], cwd: &str) -> (i32, String, String) {
    #[cfg(test)]
    let answer = *STUBBED_RUN.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(test)]
    if let Some(answer) = answer {
        return answer(argv, cwd);
    }
    burn_watch::run_command(argv, cwd)
}

/// One wake over the shared runner: mail, resume fallback on a durable
/// receipt. The bool answers "a live lane took it" (handed).
fn wake(sid: &str, text: &str) -> (bool, &'static str) {
    let runner: burn_watch::Runner = &mut run_fno;
    let (delivered, leg) = burn_watch::wake_with_text(sid, false, text, SENDER, runner);
    (delivered, leg)
}

/// The user-page filing: the operator question queue, via the inbox verb.
#[allow(clippy::too_many_arguments)]
fn file_user_question(
    cwd: &Path,
    run: &str,
    node: Option<&str>,
    class: HelpClass,
    reason: &str,
    evidence: Option<&str>,
    rung: u64,
    ladder_pos: u64,
    turn_key: &str,
) {
    let mut q = format!("help-router [{}]: {reason}", class.as_str());
    if let Some(ev) = evidence.filter(|e| !e.trim().is_empty()) {
        q.push_str(&format!(" | evidence: {ev}"));
    }
    if let Some(n) = node {
        q.push_str(&format!(" | node: {n}"));
    }
    q.push_str(&format!(" | from run {run} at ladder rung {ladder_pos}"));
    let args = vec![
        "inbox".to_string(),
        "outstanding".to_string(),
        "ask".to_string(),
        q,
    ];
    let (code, _, _) = run_fno(&args, &cwd.to_string_lossy());
    if code != 0 {
        eprintln!("help-router: user-page filing failed (non-fatal) for run {run}");
    }
    emit_help_route(
        cwd,
        run,
        node,
        class,
        reason,
        rung,
        "question-page",
        "user-page",
        None,
        None,
        0,
        turn_key,
    );
}

fn parse_ts(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.timestamp())
}

/// One newest route row: fire a past-due timer, record a proven read, or
/// climb a lease that passed without one. Rows already fired, read, ruled
/// or climbed are skipped by their route kind.
fn act_on_route_row(home: &crate::paths::AgentsHome, config_cwd: &Path, row: &Value, now: i64) {
    let kind = row
        .pointer("/data/route")
        .and_then(Value::as_str)
        .unwrap_or("");
    let run = row.get("run").and_then(Value::as_str).unwrap_or("");
    if run.is_empty() {
        return;
    }
    match kind {
        "timer" => {
            let due = row_ts(row)
                + row
                    .pointer("/data/backoff_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(300) as i64;
            if now >= due {
                fire_timer(home, row, run);
            }
        }
        "off-session" => {
            let delivery = row
                .pointer("/data/delivery")
                .and_then(Value::as_str)
                .unwrap_or("");
            let to = row
                .pointer("/data/to")
                .and_then(Value::as_str)
                .unwrap_or("");
            if delivery != "queued" && delivery != "handed" {
                return;
            }
            // A proven read retires the row for good, so no later sweep
            // climbs it once the proof leaves the transcript tail. A queued
            // route counts too: the bus delivers it into the same transcript.
            if recipient_has_read(to, &route_key(row)) {
                follow_up(home, row, "read", row_ladder(row), None);
                return;
            }
            if now >= row_ts(row) + LEASE_SECS {
                // The ruling check runs at climb time: a ruling recorded
                // mid-lease still holds the climb the moment it fires.
                if node_ruled(row.pointer("/node").and_then(Value::as_str)) {
                    follow_up(home, row, "ruled", row_ladder(row), None);
                    return;
                }
                climb(home, config_cwd, row, run);
            }
        }
        _ => {}
    }
}

/// The read proof (Q10): the recipient transcript grew around the mail.
/// Bounded tail search for the route's proof; a transcript the reader cannot
/// resolve simply never proves a read, and the lease decides.
fn recipient_has_read(to: &str, key: &RouteKey<'_>) -> bool {
    let Some(path) = recipient_transcript(to) else {
        return false;
    };
    let marker = read_proof(key.run, key.class, key.turn);
    tail_contains(&path, &marker, 256 * 1024)
}

/// The recipient's transcript: the Claude projects store under every
/// account root, else the codex rollout. An id outside the uuid shape names
/// neither, and a harness that keeps no per-session file proves no read.
fn recipient_transcript(to: &str) -> Option<PathBuf> {
    if !fno_mail::session::is_uuid_shape(to) {
        return None;
    }
    crate::claude_drive::find_transcript_any_root(to)
        .or_else(|| crate::codex_store::codex_rollout_path(None, to))
}

/// The bounded tail check the read proof uses: the last `cap` bytes of the
/// transcript searched for the marker.
fn tail_contains(path: &std::path::PathBuf, marker: &str, cap: u64) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return false;
    };
    let start = len.saturating_sub(cap);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return false;
    }
    String::from_utf8_lossy(&buf).contains(marker)
}

/// The lease fired: climb one rung from the row's stored ladder position
/// and mail the next owner. The climbed row lands first and on the row's
/// own key, so one lease climbs once. A failed write leaves the row
/// eligible, and the next tick tries again.
fn climb(home: &crate::paths::AgentsHome, config_cwd: &Path, row: &Value, run: &str) {
    let key = route_key(row);
    let to_sid = row
        .pointer("/data/to")
        .and_then(Value::as_str)
        .unwrap_or("");
    // A holder the route addressed directly sits before the ladder, so its
    // climb starts at the ladder's first rung. A ladder rung climbs one step.
    let next = if holder_session(key.node).as_deref() == Some(to_sid) {
        0
    } else {
        row_ladder(row) + 1
    };
    let body = format!(
        "help-router: a question climbed the ladder (run {run}, class {}). The holder never read the delivery; a ruling is owed.",
        key.class.as_str()
    );
    if !follow_up(home, row, "climbed", next, None) {
        return; // no durable climb row: a double climb beats a silent one
    }
    match ladder_at(key.node, to_sid, next, config_cwd) {
        Target::Session(sid) => {
            let _ = wake(&sid, &body);
        }
        _ => {
            file_user_question(
                config_cwd,
                run,
                key.node,
                key.class,
                "question lease expired on every rung",
                None,
                0,
                next,
                key.turn,
            );
        }
    }
}

/// What a sweep-side row copies from the row it follows. The sweep's key
/// is run, node, class and turn, so a follow-up that drops one of them
/// never supersedes its row.
struct RouteKey<'a> {
    run: &'a str,
    node: Option<&'a str>,
    class: HelpClass,
    turn: &'a str,
}

fn route_key(row: &Value) -> RouteKey<'_> {
    RouteKey {
        run: row.pointer("/run").and_then(Value::as_str).unwrap_or(""),
        node: row
            .pointer("/node")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty()),
        class: HelpClass::parse(
            row.pointer("/data/class")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        turn: row
            .pointer("/data/turn")
            .and_then(Value::as_str)
            .unwrap_or(""),
    }
}

/// The ladder position a row stored.
fn row_ladder(row: &Value) -> u64 {
    row.pointer("/data/ladder")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// One sweep-side route row, on the same key as the row it follows.
fn follow_up_row(
    key: &RouteKey<'_>,
    route: &str,
    to: &str,
    ladder: u64,
    delivery: Option<(&str, &str)>,
) -> Value {
    let mut data = json!({
        "class": key.class.as_str(),
        "route": route,
        "to": to,
        "ladder": ladder,
        "turn": key.turn,
    });
    if let Some((kind, leg)) = delivery {
        if let Some(obj) = data.as_object_mut() {
            obj.insert("delivery".to_string(), json!(kind));
            obj.insert("leg".to_string(), json!(leg));
        }
    }
    let mut row = json!({
        "ts": crate::loopcheck::now_rfc3339_utc(),
        "v": 1,
        "type": "help_route",
        "source": "help-router",
        "run": key.run,
        "data": data,
    });
    if let Some(node) = key.node {
        if let Some(obj) = row.as_object_mut() {
            obj.insert("node".to_string(), json!(node));
        }
    }
    row
}

/// Write the row that follows `row` in the journal the sweep reads, so it
/// supersedes `row` there: `read`, `ruled`, `climbed` or `fired`. Everything
/// but `route`, `ladder` and `delivery` copies from `row`. False when
/// nothing landed: the row it failed to retire stays eligible, and the next
/// tick tries again.
fn follow_up(
    home: &crate::paths::AgentsHome,
    row: &Value,
    route: &str,
    ladder: u64,
    delivery: Option<(&str, &str)>,
) -> bool {
    let key = route_key(row);
    let to_sid = row
        .pointer("/data/to")
        .and_then(Value::as_str)
        .unwrap_or("");
    match crate::claims::append_event_line(
        &home.events_jsonl(),
        &follow_up_row(&key, route, to_sid, ladder, delivery),
        std::time::Duration::from_secs(2),
    ) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("help-router: {route} row write failed (non-fatal): {error}");
            false
        }
    }
}

/// The wait elapsed: wake the session with the timer's resolution. A
/// budget-class timer is the terminal route's resume leg: the body names
/// the resume, not a wait re-check. One wake retires the timer with its leg,
/// landed or not. A retry would queue a second mail for a session that
/// cannot be woken, so a lost wake is a row and never a second ask.
fn fire_timer(home: &crate::paths::AgentsHome, row: &Value, run: &str) {
    let to = row
        .pointer("/data/to")
        .and_then(Value::as_str)
        .unwrap_or(run);
    let body = if row.pointer("/data/class").and_then(Value::as_str) == Some("budget") {
        "help-router: your budget resume is due. The run was cut on its budget cap; resume from the resume receipt with the same cap. If the resumed run trips the cap again, the route asks the lead instead of you."
    } else {
        "help-router: your wait backoff elapsed. Re-check the condition you were waiting on; if it cleared, carry on, else re-emit <help class=\"wait\"> with fresh evidence."
    };
    let (delivered, leg) = wake(to, body);
    let kind = if delivered { "handed" } else { "queued" };
    follow_up(home, row, "fired", 0, Some((kind, leg)));
}

/// The `help_route` follow-up event: one row per route step, so a failed
/// step is visible and the sweep has its memory. Mirrors to the journal
/// the daemon sweep reads (sessions keep project logs of their own).
#[allow(clippy::too_many_arguments)]
fn emit_help_route(
    cwd: &Path,
    run: &str,
    node: Option<&str>,
    class: HelpClass,
    reason: &str,
    rung: u64,
    route_kind: &str,
    to: &str,
    delivery: Option<(&str, &str)>,
    backoff_secs: Option<u64>,
    ladder_pos: u64,
    turn_key: &str,
) {
    let cap = |s: &str| -> String { s.chars().take(500).collect() };
    let mut data = json!({
        "class": class.as_str(),
        "reason": cap(reason),
        "rung": rung,
        "route": route_kind,
        "to": cap(to),
        "turn": cap(turn_key),
    });
    if let Some((kind, leg)) = delivery {
        data["delivery"] = json!(kind);
        data["leg"] = json!(leg);
    }
    if let Some(secs) = backoff_secs {
        data["backoff_secs"] = json!(secs);
    }
    if route_kind == "off-session" {
        data["ladder"] = json!(ladder_pos);
    }
    let mut env = json!({
        "ts": crate::loopcheck::now_rfc3339_utc(),
        "v": 1,
        "type": "help_route",
        "source": "help-router",
        "run": run,
        "data": data,
    });
    if let Some(n) = node {
        env["node"] = json!(n);
    }
    let line_timeout = std::time::Duration::from_secs(2);
    let project = crate::paths::events_path(cwd);
    if let Err(error) = crate::claims::append_event_line(&project, &env, line_timeout) {
        eprintln!("help-router: route row write failed (non-fatal): {error}");
    }
    let sweep_journal = crate::paths::AgentsHome::from_env_opt().map(|home| home.events_jsonl());
    if let Some(global) = sweep_journal.filter(|global| *global != project) {
        if let Err(error) = crate::claims::append_event_line(&global, &env, line_timeout) {
            eprintln!(
                "help-router: route row mirror to {} failed (non-fatal): {error}",
                global.display()
            );
        }
    }
}

/// The sweep period: the daemon arm re-checks leases and timers each tick.
const SWEEP_INTERVAL_SECS: u64 = 60;

/// The lease window (Q10): 10 minutes from HAND without a READ proof.
const LEASE_SECS: i64 = 600;

/// The daemon arm state for the help-router sweep.
pub struct Arm {
    last_tick: std::sync::Mutex<Option<std::time::Instant>>,
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    config_cwd: PathBuf,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Arm {
            last_tick: std::sync::Mutex::new(None),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            config_cwd,
        }
    }
}

/// The cadence-gated sweep: fires timer routes past their backoff and
/// climbs question leases past 10 minutes without a read proof.
pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    let due = {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        let fire = last.is_none_or(|t| t.elapsed().as_secs() >= SWEEP_INTERVAL_SECS);
        if fire {
            *last = Some(now);
        }
        fire
    };
    if !due {
        return;
    }
    if arm
        .in_flight
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let config_cwd = arm.config_cwd.clone();
    let flag = std::sync::Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = SweepGate(flag);
        sweep(&home, &config_cwd);
    });
}

/// Mirrors fleet_arms' SweepGate: releases the in-flight flag on scope exit.
struct SweepGate(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for SweepGate {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// One sweep pass over the global log: timers past due fire their wake;
/// off-session deliveries past the lease without a read proof climb.
fn sweep(home: &crate::paths::AgentsHome, config_cwd: &Path) {
    let global = home.events_jsonl();
    let now = now_epoch();
    let rows = crate::event_store::journal_text(&global, &["help_route"]);
    let mut newest: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for line in rows.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let key = row_key(&row);
        match newest.get(&key) {
            Some(prev) if row_ts(prev) > row_ts(&row) => {}
            _ => {
                newest.insert(key, row);
            }
        }
    }
    for (_key, row) in newest {
        act_on_route_row(home, config_cwd, &row, now);
    }
}

/// The sweep key: the four fields `route_key` reads, so a follow-up row
/// lands on the key of the row it follows.
fn row_key(row: &Value) -> String {
    let key = route_key(row);
    format!(
        "{}|{}|{}|{}",
        key.run,
        key.node.unwrap_or(""),
        key.class.as_str(),
        key.turn
    )
}

fn row_ts(row: &Value) -> i64 {
    row.get("ts")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or(0)
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The generated routing page (docs/architecture/help-routing.md), rendered
/// from the enum and the route table so the doc cannot drift. The
/// completeness test blesses with HELP_ROUTING_BLESS=1.
pub fn render_routing_page() -> String {
    let all = [
        HelpClass::StalePlan,
        HelpClass::MissingPrereq,
        HelpClass::CiRed,
        HelpClass::Stuck,
        HelpClass::Held,
        HelpClass::Wait,
        HelpClass::EnvDenied,
        HelpClass::GateDeadlock,
        HelpClass::GateUnsatisfiable,
        HelpClass::Question,
        HelpClass::Budget,
        HelpClass::Unclassified,
    ];
    let mut out = String::from(
        "<!-- generated: crates/fno-agent/src/help_router.rs renders this; HELP_ROUTING_BLESS=1 rewrites it. Edits are refused by generated-write-guard. -->\n\n# Help routing\n\nEvery `<help class=... reason=... evidence=...>` routes to its next step. The run takes that step in the same session or by mail to the rung that owns it. The chain ends only at a guard that names its owner.\n\n| Class | First route (rung 0) | Escalation |\n|---|---|---|\n",
    );
    for class in all {
        let first = route(class, 0);
        let next = route(class, 2);
        let first_kind = route_kind(&first);
        let next_kind = route_kind(&next);
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            class.as_str(),
            route_cell(&first, first_kind),
            route_cell(&next, next_kind),
        ));
    }
    out.push_str("\n## Flow\n\n```mermaid\nflowchart TD\n    help[\"<help class=...>\"] --> scan[\"stop-hook scan writes the blocked row\"]\n    scan --> route{\"route(class, rung)\"}\n    route -->|in-session| block[\"stop hook blocks with the route text (2-cap)\"]\n    route -->|off-session| mail[\"fno/help-router mail to the owner rung\"]\n    route -->|timer| wait[\"daemon arm fires the wake after the backoff\"]\n    mail -->|unread 10 min| climb[\"climb the owner ladder one rung\"]\n    climb --> page[\"fno inbox outstanding ask files on the user page\"]\n```\n\n## Emission rule\n\nEmit the tag, then take the routed step. STOP only for irreversible, money, public surface, or taste. The in-session classes block at most twice per run and class (the third help mails the lead). Question climbs the owner ladder on a 10 minute lease from READ.\n\n## Delivery legs and the lease\n\nOff-session routes deliver through `burn_watch::wake_with_text`: mail from `fno/help-router` first. A durable receipt triggers the resume fallback. The route row records the leg that landed (queued, handed). The lease runs from READ, proven in the recipient transcript at the sweep. A delivery that hands and is never read climbs one rung after 10 minutes, ending on the user page (`fno inbox outstanding ask`).\n\nA live decision on the escalation's node records `ruled` instead and sends nothing, so a lead's ruling cannot be re-asked.
");
    out
}

fn route_kind(r: &Route) -> &'static str {
    match r {
        Route::InSession(_) => "in-session",
        Route::OffSession { to, .. } => match to {
            Recipient::Holder => "off-session, node holder",
            Recipient::Lead => "off-session, lead",
            Recipient::Ladder => "off-session, ladder",
            Recipient::EvidenceHolder => "off-session, evidence holder",
        },
        Route::Timer { .. } => "timer",
    }
}

fn route_cell(r: &Route, kind: &'static str) -> String {
    match r {
        Route::InSession(text) => format!("in-session: {text}"),
        Route::OffSession { text, .. } => format!("{kind}: {}", text.trim()),
        Route::Timer { backoff_secs } => format!("timer: {backoff_secs}s backoff, 5m/10m/15m cap"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distress::HelpClass;

    /// A live ruling on the node holds the route (the re-send bug): a live
    /// decision row whose subject names the node answers the escalation, so
    /// nothing re-sends it. A retracted row, a ruling on another node, and
    /// an empty store rule nothing out.
    #[test]
    fn a_live_ruling_on_the_node_holds_the_route() {
        let node = "x-1111";
        let row = |id: &str, subject: &str| {
            json!({
                "decision_id": id,
                "subject": subject,
                "decision": "Hold the node until the lane frees.",
                "authority_source": "operator",
                "ts": "2026-11-16T00:00:00Z",
            })
        };
        assert!(ruled_in_store(vec![row("d-aaaa0001", node)], node));
        let retracted = vec![
            row("d-bbbb0002", node),
            json!({
                "_event_type": "decision_retracted",
                "ts": "2026-11-17T00:00:00Z",
                "target_decision_id": "d-bbbb0002",
                "reason": "the operator reversed it in chat",
            }),
        ];
        assert!(!ruled_in_store(retracted, node));
        assert!(!ruled_in_store(vec![row("d-cccc0003", "x-2222")], node));
        assert!(!ruled_in_store(Vec::new(), node));
    }

    /// The sweep marks a ruled escalation instead of climbing it: the store
    /// holds a live ruling on the row's node, so the lease expiry writes
    /// one `ruled` row and no `climbed` row lands behind it.
    #[test]
    fn the_sweep_marks_a_ruled_escalation_instead_of_climbing() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let _fno_home =
            crate::claims::EnvVarGuard::set("FNO_HOME", dir.path().to_str().expect("utf-8"));
        let jsonl = dir.path().join("decisions.jsonl");
        let envelope = r#"{"type":"operator_decision","ts":"2026-11-16T00:00:00Z","data":{"decision_id":"d-aaaa0001","subject":"x-1111","decision":"Hold the node.","authority_source":"operator"}}"#;
        std::fs::write(&jsonl, format!("{envelope}\n")).expect("writes");
        let home = crate::paths::AgentsHome::at(dir.path().join("agents"));
        let past = (chrono::Utc::now() - chrono::Duration::seconds(3600)).to_rfc3339();
        let row = json!({
            "ts": past,
            "v": 1,
            "type": "help_route",
            "source": "help-router",
            "run": "run-x6cea",
            "node": "x-1111",
            "data": {
                "class": "env_denied",
                "route": "off-session",
                "to": "lead-session",
                "delivery": "queued",
                "ladder": 0,
                "turn": "turn-1",
            },
        });
        act_on_route_row(&home, dir.path(), &row, now_epoch());
        let journal = crate::event_store::journal_text(&home.events_jsonl(), &["help_route"]);
        assert!(
            journal.contains("\"route\":\"ruled\""),
            "no ruled row in the journal: {journal}"
        );
        assert!(
            !journal.contains("\"route\":\"climbed\""),
            "climbed a ruled escalation: {journal}"
        );
    }

    /// A test's stand-in for the `fno` children a route step runs, so a
    /// unit test answers them itself and no mail or resume reaches a live
    /// session. Installed for one test; every child it answers is recorded.
    struct StubbedRun;

    static RUN_CALLS: std::sync::Mutex<Vec<Vec<String>>> = std::sync::Mutex::new(Vec::new());

    impl StubbedRun {
        fn install(answer: fn(&[String], &str) -> (i32, String, String)) -> Self {
            *STUBBED_RUN.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
            RUN_CALLS.lock().unwrap_or_else(|e| e.into_inner()).clear();
            Self
        }
    }

    impl Drop for StubbedRun {
        fn drop(&mut self) {
            *STUBBED_RUN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// How many recorded children mention `token`. A child another test
    /// runs inside this test's stub window is not this test's.
    fn calls_mentioning(token: &str) -> usize {
        RUN_CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|argv| argv.join(" ").contains(token))
            .count()
    }

    fn record(argv: &[String]) {
        RUN_CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(argv.to_vec());
    }

    /// The mail reached its recipient.
    fn reached(argv: &[String], _cwd: &str) -> (i32, String, String) {
        record(argv);
        (0, "injected".to_string(), String::new())
    }

    /// A durable queue, then a resume that fails: the wake did not land.
    fn durable_then_failed(argv: &[String], _cwd: &str) -> (i32, String, String) {
        record(argv);
        if argv.iter().any(|arg| arg == "send") {
            (0, "queued (durable)".to_string(), String::new())
        } else {
            (1, String::new(), String::new())
        }
    }

    fn journal_row(home: &crate::paths::AgentsHome, row: &Value) {
        crate::claims::append_event_line(
            &home.events_jsonl(),
            row,
            std::time::Duration::from_secs(2),
        )
        .expect("route row lands");
    }

    fn journal_text(home: &crate::paths::AgentsHome) -> String {
        crate::event_store::journal_text(&home.events_jsonl(), &["help_route"])
    }

    /// Every root a route step resolves, pinned under one temp dir: the
    /// state root, the spaces, the agents home the sweep reads, the claims
    /// and the transcript base. A failing assertion writes nothing outside.
    struct PinnedRoots {
        _home: crate::claims::EnvVarGuard,
        _spaces: crate::claims::EnvVarGuard,
        _agents: crate::claims::EnvVarGuard,
        _claims: crate::claims::EnvVarGuard,
        _projects: crate::claims::EnvVarGuard,
    }

    impl PinnedRoots {
        /// Pin under `dir` and name the agents home the sweep reads.
        fn under(dir: &Path) -> (Self, crate::paths::AgentsHome) {
            let at = |rel: &str| dir.join(rel).to_string_lossy().into_owned();
            (
                Self {
                    _home: crate::claims::EnvVarGuard::set("FNO_HOME", &dir.to_string_lossy()),
                    _spaces: crate::claims::EnvVarGuard::set("FNO_SPACES_DIR", &at("spaces")),
                    _agents: crate::claims::EnvVarGuard::set("FNO_AGENT_HOME", &at("agents")),
                    _claims: crate::claims::EnvVarGuard::set("FNO_CLAIMS_ROOT", &at("claims")),
                    _projects: crate::claims::EnvVarGuard::set(
                        crate::claude_drive::PROJECTS_DIR_ENV,
                        &at("projects"),
                    ),
                },
                crate::paths::AgentsHome::at(dir.join("agents")),
            )
        }
    }

    /// A route row written `secs` ago.
    fn aged_row(secs: i64, run: &str, node: &str, data: Value) -> Value {
        let ts = (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
        json!({
            "ts": ts,
            "v": 1,
            "type": "help_route",
            "source": "help-router",
            "run": run,
            "node": node,
            "data": data,
        })
    }

    /// A due timer wakes its session once. The `fired` row takes the
    /// timer's key, so the next sweep leaves the timer alone.
    #[test]
    fn a_due_timer_fires_once_and_records_its_leg() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        let _stub = StubbedRun::install(reached);
        journal_row(
            &home,
            &aged_row(
                600,
                "run-timer-once",
                "x-timer",
                json!({"class": "wait", "route": "timer", "to": "run-timer-once", "backoff_secs": 300, "turn": "turn-once"}),
            ),
        );
        sweep(&home, dir.path());
        sweep(&home, dir.path());
        let rows = journal_text(&home);
        assert_eq!(
            rows.matches("\"route\":\"fired\"").count(),
            1,
            "one fired row for one timer: {rows}"
        );
        assert!(
            rows.contains("\"delivery\":\"handed\"") && rows.contains("\"leg\":\"mail\""),
            "the fired row names the leg that landed: {rows}"
        );
        assert_eq!(
            calls_mentioning("run-timer-once"),
            1,
            "two sweeps, one wake"
        );
    }

    /// A wake whose live hop fails retires the timer after one try, with
    /// the leg that took it. Here the mail leg queued durably and the
    /// resume hop failed, so the row names the durable leg: the bytes land
    /// at the session's next drain, and a session that cannot be woken is
    /// not asked a second time.
    #[test]
    fn a_failed_timer_wake_retires_after_one_try() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        let _stub = StubbedRun::install(durable_then_failed);
        journal_row(
            &home,
            &aged_row(
                600,
                "run-timer-lost",
                "x-timer",
                json!({"class": "wait", "route": "timer", "to": "run-timer-lost", "backoff_secs": 300, "turn": "turn-lost"}),
            ),
        );
        sweep(&home, dir.path());
        sweep(&home, dir.path());
        let rows = journal_text(&home);
        assert_eq!(
            rows.matches("\"route\":\"fired\"").count(),
            1,
            "one fired row for one timer: {rows}"
        );
        assert!(
            rows.contains("\"delivery\":\"queued\"") && rows.contains("\"leg\":\"durable\""),
            "the fired row names the leg that took the wake: {rows}"
        );
        assert_eq!(
            calls_mentioning("run-timer-lost"),
            2,
            "two sweeps, and one try runs a mail and a resume"
        );
    }

    /// The read proof, admit arm: a delivered route whose recipient
    /// transcript holds its proof records `read`, and no sweep climbs it
    /// afterwards. A queued route counts the same as a handed one.
    #[test]
    fn a_proven_read_records_read_and_never_climbs() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        let projects = dir.path().join("projects").join("one");
        std::fs::create_dir_all(&projects).expect("projects dir");
        let recipient = "11111111-2222-4333-8444-555555555555";
        let run = "run-read-proof";
        let proofs = ["turn-read", "turn-queued"]
            .iter()
            .map(|turn| {
                format!(
                    "mail read: {}\n",
                    read_proof(run, HelpClass::Question, turn)
                )
            })
            .collect::<String>();
        std::fs::write(projects.join(format!("{recipient}.jsonl")), proofs)
            .expect("recipient transcript");
        let route = |turn: &str, delivery: &str| json!({"class": "question", "route": "off-session", "to": recipient, "delivery": delivery, "leg": "mail", "ladder": 0, "turn": turn});
        let _stub = StubbedRun::install(reached);
        let handed = route("turn-read", "handed");
        let queued = route("turn-queued", "queued");
        journal_row(&home, &aged_row(3600, run, "x-read", handed));
        journal_row(&home, &aged_row(3600, run, "x-read", queued));
        sweep(&home, dir.path());
        sweep(&home, dir.path());
        let rows = journal_text(&home);
        assert_eq!(
            rows.matches("\"route\":\"read\"").count(),
            2,
            "one read row per read route: {rows}"
        );
        assert!(
            !rows.contains("\"route\":\"climbed\""),
            "climbed a route the recipient read: {rows}"
        );
        assert_eq!(calls_mentioning(run), 0, "a read route starts no child");
    }

    /// The read proof, withhold arm: the same handed route climbs once its
    /// lease passes while its own proof is missing from the recipient's
    /// transcript. The climbed row keeps the node, so it takes the row's key
    /// and the next sweep leaves it alone.
    #[test]
    fn an_unread_route_climbs_once_on_its_own_key() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        let _stub = StubbedRun::install(reached);
        let unread = "22222222-2222-4333-8444-555555555555";
        // The recipient read an earlier route from this sender. That proof
        // sits in its transcript and must not read this route.
        let projects = dir.path().join("projects").join("one");
        std::fs::create_dir_all(&projects).expect("projects dir");
        let earlier = read_proof("run-climb-once", HelpClass::EnvDenied, "turn-earlier");
        std::fs::write(
            projects.join(format!("{unread}.jsonl")),
            format!("mail read: {earlier}\n"),
        )
        .expect("recipient transcript");
        journal_row(
            &home,
            &aged_row(
                3600,
                "run-climb-once",
                "x-climb",
                json!({"class": "env-denied", "route": "off-session", "to": unread, "delivery": "handed", "leg": "mail", "ladder": 0, "turn": "turn-climb"}),
            ),
        );
        sweep(&home, dir.path());
        sweep(&home, dir.path());
        let rows = journal_text(&home);
        assert_eq!(
            rows.matches("\"route\":\"climbed\"").count(),
            1,
            "one climb for one lease: {rows}"
        );
        assert!(
            rows.lines()
                .filter(|line| line.contains("\"route\":\"climbed\""))
                .all(|line| line.contains("\"node\":\"x-climb\"")),
            "the climbed row dropped its node: {rows}"
        );
        assert_eq!(
            calls_mentioning("run-climb-once"),
            1,
            "the second sweep climbed again"
        );
    }

    /// A holder the route addressed directly and never read the delivery
    /// hands the route to the ladder's first rung, the node lead, and not to
    /// the rung above it.
    #[test]
    fn an_unread_holder_hands_the_route_to_the_ladders_first_rung() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        std::fs::create_dir_all(dir.path().join("claims")).expect("claims root");
        let holder = "harness-silent-holder";
        let acquired = crate::claims::acquire(
            "node:x-silent-holder",
            "target-session:harness-silent-holder",
            crate::claims::AcquireOpts {
                pid: Some(std::process::id()),
                identity: Some((holder.to_string(), "claude".to_string())),
                root: None,
                events_dir: Some(dir.path().join("claim-events")),
                ..Default::default()
            },
        );
        assert!(
            matches!(acquired, crate::claims::AcquireOutcome::Acquired(_)),
            "the claim was not acquired: {acquired:?}"
        );
        let _stub = StubbedRun::install(reached);
        journal_row(
            &home,
            &aged_row(
                3600,
                "run-holder-unread",
                "x-silent-holder",
                json!({"class": "held", "route": "off-session", "to": holder, "delivery": "handed", "leg": "mail", "ladder": 0, "turn": "turn-holder"}),
            ),
        );
        sweep(&home, dir.path());
        let rows = journal_text(&home);
        assert_eq!(
            rows.matches("\"route\":\"climbed\"").count(),
            1,
            "one climb for one lease: {rows}"
        );
        let filings = RUN_CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|argv| argv.join(" "))
            .collect::<Vec<String>>();
        assert_eq!(
            filings.len(),
            1,
            "the climb ran an unexpected child: {filings:?}"
        );
        assert!(
            filings
                .iter()
                .all(|filing| filing.contains("ladder rung 0")),
            "the climb skipped the ladder's first rung: {filings:?}"
        );
    }

    /// A codex thread keeps no Claude transcript. Its rollout holds the mail,
    /// so the proof search reads that and the route records `read`.
    #[test]
    fn a_codex_recipient_proves_a_read_from_its_rollout() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        let _codex = crate::claims::EnvVarGuard::set(
            "CODEX_HOME",
            &dir.path().join("codex").to_string_lossy(),
        );
        let sessions = dir.path().join("codex").join("sessions");
        std::fs::create_dir_all(&sessions).expect("codex sessions");
        let recipient = "01a1fca9-28cc-7bd2-8d82-72796e1e2942";
        let run = "run-codex-read";
        std::fs::write(
            sessions.join(format!("rollout-{recipient}.jsonl")),
            format!(
                "mail read: {}\n",
                read_proof(run, HelpClass::Question, "turn-codex")
            ),
        )
        .expect("codex rollout");
        let _stub = StubbedRun::install(reached);
        journal_row(
            &home,
            &aged_row(
                3600,
                run,
                "x-codex",
                json!({"class": "question", "route": "off-session", "to": recipient, "delivery": "handed", "leg": "mail", "ladder": 0, "turn": "turn-codex"}),
            ),
        );
        sweep(&home, dir.path());
        let rows = journal_text(&home);
        assert!(
            rows.contains("\"route\":\"read\""),
            "no read row for a codex recipient: {rows}"
        );
        assert!(
            !rows.contains("\"route\":\"climbed\""),
            "climbed a route the codex recipient read: {rows}"
        );
        assert_eq!(calls_mentioning(run), 0, "a read route starts no child");
    }

    /// A timer route lands in the journal the sweep reads. The mirror went
    /// to another store, so no wait timer ever fired. The wake target is the
    /// harness session id, not the run id the row is filed under.
    #[test]
    fn an_emitted_timer_lands_in_the_journal_the_sweep_reads() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        route_emitted_distress(
            dir.path(),
            "run-mirror",
            Some("harness-mirror"),
            Some("x-mirror"),
            HelpClass::Wait,
            "ci pending",
            None,
            0,
            "turn-mirror",
        );
        let rows = journal_text(&home);
        assert!(
            rows.contains("\"route\":\"timer\"") && rows.contains("\"turn\":\"turn-mirror\""),
            "no timer row in the sweep's journal: {rows}"
        );
        assert!(
            rows.contains("\"to\":\"harness-mirror\""),
            "the wake target is not the harness session id: {rows}"
        );
    }

    /// A holder's own held help is no recipient. Under either of the
    /// sender's ids it records `holder-self` and starts no child, so it
    /// mails itself nothing and resumes itself never.
    #[test]
    fn a_holders_own_held_help_sends_nothing_to_itself() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let (_roots, home) = PinnedRoots::under(dir.path());
        std::fs::create_dir_all(dir.path().join("claims")).expect("claims root");
        let holder = "harness-own-holder";
        let acquired = crate::claims::acquire(
            "node:x-own-holder",
            "target-session:harness-own-holder",
            crate::claims::AcquireOpts {
                pid: Some(std::process::id()),
                identity: Some((holder.to_string(), "claude".to_string())),
                root: None,
                events_dir: Some(dir.path().join("claim-events")),
                ..Default::default()
            },
        );
        assert!(
            matches!(acquired, crate::claims::AcquireOutcome::Acquired(_)),
            "the claim was not acquired: {acquired:?}"
        );
        let _stub = StubbedRun::install(reached);
        let node = Some("x-own-holder");
        // Loop-check files under the manifest's run id and names the harness id.
        route_emitted_distress(
            dir.path(),
            "run-own-holder",
            Some(holder),
            node,
            HelpClass::Held,
            "parked by the lead",
            None,
            0,
            "turn-own-a",
        );
        // The stop hook files under the harness id.
        route_emitted_distress(
            dir.path(),
            holder,
            Some(holder),
            node,
            HelpClass::Held,
            "parked by the lead",
            None,
            1,
            "turn-own-b",
        );
        let self_mail = calls_mentioning(holder) + calls_mentioning("run-own-holder");
        assert_eq!(self_mail, 0, "the holder mailed or resumed itself");
        let rows = journal_text(&home);
        assert_eq!(
            rows.matches("\"route\":\"holder-self\"").count(),
            2,
            "one holder-self row per help: {rows}"
        );
        assert!(
            !rows.contains("\"route\":\"off-session\""),
            "routed the holder to itself: {rows}"
        );
        // No claim names a holder for this node. The route takes the ladder
        // from its first rung, and an empty ladder files from that rung.
        route_emitted_distress(
            dir.path(),
            "run-no-holder",
            Some("harness-no-sender"),
            Some("x-no-holder"),
            HelpClass::Held,
            "claim held",
            None,
            0,
            "turn-own-c",
        );
        assert_eq!(
            calls_mentioning("x-no-holder"),
            1,
            "one filing for the unreachable holder"
        );
        let filings = RUN_CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|argv| argv.join(" ").contains("x-no-holder"))
            .map(|argv| argv.join(" "))
            .collect::<Vec<String>>();
        assert!(
            filings
                .iter()
                .all(|filing| filing.contains("ladder rung 0")),
            "the route skipped the ladder and filed from its top: {filings:?}"
        );
    }

    /// An unreachable holder takes the lead ladder like any other help, so
    /// the user page is never its first stop. A lead recipient climbs one
    /// rung, and the top rung files.
    #[test]
    fn an_unreachable_holder_takes_the_ladder_before_the_user_page() {
        assert_eq!(
            unresolved_step(Recipient::Holder, 0),
            UnresolvedStep::TakeLadder
        );
        assert_eq!(
            unresolved_step(Recipient::EvidenceHolder, 0),
            UnresolvedStep::TakeLadder
        );
        assert_eq!(
            unresolved_step(Recipient::Lead, 0),
            UnresolvedStep::Climb,
            "a lead recipient climbs instead of starting over"
        );
        assert_eq!(
            unresolved_step(Recipient::Ladder, LADDER_TOP - 1),
            UnresolvedStep::Climb
        );
        assert_eq!(
            unresolved_step(Recipient::Ladder, LADDER_TOP),
            UnresolvedStep::Page
        );
    }

    /// Every class has a fixture line: the completeness gate that fails a
    /// new enum arm with no fixture, naming the class.
    #[test]
    fn every_class_has_a_fixture_line() {
        let all = [
            HelpClass::StalePlan,
            HelpClass::MissingPrereq,
            HelpClass::CiRed,
            HelpClass::Stuck,
            HelpClass::Held,
            HelpClass::Wait,
            HelpClass::EnvDenied,
            HelpClass::GateDeadlock,
            HelpClass::GateUnsatisfiable,
            HelpClass::Question,
            HelpClass::Budget,
            HelpClass::Unclassified,
        ];
        let fixture_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/distress");
        for class in all {
            let needle = format!("class=\\\"{}\\\"", class.as_str());
            let mut found = false;
            let entries = std::fs::read_dir(&fixture_dir)
                .unwrap_or_else(|e| panic!("fixture dir unreadable: {e}"));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                if body.contains(&needle) {
                    found = true;
                    break;
                }
            }
            assert!(
                found,
                "class {} has no fixture line in tests/fixtures/distress/",
                class.as_str()
            );
        }
    }

    /// The checked-in page matches the rendered page; HELP_ROUTING_BLESS=1
    /// rewrites it instead.
    #[test]
    fn checked_in_routing_page_matches_the_renderer() {
        let page_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/architecture/help-routing.md");
        let rendered = render_routing_page();
        if std::env::var("HELP_ROUTING_BLESS").ok().as_deref() == Some("1") {
            if let Some(parent) = page_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&page_path, &rendered).unwrap();
            return;
        }
        let checked_in = std::fs::read_to_string(&page_path).unwrap_or_default();
        assert_eq!(
            checked_in, rendered,
            "docs/architecture/help-routing.md is stale; rerun with HELP_ROUTING_BLESS=1"
        );
    }
}
