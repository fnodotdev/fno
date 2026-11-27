//! The `claim refresh` leaf, ported from `cli/src/fno/claims/cli.py::refresh`
//! plus the two shapes `core.refresh_claim` kept behind its routing: a
//! global-id key (session:, node:, ...) ran the legacy in-process body
//! (`core._legacy_refresh_claim`: holder mismatch is a named exit 4, a
//! missing file is `claim missing`, the ttl is range-checked) and a
//! repo-local key ran the native leg (a status pre-read, then the engine
//! `renew`, where every benign refusal - wrong holder, gone, corrupted -
//! collapses into the PID-liveness no-op). Both shapes drive the same engine
//! write here; the routing only picks the error taxonomy and the pre-reads.
//!
//! Output contract: `refreshed: {key} (new expires_at=N)` or the claim JSON
//! under `--json`; the PID-liveness no-op line / `{"key", "refreshed",
//! "reason": "pid_liveness"}` receipt. Exit codes: 0 refreshed or no-op; 1
//! write failure; 2 validation; 3 missing/corrupted; 4 holder mismatch.

use serde_json::json;

use super::{claim_json_string, node_aware_root, parse_ttl_expression, ttl_ms_checked};
use crate::claims::{self, ClaimState, MIN_TTL_MS};

struct LeafArgs {
    key: Option<String>,
    holder: Option<String>,
    json_output: bool,
}

/// What a refresh did: the renewed record, or the PID-liveness no-op.
pub(crate) enum Refreshed {
    Renewed(crate::claims::ClaimRecord),
    NoOp,
}

pub fn run(args: &[String]) -> i32 {
    let mut a = LeafArgs {
        key: None,
        holder: None,
        json_output: false,
    };
    let mut ttl_raw = String::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--holder" => match it.next() {
                Some(v) => a.holder = Some(v.clone()),
                None => return usage_error("Error: Option '--holder' requires an argument."),
            },
            "--ttl" => match it.next() {
                Some(v) => ttl_raw = v.clone(),
                None => return usage_error("Error: Option '--ttl' requires an argument."),
            },
            "--json" | "-J" => a.json_output = true,
            other => {
                if other.starts_with('-') && other != "-" {
                    eprintln!("fno-agent: claim: unknown flag {other}");
                    return 2;
                }
                if a.key.is_some() {
                    eprintln!("fno-agent: claim: unexpected extra argument {other}");
                    return 2;
                }
                a.key = Some(other.to_string());
            }
        }
    }
    // typer refused an absent required option with the usage layout (the
    // frozen goldens pin the lines); an empty --holder VALUE reached the
    // engine validation instead.
    let Some(key) = a.key.clone() else {
        return usage_error("Error: Missing argument 'KEY'.");
    };
    let Some(holder) = a.holder.clone() else {
        return usage_error("Error: Missing option '--holder'.");
    };
    match refresh(&key, &holder, &ttl_raw) {
        Ok(Refreshed::Renewed(r)) => {
            if a.json_output {
                println!("{}", claim_json_string(&r));
            } else {
                println!(
                    "refreshed: {key} (new expires_at={})",
                    r.expires_at.unwrap_or_default()
                );
            }
            0
        }
        Ok(Refreshed::NoOp) => no_op(&key, a.json_output),
        Err(code) => code,
    }
}

/// The leaf's engine without its stdout: an `Err` is the leaf's exit code,
/// with the reason already on stderr. The claim heartbeat calls it in
/// process.
pub(crate) fn refresh(key: &str, holder: &str, ttl_raw: &str) -> Result<Refreshed, i32> {
    if key.is_empty() || holder.is_empty() {
        eprintln!("validation error: key and holder must be non-empty");
        return Err(2);
    }
    let ttl_ms = match parse_ttl_expression(ttl_raw) {
        Ok(v) => v,
        Err(e) => return Err(bad_parameter(&e)),
    };
    if node_aware_root(key).is_some() {
        run_global(ttl_ms, key, holder)
    } else {
        run_local(ttl_ms, key, holder)
    }
}

fn usage_error(detail: &str) -> i32 {
    super::usage_refusal(
        "fno agent claim refresh [OPTIONS] KEY",
        "fno agent claim refresh --help",
        detail,
    )
}

fn bad_parameter(msg: &str) -> i32 {
    usage_error(&format!("Error: Invalid value: {msg}"))
}

