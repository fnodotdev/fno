//! The HQ Requests rows: one bounded, fail-open gather both the web bridge
//! and the mux org view share. The read model lives in the runtime crate as
//! `fno-agent compaction operator-turns --render requests --json` - the exact
//! argv `fno inbox user requests` execs - so this spawns it like the
//! mail-threads door does: rows on stdout, one bounded subprocess, a failed
//! read is an Err the caller folds into its last-good retention.

/// Gather the merged ask rows, rank order as the fold printed them.
pub async fn gather() -> Result<Vec<serde_json::Value>, String> {
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
        Err(_) => Err("requests read timed out after 30s".into()),
        Ok(Err(error)) => Err(format!("requests read could not run: {error}")),
        Ok(Ok(output)) if !output.status.success() => Err(format!(
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

/// The rows array off a successful read's stdout. The skip counts ride
/// stderr as one JSON line and are not part of the board shape.
fn parse_rows(bytes: &[u8]) -> Result<Vec<serde_json::Value>, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("requests read invalid JSON: {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_rows_reads_the_array_and_refuses_junk() {
        let rows = super::parse_rows(br#"[{"key":"node:x-1","ask_count":3}]"#).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("ask_count").and_then(serde_json::Value::as_i64),
            Some(3)
        );
        let err = super::parse_rows(b"no requests since 2026-11-14").unwrap_err();
        assert!(err.contains("invalid JSON"), "{err}");
    }
}
