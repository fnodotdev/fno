//! The `claim main-red` action: one repo's red main is repaired by exactly
//! one fixer. The first fixer takes the `main-red:<owner/repo>` claim (a
//! global-id key, so every worktree reads the same claim); the next fixer's
//! run names the holder and backs off; a green main releases the caller's
//! own claim. Born from a lead retro's reform: five duplicate main repairs
//! in one night. The take is never decided from the pre-read alone:
//! `claims::acquire` is the atomic authority, so a race resolves to one
//! owner and one back-off.

use serde_json::{json, Map, Value};
use std::path::Path;

use super::{node_aware_root, parse_ttl_expression, ttl_ms_checked};
use crate::claims::{self, AcquireOpts, AcquireOutcome, ClaimState};

/// The claim TTL default, in minutes: `ci.main_red_claim_ttl_minutes`. Long
/// enough for a real repair, short enough that an abandoned claim frees
/// itself for the next fixer instead of holding main hostage.
const DEFAULT_TTL_MINUTES: i64 = 120;

/// The decision table's action half. The exit code rides beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainRedAction {
    /// Acquire the key and start the repair.
    Take,
    /// A live holder owns the repair: name it and back off.
    BackOff,
    /// Main is green and the caller holds the claim: release it.
    ReleaseOwn,
    /// Nothing to do: main is not red and no owned claim sits on the key.
    Nothing,
}

/// The pure decision core: (main is red, claim state, the caller holds the
/// claim) -> (action, exit). `Corrupted` never reaches here; the shell
/// refuses it before the table runs, because no table row can repair a
/// corrupt claim file.
pub(crate) fn decide(
    main_red: bool,
    state: ClaimState,
    holder_matches: bool,
) -> (MainRedAction, i32) {
    match (main_red, state, holder_matches) {
        // A repair is claimable: free, or the prior holder provably died
        // (Stale). The engine's acquire stays the atomic authority; this row
        // only decides the attempt.
        (true, ClaimState::Free | ClaimState::Stale, _) => (MainRedAction::Take, 0),
        // Red and already owned by a live holder: the reform's whole point.
        (true, ClaimState::Live | ClaimState::Suspect, false) => (MainRedAction::BackOff, 1),
        // Red and the caller already holds it: re-acquire is idempotent and
        // refreshes the lease.
        (true, ClaimState::Live | ClaimState::Suspect, true) => (MainRedAction::Take, 0),
        // Main recovered and the caller still holds the claim: put it down.
        (false, ClaimState::Live | ClaimState::Suspect, true) => (MainRedAction::ReleaseOwn, 2),
        // Main is fine and someone else's claim lingers: report it, touch
        // nothing - the claim frees at TTL expiry.
        (false, ClaimState::Live | ClaimState::Suspect, false) => (MainRedAction::Nothing, 2),
        // Main is fine, nothing held.
        (false, ClaimState::Free | ClaimState::Stale, _) => (MainRedAction::Nothing, 2),
        // Unreachable: the shell refuses Corrupted before the table runs.
        // The arm exists for exhaustiveness; its exit names the repair path.
        (_, ClaimState::Corrupted, _) => (MainRedAction::Nothing, 3),
    }
}

/// `ci.main_red_claim_ttl_minutes` through the shared global-then-local
/// overlay (review_hold owns it; this is the only other leaf that reads one
/// config key through it).
fn configured_ttl_minutes(cwd: &Path) -> i64 {
    crate::review_hold::config_leaf(cwd, |s| s.ci_main_red_claim_ttl_minutes_opt())
        .unwrap_or(DEFAULT_TTL_MINUTES)
}

struct LeafArgs {
    holder: String,
    ttl: String,
    json_output: bool,
}

pub fn run(args: &[String]) -> i32 {
    let mut a = LeafArgs {
        holder: String::new(),
        ttl: String::new(),
        json_output: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--holder" => match it.next() {
                Some(v) => a.holder = v.clone(),
                None => return usage_flag("--holder"),
            },
            "--ttl" => match it.next() {
                Some(v) => a.ttl = v.clone(),
                None => return usage_flag("--ttl"),
            },
            "--json" | "-J" => a.json_output = true,
            other => {
                if other.starts_with('-') && other != "-" {
                    eprintln!("fno-agents: claim: unknown flag {other}");
                    return 2;
                }
                eprintln!("fno-agents: claim: main-red takes no positional argument ({other})");
                return 2;
            }
        }
    }
    run_parsed(a)
}

fn usage_flag(name: &str) -> i32 {
    eprintln!("fno-agents: claim: {name} requires a value");
    2
}

