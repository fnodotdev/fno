//! One GitHub-read retry helper for the gate readers.
//!
//! On 2026-11-16 a GitHub 502 burst turned green heads red: each gate
//! reader treats a failed `gh api` read as data, and one transient
//! failure decided a verdict. The cure is shared, not per reader: every
//! reader routes its GitHub reads through [`retried_read`] with
//! [`transient`] as the classifier, exactly as `scripts/ci/ci-required.sh`
//! retries its own reads with its `gh_read_retry`.
//!
//! The contract: a failed GitHub read is an absent result, never a
//! verdict. A read retries what GitHub itself is failing - a named
//! `(HTTP 5xx)`, a `(HTTP 429)`, or an error with no HTTP status at all
//! (DNS, reset, timeout). A named other status is GitHub answering no
//! (auth, permissions, a missing ref); it will not heal, so the read
//! gives up at once. An exhausted read returns the last attempt's error
//! verbatim, so a caller's message never changes shape.
//!
//! Writes never retry: a 5xx on a mutation may have taken effect
//! server-side, and replaying it could double the write. [`transient`]
//! classifies error text only; the caller decides which failures are
//! reads.

/// Attempts per read, backing off from [`READ_BACKOFF_S`] and doubling
/// (2s, 4s, 8s, 16s: 30s across five tries). The same budget
/// `ci-required.sh` applies through its retry knobs.
pub(crate) const READ_ATTEMPTS: u32 = 5;
pub(crate) const READ_BACKOFF_S: u64 = 2;

/// True when `err` is GitHub itself failing rather than GitHub refusing:
/// a named `(HTTP 5xx)`, a named `(HTTP 429)`, or no named status at all.
/// Any other named status returns false.
pub(crate) fn transient(err: &str) -> bool {
    let mut rest = err;
    let mut named = false;
    while let Some(i) = rest.find("(HTTP ") {
        let after = &rest[i + "(HTTP ".len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() == 3 && after[digits.len()..].starts_with(')') {
            named = true;
            if digits.starts_with('5') || digits == "429" {
                return true;
            }
        }
        rest = after;
    }
    // No named status anywhere: transport failure (DNS, reset, timeout),
    // not a refusal.
    !named
}

/// One read, retried while GitHub itself is failing. `attempt` produces
/// the read or its error; `is_transient` says whether an error is GitHub
/// failing (retry) or GitHub refusing (give up now). `sleep` waits
/// between attempts, starting at `backoff_s` and doubling; callers inject
/// it so tests record waits instead of taking them. The returned error is
/// the last attempt's, verbatim.
pub(crate) fn retried_read<T, E>(
    attempts: u32,
    backoff_s: u64,
    sleep: &mut dyn FnMut(u64),
    mut attempt: impl FnMut() -> Result<T, E>,
    is_transient: impl Fn(&E) -> bool,
) -> Result<T, E> {
    let rounds = attempts.max(1);
    let mut delay = backoff_s;
    let mut done = 0u32;
    loop {
        let last = done + 1 >= rounds;
        match attempt() {
            body @ Ok(_) => return body,
            Err(err) if !is_transient(&err) => return Err(err),
            Err(err) => {
                done += 1;
                if last {
                    return Err(err);
                }
                sleep(delay);
                delay = delay.saturating_mul(2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_failing_retries_github_refusing_does_not() {
        assert!(transient("gh: Server Error (HTTP 502)"));
        assert!(transient("gh: Gateway Timeout (HTTP 504)"));
        assert!(transient("gh: API rate limit exceeded (HTTP 429)"));
        assert!(transient("dial tcp: lookup github.com: no such host"));
        assert!(transient("read: connection reset by peer"));
        assert!(transient("(HTTP 403) then later (HTTP 502)"));
        assert!(!transient(
            "gh: Resource not accessible by integration (HTTP 403)"
        ));
        assert!(!transient("gh: Not Found (HTTP 404)"));
        assert!(!transient("(HTTP 404) and (HTTP 422)"));
    }

    #[test]
    fn a_flaky_read_recovers_within_the_budget() {
        let mut calls = 0u32;
        let mut waits: Vec<u64> = Vec::new();
        let sleep = &mut |secs: u64| waits.push(secs);
        let got = retried_read(
            3,
            1,
            sleep,
            || {
                calls += 1;
                if calls < 3 {
                    Err(calls)
                } else {
                    Ok("read")
                }
            },
            |err| *err > 0,
        );
        assert_eq!(got, Ok("read"));
        assert_eq!(calls, 3);
        assert_eq!(waits, vec![1, 2]);
    }

    #[test]
    fn an_exhausted_read_returns_the_last_error() {
        let mut calls = 0u32;
        let sleep = &mut |_secs: u64| {};
        let got: Result<(), u32> = retried_read(
            2,
            0,
            sleep,
            || {
                calls += 1;
                Err(calls)
            },
            |err| *err > 0,
        );
        assert_eq!(got, Err(2));
        assert_eq!(calls, 2);
    }

    #[test]
    fn a_refused_read_gives_up_at_once() {
        let mut calls = 0u32;
        let sleep = &mut |_secs: u64| {};
        let got: Result<(), u32> = retried_read(
            5,
            0,
            sleep,
            || {
                calls += 1;
                Err(0)
            },
            |err| *err > 0,
        );
        assert_eq!(got, Err(0));
        assert_eq!(calls, 1);
    }

    #[test]
    fn zero_attempts_still_reads_once() {
        let mut calls = 0u32;
        let sleep = &mut |_secs: u64| {};
        let got: Result<(), u32> = retried_read(
            0,
            0,
            sleep,
            || {
                calls += 1;
                Err(7)
            },
            |_| true,
        );
        assert_eq!(got, Err(7));
        assert_eq!(calls, 1);
    }
}
