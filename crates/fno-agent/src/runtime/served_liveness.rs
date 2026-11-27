//! The served-liveness freshness rule, vendored from
//! `crates/fno/src/sessions/served_liveness.rs`.
//!
//! fno links this crate, never the other way, so the rule lives twice. The
//! twin test in `crates/fno/tests/fno_agent_twins.rs` holds both copies to
//! one contract.

use std::time::Duration;

/// How often the daemon's serve-only tick re-measures the served pair.
/// The sweep's shared reads (the batched truth probe alone is near a
/// second cold) are too heavy to run continuously, so the tick runs on
/// this cadence and the freshness window tolerates one missed tick.
pub const SERVED_LIVENESS_CADENCE: Duration = Duration::from_secs(60);

/// A measurement older than twice the cadence no longer answers "is this
/// row alive NOW": one missed tick still serves, two do not. Republishing
/// an old word is what made a 24-hour-old `dead` read as current.
pub const SERVED_LIVENESS_MAX_AGE_SECS: u64 = 2 * 60;

pub(crate) fn fresh(measured_at_secs: Option<u64>, now_secs: u64) -> bool {
    measured_at_secs
        .and_then(|stamp| now_secs.checked_sub(stamp))
        .is_some_and(|age| age <= SERVED_LIVENESS_MAX_AGE_SECS)
}

/// The stored liveness word, served only while its stamp is fresh. Fresh
/// and measured serves the stored word; every other case reads `unknown` -
/// a probe that did not answer NOW is neither alive nor dead, and a null
/// made any alive filter count a full fleet as zero.
pub fn served_liveness_word<'a>(
    word: Option<&'a str>,
    measured_at_secs: Option<u64>,
    now_secs: u64,
) -> &'a str {
    match (fresh(measured_at_secs, now_secs), word) {
        (true, Some(w)) => w,
        _ => "unknown",
    }
}

/// Why the row's served `liveness` reads the way it does: `fresh` inside
/// the window, `stale` once a word and stamp exist but the window has
/// passed (the word reads `unknown`), `never-measured` with no word or no
/// stamp. A served field carries its basis; a null with no reason beside
/// it answers nothing.
pub fn served_liveness_basis(
    word: Option<&str>,
    measured_at_secs: Option<u64>,
    now_secs: u64,
) -> &'static str {
    match (word, measured_at_secs) {
        (Some(_), Some(_)) if fresh(measured_at_secs, now_secs) => "fresh",
        (Some(_), Some(_)) => "stale",
        _ => "never-measured",
    }
}
