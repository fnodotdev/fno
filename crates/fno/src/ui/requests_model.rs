//! The HQ Requests rows: one bounded, fail-open gather both the web bridge
//! and the mux org view share. The read model lives in the runtime crate as
//! `fno-agent compaction operator-turns --render requests --json` - the exact
//! argv `fno inbox user requests` execs - so this spawns it like the
//! mail-threads door does: rows on stdout, one bounded subprocess, a failed
//! read is `RequestsRead::Failed` and the caller folds it into its
//! last-good retention.

use serde::{Deserialize, Serialize};

/// One agent response inside an ask row: the session that answered, when,
/// the reply excerpt, and the transcript path that carries it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ResponseRef {
    pub session: String,
    pub name: String,
    pub ts: String,
    pub reply: String,
    pub transcript: String,
}

/// One merged ask row as the fold printed it, parsed once off the door's
/// stdout. Fields beyond these are the fold's own bookkeeping and are
/// skipped, so the struct carries only what the board surfaces read.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RequestRow {
    pub text: String,
    pub ask_count: u64,
    pub first_ts: String,
    pub last_ts: String,
    pub node: Option<String>,
    pub node_status: Option<String>,
    pub state: String,
    pub days_unaddressed: Option<i64>,
    pub reply_excerpt: String,
    pub responses: Vec<ResponseRef>,
    pub outcome: Option<String>,
    pub outcome_why: Option<String>,
}

/// A requests read's outcome: the ranked rows, or why the read failed.
#[derive(Debug)]
pub enum RequestsRead {
    Rows(Vec<RequestRow>),
    Failed(String),
}

/// Gather the merged ask rows, rank order as the fold printed them.
pub async fn gather() -> RequestsRead {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::process_admission::tokio_command(crate::digest_overlay::fno_agent_bin())
            .args([
                "compaction",
                "operator-turns",
                "--render",
                "requests",
                "--json",
            ])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Err(_) => RequestsRead::Failed("requests read timed out after 30s".into()),
        Ok(Err(error)) => RequestsRead::Failed(format!("requests read could not run: {error}")),
        Ok(Ok(output)) if !output.status.success() => RequestsRead::Failed(format!(
            "requests read exited {}: {}",
            output
                .status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "by signal".into()),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(Ok(output)) => parse_rows(&output.stdout),
    }
}

/// The rows off a successful read's stdout. The skip counts ride stderr as
/// one JSON line and are not part of the board shape.
fn parse_rows(bytes: &[u8]) -> RequestsRead {
    let values: Vec<serde_json::Value> = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return RequestsRead::Failed(format!("requests read invalid JSON: {e}")),
    };
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        match serde_json::from_value::<RequestRow>(value) {
            Ok(row) => rows.push(row),
            Err(e) => return RequestsRead::Failed(format!("requests read row shape: {e}")),
        }
    }
    RequestsRead::Rows(rows)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_rows_reads_the_array_and_refuses_junk() {
        let read = super::parse_rows(
            br#"[{"key":"node:x-1","ask_count":3,"outcome":"solved",
                 "responses":[{"session":"s1","name":"n1","ts":"2026-11-12T00:00:00Z",
                 "reply":"done","transcript":"/t/p.jsonl"}]}]"#,
        );
        let super::RequestsRead::Rows(rows) = read else {
            panic!("a well-shaped array parses");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().map(|r| r.ask_count), Some(3));
        let first = rows.first().unwrap();
        assert_eq!(first.outcome.as_deref(), Some("solved"));
        assert_eq!(
            first
                .responses
                .first()
                .map(|x| (x.session.as_str(), x.reply.as_str())),
            Some(("s1", "done"))
        );
        let failed = super::parse_rows(b"no requests since 2026-11-14");
        let super::RequestsRead::Failed(reason) = failed else {
            panic!("junk stdout refuses");
        };
        assert!(reason.contains("invalid JSON"), "{reason}");
    }
}
