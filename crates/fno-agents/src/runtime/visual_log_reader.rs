//! The lead check-in's Visual Log tap reader probe: is the fleet-owned
//! reader session alive before the beat asks the user to tap.
//!
//! The reader sat down for ~13h on 2026-11-14 and the user's taps were lost
//! while leads kept asking for them (lead-cf20 retro, w9 21:40:45). The probe
//! is `ensure-reader.sh check`, run by the beat so the lead sees the reader's
//! state in the same output that names any tap ask. The script is the one
//! probe implementation; the beat runs it, it never reimplements the verdicts.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The beat's probe reading. A hard failure is `Err`, so the beat prints
/// READER FAILED; every measured state lands as a payload.
pub(crate) fn reading() -> Result<Value, String> {
    let script = ensure_reader_script()?;
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script).arg("check");
    let out = crate::bounded_cmd::output_with_timeout(cmd, VISUAL_LOG_PROBE_BUDGET_S)
        .map_err(|e| format!("ensure-reader.sh check failed: {e}"))?;
    Ok(visual_log_verdict(
        out.status.code().unwrap_or(1),
        &String::from_utf8_lossy(&out.stdout),
    ))
}

/// The wall clock for the reader probe: the script reads one registry listing,
/// far under any pipe buffer; the bound exists for a wedged filesystem.
const VISUAL_LOG_PROBE_BUDGET_S: u64 = 15;

/// The script ladder: an env-named plugin root, then the state root's
/// install pointer, then the canonical checkout's own copy. A missing script
/// is a failed reading (loud), never a quiet fresh.
fn ensure_reader_script() -> Result<PathBuf, String> {
    let rel = Path::new("skills")
        .join("visual-log-reader")
        .join("scripts")
        .join("ensure-reader.sh");
    let home = crate::paths::AgentsHome::from_env();
    let pointer = crate::reclaim::reclaim_state_root(&home)
        .join("install")
        .join("plugin-root");
    let pointer = std::fs::read_to_string(&pointer)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let candidates: [Option<String>; 3] = [
        std::env::var("CLAUDE_PLUGIN_ROOT")
            .ok()
            .or_else(|| std::env::var("CODEX_PLUGIN_ROOT").ok())
            .or_else(|| std::env::var("FNO_REPO_ROOT").ok()),
        pointer,
        crate::paths::canonical_repo_root(&std::env::current_dir().unwrap_or_default())
            .map(|r| r.to_string_lossy().into_owned()),
    ];
    for root in candidates.into_iter().flatten() {
        let path = Path::new(&root).join(&rel);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!(
        "ensure-reader.sh not found (env roots, {}, checkout copy)",
        pointer
            .map(|_| "install pointer".to_string())
            .unwrap_or_else(|| "no install pointer".to_string())
    ))
}

/// The probe's verdict, from the script's exit code alone: exit 0 is fresh,
/// anything else is down. The script's own first output line rides the
/// payload as the detail, so the rendered line still names which state it
/// is without this module keying on the script's English wording.
fn visual_log_verdict(exit: i32, stdout: &str) -> Value {
    let detail = stdout.lines().next().unwrap_or("").trim().to_string();
    let status = if exit == 0 { "fresh" } else { "down" };
    json!({"status": status, "detail": detail})
}

/// The beat line. A state other than fresh carries the one rule the reform
/// asks for: no tap ask while the reader is down.
pub(crate) fn line(v: &Value) -> String {
    let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("unknown");
    let detail = field("detail");
    let detail = detail.strip_prefix("reader ").unwrap_or(detail);
    match field("status") {
        "fresh" => format!("visual log reader: {detail}"),
        _ => format!(
            "visual log reader: {detail}; do not ask the user for a tap until the repair ran"
        ),
    }
}

#[cfg(test)]
mod visual_log_reader_tests {
    use super::*;

    #[test]
    fn the_probe_maps_exit_to_fresh_or_down() {
        assert_eq!(
            visual_log_verdict(0, "reader fresh: last activity 120s ago (bound 1200s)\n"),
            json!({"status": "fresh", "detail": "reader fresh: last activity 120s ago (bound 1200s)"})
        );
        assert_eq!(
            visual_log_verdict(1, "reader absent from the registry; repair: spawn\n"),
            json!({"status": "down", "detail": "reader absent from the registry; repair: spawn"})
        );
        assert_eq!(
            visual_log_verdict(
                1,
                "reader stale: last activity 9000s ago (bound 1200s); repair: resume\n"
            ),
            json!({"status": "down", "detail": "reader stale: last activity 9000s ago (bound 1200s); repair: resume"})
        );
        assert_eq!(
            visual_log_verdict(
                1,
                "reader present but its last activity is unreadable; check with: peek\n"
            ),
            json!({"status": "down", "detail": "reader present but its last activity is unreadable; check with: peek"})
        );
    }

    #[test]
    fn the_down_line_names_the_tap_rule_and_fresh_says_nothing_extra() {
        assert_eq!(
            line(&json!({"status": "fresh", "detail": "reader fresh: last activity 120s ago"})),
            "visual log reader: fresh: last activity 120s ago"
        );
        assert_eq!(
            line(&json!({"status": "down", "detail": "reader absent from the registry; repair: spawn"})),
            "visual log reader: absent from the registry; repair: spawn; do not ask the user for a tap until the repair ran"
        );
    }
}