/// The legacy-shaped leg (`core._legacy_refresh_claim`): the named holder
/// mismatch, the range-checked ttl, and a direct file pre-read before the
/// engine extend.
fn run_global(ttl_ms: Option<i128>, key: &str, holder: &str) -> Result<Refreshed, i32> {
    let root = node_aware_root(key);
    // The legacy body range-checked BEFORE touching the file, with its own
    // message (the engine only refuses non-positive); the range check also
    // narrows the i128 to the engine's i64.
    let ttl = match ttl_ms {
        Some(t) => {
            if t < i128::from(MIN_TTL_MS) || t > i128::from(claims::MAX_TTL_MS) {
                eprintln!(
                    "validation error: ttl_ms={t} out of range [{}, {}]",
                    MIN_TTL_MS,
                    claims::MAX_TTL_MS
                );
                return Err(2);
            }
            t as i64
        }
        None => MIN_TTL_MS,
    };
    let path = match claims::claim_path(key, root.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("validation error: {e}");
            return Err(2);
        }
    };
    let record = match claims::read_claim_file(&path) {
        Ok(r) => r,
        Err(claims::ReadError::GoneAway) => {
            eprintln!("claim missing: {}", path.display());
            return Err(3);
        }
        Err(claims::ReadError::Corrupted(e)) => {
            eprintln!("corrupted claim: {e}");
            return Err(3);
        }
    };
    if record.holder != holder {
        eprintln!(
            "holder mismatch: claim '{key}': holder mismatch (expected '{holder}', got '{}')",
            record.holder
        );
        return Err(4);
    }
    if record.expires_at.is_none() {
        return Ok(Refreshed::NoOp);
    }
    // STALE is the only refused verdict, matching the engine's own renew
    // gate; the named refusal is the legacy body's message.
    if crate::claim_commands::status_verdict(&record).0 == ClaimState::Stale {
        eprintln!(
            "validation error: claim '{key}' expired and its holder reads dead; \
             refusing to resurrect it"
        );
        return Err(2);
    }
    finish(key, holder, root.as_deref(), ttl, 1)
}

/// The native-shaped leg (`core.refresh_claim`'s native path): with an
/// explicit ttl it goes straight to the engine renew (a stale lease then
/// reads as the benign no-op); with an EMPTY --ttl it status-pre-reads
/// first (free names the gone-away, stale refuses, a PID-liveness claim
/// no-ops) and renews by the MIN window. Benign renew refusals all read as
/// the PID-liveness no-op exactly as the Python core returned None.
fn run_local(ttl_ms: Option<i128>, key: &str, holder: &str) -> Result<Refreshed, i32> {
    if let Some(t) = ttl_ms {
        if t <= 0 {
            eprintln!("validation error: ttl_ms must be positive");
            return Err(2);
        }
        let ttl = match ttl_ms_checked(t) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("validation error: {e}");
                return Err(2);
            }
        };
        return finish(key, holder, None, ttl, 3);
    }
    let (state, record) = claims::status(key, None);
    match state {
        ClaimState::Free => {
            let path = claims::claim_path(key, None);
            match path {
                Ok(p) => eprintln!("claim missing: {}", p.display()),
                Err(e) => eprintln!("claim missing: {e}"),
            }
            return Err(3);
        }
        ClaimState::Corrupted => {
            // The status payload carries no parse detail for a corrupted
            // record, so the core's `error or key` fallback names the key.
            eprintln!("corrupted claim: {key}");
            return Err(3);
        }
        ClaimState::Stale => {
            eprintln!("validation error: claim '{key}' expired and cannot be refreshed");
            return Err(2);
        }
        _ => {}
    }
    if record.and_then(|r| r.expires_at).is_none() {
        return Ok(Refreshed::NoOp);
    }
    finish(key, holder, None, MIN_TTL_MS, 3)
}

/// Extend through the engine. `err_exit` is the exit code the leg's
/// taxonomy gives an engine write failure (legacy surfaced a raw failure as
/// exit 1; the native leg read a renew refusal as a verdict error, exit 3).
/// A benign `Ok(false)` after the pre-reads passed means the record moved
/// under us or a peer holds the recovery mutex - the no-op surface is the
/// leaf's only None shape, so it takes that.
fn finish(
    key: &str,
    holder: &str,
    root: Option<&std::path::Path>,
    ttl_ms: i64,
    err_exit: i32,
) -> Result<Refreshed, i32> {
    match claims::renew(key, holder, ttl_ms, root) {
        Ok(true) => Ok(match claims::status(key, root).1 {
            Some(r) => Refreshed::Renewed(r),
            None => Refreshed::NoOp,
        }),
        Ok(false) => Ok(Refreshed::NoOp),
        Err(e) => {
            if err_exit == 3 {
                eprintln!("native verdict unavailable: fno-agent claim renew failed: {e}");
            } else {
                eprintln!("refresh failed: {e}");
            }
            Err(err_exit)
        }
    }
}

fn no_op(key: &str, json_output: bool) -> i32 {
    if json_output {
        println!(
            "{}",
            json!({"key": key, "refreshed": false, "reason": "pid_liveness"})
        );
    } else {
        println!("no-op for PID-liveness claim: {key}");
    }
    0
}
