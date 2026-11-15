//! The lead check-in's Visual Log tap reader probe: is the fleet-owned
//! reader session alive before the beat asks the user to tap.
//!
//! The reader sat down for ~13h on 2026-11-14 and the user's taps were lost
//! while leads kept asking for them (lead-cf20 retro, w9 21:40:45). The probe
//! is `ensure-reader.sh check`, run by the beat so the lead sees the reader's
//! state in the same output that names any tap ask. The script is the one
//! probe implementation; the beat runs it, it never reimplements the verdicts.

use crate::lead_checkin_beat_budget::ReadResult;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The beat's probe reading. A hard failure is `Err`, so the beat prints
/// READER FAILED; every measured state lands as a payload.
pub(crate) fn reading() -> ReadResult {
    let Some(script) = ensure_reader_script() else {
        return Err(
            "ensure-reader.sh not found (env roots, install pointer, checkout copy)".to_string(),
        );
    };
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script).arg("check");
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, VISUAL_LOG_PROBE_BUDGET_S)
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
fn ensure_reader_script() -> Option<PathBuf> {
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
            return Some(path);
        }
    }
    None
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

/// The ask-time gate: a tap asked while the reader is down is a tap nobody
/// records, so the ask refuses and names the repair. A fresh reader lets it
/// through (`None`). An unrunnable probe refuses too: an unmeasured reader is
/// no proof a tap will land.
pub(crate) fn tap_ask_refusal(probe: &ReadResult) -> Option<String> {
    let detail = match probe {
        Ok(v) if v.get("status").and_then(Value::as_str) == Some("fresh") => return None,
        Ok(v) => v
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("reader down")
            .to_string(),
        Err(e) => e.clone(),
    };
    Some(format!(
        "the tap ask is refused: {detail}; repair first with `bash skills/visual-log-reader/scripts/ensure-reader.sh spawn`, then ask"
    ))
}

#[cfg(test)]
mod visual_log_reader_tests {
    use super::*;

    #[test]
    fn a_down_reader_refuses_the_tap_ask_and_a_fresh_one_lets_it_through() {
        let down = Ok(json!({"status": "down", "detail": "reader absent from the registry"}));
        let refusal = tap_ask_refusal(&down).expect("a down reader refuses");
        assert!(refusal.contains("reader absent"), "{refusal}");
        assert!(refusal.contains("ensure-reader.sh spawn"), "{refusal}");
        assert!(tap_ask_refusal(&Err("ensure-reader.sh not found".into())).is_some());
        let fresh = Ok(json!({"status": "fresh", "detail": "reader fresh"}));
        assert_eq!(tap_ask_refusal(&fresh), None);
    }

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
