//! The activity-feed shell-out leg : a bounded, fail-open shell-out to
//! `fno agent fleet feed --json`, in the same shape as [`crate::org_overlay`]'s
//! fold (: the 800ms cap it first copied was half the projection's
//! measured runtime, so the feed could never render).
//!
//! The projection itself lives in `fno-agent` (`feed.rs`): it joins
//! questions.jsonl and graph.json into ordered rows, so no second
//! implementation of the join can drift from the command's. This module only
//! carries the row shape the client renders and the one bounded call that
//! fetches it - off the UI loop, and a typed [`FeedError`] instead of a bare
//! `None` so the client can say WHICH failure fired.

pub use fno_proto::feed::row::{event_fields, owned, EventCtx, FeedItem, IStr};
use std::time::Duration;

/// Ten seconds, org_overlay's budget for a comparable multi-store read.
/// The feed joins three stores and serialises 200 rows; its measured runtime
/// is 1.6s against the 800ms this file inherited from needs_overlay's cheap
/// reads, so under the old cap it could not succeed at all. A long budget
/// costs nothing: the fold runs off the UI loop, one at a time, and
/// `kill_on_drop` reaps the child on the timeout.
const SHELLOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a feed fold failed. Each variant is a different user action - retune a
/// budget, free an admission slot, read the projection's stderr - which is
/// why they stopped collapsing into one `None`.
#[derive(Debug, Clone)]
pub enum FeedError {
    /// The projection outlived [`SHELLOUT_TIMEOUT`].
    Timeout,
    /// The projection never ran: spawn failure or a process-admission
    /// refusal. Carries the io error's own words.
    Spawn(String),
    /// The projection ran and exited non-zero. Carries its stderr.
    Exit(String),
    /// stdout was not the row array. Carries the serde error and stderr.
    Malformed(String),
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedError::Timeout => {
                write!(f, "feed timed out after {}s", SHELLOUT_TIMEOUT.as_secs())
            }
            FeedError::Spawn(e) => write!(f, "feed could not start: {e}"),
            FeedError::Exit(stderr) => write!(f, "feed exited non-zero: {stderr}"),
            FeedError::Malformed(e) => write!(f, "feed returned malformed JSON: {e}"),
        }
    }
}

/// The projection's stderr, first line, capped for the overlay footer. The
/// ONE shaping rule for stderr in this file: both failure renders go through
/// it, so an Exit line and a Malformed line cannot drift apart.
fn stderr_first_line(stderr: &[u8], none_label: &str) -> String {
    std::str::from_utf8(stderr)
        .ok()
        .and_then(|text| text.lines().next())
        .filter(|line| !line.is_empty())
        .unwrap_or(none_label)
        .chars()
        .take(160)
        .collect()
}

/// The Exit variant's stderr note; a stderr that says nothing (empty, or not
/// text at all) degrades to the exit's own spelling so the line never dangles.
fn stderr_note(stderr: &[u8], status: &std::process::ExitStatus) -> String {
    if std::str::from_utf8(stderr).is_ok() {
        stderr_first_line(stderr, "no stderr")
    } else {
        format!("{status}")
    }
}

/// What one fold lands: the full rows of a one-shot read (a shot, a test),
/// oldest first, or one answer from the live panel's held reader.
#[derive(Debug)]
pub enum Land {
    Rows(Vec<FeedItem>),
    Answer(Box<fno_proto::feed::serve::Answer>),
}

/// One fold result, as it travels the client's single-flight channel.
pub type FoldResult = Result<Land, FeedError>;

/// The `--limit` that reads every row: the projection keeps the newest
/// `limit`, and no history comes near this.
pub const FEED_HISTORY_ROWS: usize = 10_000_000;

/// The argv of the panel's one read: the whole history as a JSON array.
pub fn history_argv() -> Vec<String> {
    vec![
        "agent".to_string(),
        "fleet".to_string(),
        "feed".to_string(),
        "--json".to_string(),
        "--limit".to_string(),
        FEED_HISTORY_ROWS.to_string(),
    ]
}

/// Read the whole feed history, `Err` carrying the typed reason on any
/// failure. An empty history is `Ok` with no rows - a real answer.
pub async fn fetch_history() -> Result<Vec<FeedItem>, FeedError> {
    let mut command = crate::process_admission::tokio_command(crate::server::fno_bin());
    command
        .args(history_argv())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let output = tokio::time::timeout(SHELLOUT_TIMEOUT, fut)
        .await
        .map_err(|_| FeedError::Timeout)?
        .map_err(|e| FeedError::Spawn(e.to_string()))?;
    if !output.status.success() {
        return Err(FeedError::Exit(stderr_note(&output.stderr, &output.status)));
    }
    parse_feed(&output.stdout, &output.stderr)
}

fn parse_feed(stdout: &[u8], stderr: &[u8]) -> Result<Vec<FeedItem>, FeedError> {
    serde_json::from_slice::<Vec<FeedItem>>(stdout).map_err(|e| {
        FeedError::Malformed(format!(
            "{e}; stderr: {}",
            stderr_first_line(stderr, "none")
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_deserializes() {
        let body = br#"[{"ts":"2026-09-02T17:12:52Z","kind":"node_started","node":"x-aaaa","session_id":"s-do","harness":"claude","title":"port the claim classifier","ref":null},{"ts":"2026-09-02T18:27:06Z","kind":"pr_created","node":"x-aaaa","session_id":"s-ship","title":"PR 1395","ref":"1395"}]"#;
        let items = parse_feed(body, b"").expect("a well-formed body parses");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, "node_started");
        assert_eq!(items[0].session_id.as_deref(), Some("s-do"));
        assert_eq!(items[1].r#ref.as_deref(), Some("1395"));
        // Both rows name node x-aaaa: one shared allocation, not two copies.
        let node_ptr = |i: usize| {
            items
                .get(i)
                .and_then(|it| it.node.as_deref())
                .map(str::as_ptr)
        };
        assert!(
            node_ptr(0).is_some() && node_ptr(0) == node_ptr(1),
            "a repeated node interns once"
        );
    }
    #[test]
    fn garbage_is_malformed_carrying_stderr() {
        let err = parse_feed(b"not json", b"skipped 3 malformed question lines\n")
            .expect_err("garbage is a typed error, not a silent None");
        assert!(matches!(err, FeedError::Malformed(_)));
        let rendered = err.to_string();
        assert!(rendered.contains("malformed JSON"));
        assert!(rendered.contains("skipped 3 malformed question lines"));
    }

    // AC5 and premise 12: the panel's one read spells the current command
    // and reads every row; no cursor rides it, so `--after ''`, which the
    // projection refuses with exit 2, can never be sent.
    #[test]
    fn history_argv_reads_every_row_with_no_cursor() {
        let argv = history_argv();
        assert!(argv.starts_with(&["agent".into(), "fleet".into(), "feed".into()]));
        assert!(argv.ends_with(&["--limit".into(), FEED_HISTORY_ROWS.to_string()]));
        assert!(!argv.iter().any(|a| a == "--after" || a == "--before"));
    }

    // regression: the timeout names itself and its duration, so the
    // rendered line is a budget statement, never the old generic failure.
    #[test]
    fn timeout_renders_the_budget_it_outran() {
        let line = FeedError::Timeout.to_string();
        assert!(line.contains("timed out after 10s"), "got: {line}");
        assert!(!line.contains("failed"));
    }
}