fn run_parsed(mut a: LeafArgs) -> i32 {
    // The holder names who owns the repair, exactly like acquire's default:
    // the caller's own ambient session id, refused (not guessed) when no
    // harness identity resolves.
    if a.holder.is_empty() {
        let (ambient_session, _harness) = claims::resolve_identity();
        match ambient_session {
            Some(session_id) => a.holder = session_id,
            None => {
                eprintln!(
                    "validation error: --holder is required when no harness session \
                     identity is ambient; run inside a harness session or pass \
                     --holder <session-id>"
                );
                return 2;
            }
        }
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("main-red: cannot read the working directory: {e}");
            return 4;
        }
    };
    let ttl_ms = match parse_ttl_expression(&a.ttl) {
        Ok(Some(v)) => match ttl_ms_checked(v) {
            Ok(ms) => ms,
            Err(msg) => {
                eprintln!("validation error: {msg}");
                return 2;
            }
        },
        Ok(None) => configured_ttl_minutes(&cwd)
            .saturating_mul(60_000)
            .clamp(claims::MIN_TTL_MS, claims::MAX_TTL_MS),
        Err(msg) => {
            eprintln!("validation error: {msg}");
            return 2;
        }
    };
    let Some(slug) = crate::finalize::slug_from_git_remote(&cwd) else {
        eprintln!(
            "main-red: cannot resolve the repo slug from the origin remote; \
             a red main is claimed per repo (main-red:<owner/repo>)"
        );
        return 4;
    };
    let key = format!("main-red:{slug}");

    // The reading composes the message and the green path; a failed read is
    // an Err, never a manufactured verdict (main_ci's own contract).
    let reading = crate::gate::main_ci::main_ci_reading_cached(&cwd);
    let token = match reading {
        Ok(t) => t,
        Err(e) => {
            eprintln!("main-red: main CI read failed: {e}");
            return 4;
        }
    };
    let red_run = crate::gate::main_ci::main_ci_red_run(&token);
    let verdict = token
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    // A corrupt claim file repairs no red: refuse and name the recovery.
    let (state, record) = match claims::status_checked(&key, None) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("main-red: claim read failed: {e}");
            return 3;
        }
    };
    if state == ClaimState::Corrupted {
        eprintln!(
            "main-red: the claim file for {key} is corrupted; repair with \
             `fno agents claim release --force {key} -R \"corrupted main-red claim\"`"
        );
        return 3;
    }
    let holder_matches = record
        .as_ref()
        .map(|r| r.holder == a.holder)
        .unwrap_or(false);
    let held_minutes = record
        .as_ref()
        .map(|r| ((claims::now_ms() - r.acquired_at) / 60_000).max(0))
        .unwrap_or(0);
    let record_holder = record
        .as_ref()
        .map(|r| r.holder.clone())
        .unwrap_or_default();

    let (action, exit) = decide(red_run.is_some(), state, holder_matches);
    match action {
        MainRedAction::Take => take(&a, &key, &slug, ttl_ms, red_run.as_ref()),
        MainRedAction::BackOff => {
            let line = json!({
                "key": key, "action": "back_off", "verdict": verdict,
                "holder": record_holder, "held_minutes": held_minutes,
            });
            if a.json_output {
                println!("{line}");
            } else {
                eprintln!(
                    "main-red: {key} is being repaired by {record_holder} \
                     (held {held_minutes}m); back off"
                );
            }
            exit
        }
        MainRedAction::ReleaseOwn => {
            let released = claims::release(&key, &a.holder, node_aware_root(&key).as_deref(), None);
            let line = json!({
                "key": key, "action": "release", "verdict": verdict,
                "holder": a.holder,
                "released": released.is_ok(),
            });
            if a.json_output {
                println!("{line}");
            } else {
                match released {
                    Ok(()) => println!("main-red: {key} released (main is green)"),
                    Err(e) => eprintln!("main-red: {key} release failed: {e}"),
                }
            }
            exit
        }
        MainRedAction::Nothing => {
            let line = json!({
                "key": key, "action": "none", "verdict": verdict,
                "holder": record_holder, "held_minutes": held_minutes,
            });
            if a.json_output {
                println!("{line}");
            } else if state == ClaimState::Free || state == ClaimState::Stale {
                println!("main-red: main is {verdict}; nothing to claim");
            } else {
                println!(
                    "main-red: main is {verdict}; {key} still held by {record_holder} \
                     (held {held_minutes}m) - left for its holder"
                );
            }
            exit
        }
    }
}

/// The take: `claims::acquire` decides atomically. A race this pre-read lost
/// still lands on the honest arm (one owner, one back-off).
fn take(
    a: &LeafArgs,
    key: &str,
    slug: &str,
    ttl_ms: i64,
    red_run: Option<&(String, String)>,
) -> i32 {
    // The pid anchors to the durable session like every operator claim;
    // a TTL claim without a resolvable session records explicit pid
    // absence instead of a transient pid that dies at exit.
    let pid: Option<u32> = claims::open_session_pid().map(|p| p as u32);
    let pid_unavailable = pid.is_none();
    let mut metadata = Map::new();
    metadata.insert("repo".into(), Value::String(slug.to_string()));
    if let Some((wf, sha)) = red_run {
        metadata.insert("workflow".into(), Value::String(wf.clone()));
        metadata.insert("sha".into(), Value::String(sha.clone()));
    }
    let (ambient_session, ambient_harness) = claims::resolve_identity();
    let opts = AcquireOpts {
        pid,
        pid_unavailable,
        ttl_ms: Some(ttl_ms),
        reason: Some("main-red repair claim".into()),
        metadata: Some(metadata),
        pid_provenance: Some("ambient".into()),
        root: node_aware_root(key),
        host: None,
        events_dir: None,
        identity: ambient_session
            .clone()
            .map(|s| (s, ambient_harness.clone().unwrap_or_default())),
    };
    match claims::acquire(key, &a.holder, opts) {
        AcquireOutcome::Acquired(_claim) => {
            let ttl_word = format!("{}m", ttl_ms / 60_000);
            let line = json!({
                "key": key, "action": "take", "verdict": "red",
                "holder": a.holder, "ttl_minutes": ttl_ms / 60_000,
            });
            if a.json_output {
                println!("{line}");
            } else {
                println!(
                    "main-red: you own main's red ({key}, ttl {ttl_word}). Fix it, \
                     land the repair, and release when green: \
                     fno agents claim release {key}"
                );
            }
            0
        }
        AcquireOutcome::HeldByOther { holder, .. } => {
            let line = json!({
                "key": key, "action": "back_off", "verdict": "red",
                "holder": holder,
            });
            if a.json_output {
                println!("{line}");
            } else {
                eprintln!("main-red: {key} is being repaired by {holder}; back off");
            }
            1
        }
        AcquireOutcome::Error(e) => {
            eprintln!("main-red: transient error: {e}");
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(main_red: bool, state: ClaimState, holder_matches: bool) -> (MainRedAction, i32) {
        decide(main_red, state, holder_matches)
    }

    /// The decision table is the reform: one fixer takes a red main, the
    /// next backs off, a green main never keeps the caller's claim. Each
    /// row pins one cell of that table.
    #[test]
    fn the_decision_table_answers_take_back_off_and_release() {
        // Red main, free or stale claim: the first fixer takes it.
        assert_eq!(row(true, ClaimState::Free, false), (MainRedAction::Take, 0));
        assert_eq!(
            row(true, ClaimState::Stale, false),
            (MainRedAction::Take, 0)
        );
        // Red main, live foreign holder: back off, exit 1.
        assert_eq!(
            row(true, ClaimState::Live, false),
            (MainRedAction::BackOff, 1)
        );
        assert_eq!(
            row(true, ClaimState::Suspect, false),
            (MainRedAction::BackOff, 1)
        );
        // Red main, own live claim: idempotent re-acquire.
        assert_eq!(row(true, ClaimState::Live, true), (MainRedAction::Take, 0));
        assert_eq!(
            row(true, ClaimState::Suspect, true),
            (MainRedAction::Take, 0)
        );
        // Green main, own claim: release it, exit 2.
        assert_eq!(
            row(false, ClaimState::Live, true),
            (MainRedAction::ReleaseOwn, 2)
        );
        assert_eq!(
            row(false, ClaimState::Suspect, true),
            (MainRedAction::ReleaseOwn, 2)
        );
        // Green main, foreign claim: report, touch nothing.
        assert_eq!(
            row(false, ClaimState::Live, false),
            (MainRedAction::Nothing, 2)
        );
        // Green main, nothing held.
        assert_eq!(
            row(false, ClaimState::Free, false),
            (MainRedAction::Nothing, 2)
        );
        assert_eq!(
            row(false, ClaimState::Stale, true),
            (MainRedAction::Nothing, 2)
        );
        // Corrupted never reaches the table (the shell refuses it first);
        // the exhaustiveness arm answers the repair exit.
        assert_eq!(
            row(true, ClaimState::Corrupted, false),
            (MainRedAction::Nothing, 3)
        );
    }
}
