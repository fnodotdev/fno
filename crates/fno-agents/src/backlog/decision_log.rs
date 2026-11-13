//! The readable decision log: one markdown file beside the project's plans
//! dir (`<plans dir>/../decision-log.md`), rewritten on every law write,
//! retract, restore and renew. `fno backlog decisions --log` rewrites it on
//! demand and prints its path. The decision store stays the record; this file
//! is where a person reads what was decided.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

/// A retired ruling stays in the log this long, so a reader sees what died.
const RETIRED_DAYS: i64 = 30;

/// The log's path for the project at the working directory.
pub(crate) fn log_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let anchor = crate::paths::canonical_repo_root(&cwd).unwrap_or(cwd);
    let plans = crate::plans_path::plans_content_dir(&anchor)?;
    Some(plans.parent()?.join("decision-log.md"))
}

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn is_ruling(row: &Value) -> bool {
    matches!(field(row, "lane"), "law" | "unattributed")
}

fn bullet(row: &Value) -> String {
    let id = match field(row, "number") {
        "" => field(row, "decision_id"),
        n => n,
    };
    let day = field(row, "ts").get(..10).unwrap_or("");
    let mut line = format!(
        "- **{id}** {day}, {}, {}: {}",
        field(row, "authority_source"),
        match field(row, "scope") {
            "" => "project",
            s => s,
        },
        crate::law_match::one_line(field(row, "decision"), 600)
    );
    if let Some(by) = row.get("superseded_by").and_then(Value::as_str) {
        line.push_str(&format!(" (superseded by {by})"));
    }
    line
}

/// The log's markdown over decision rows (newest first, the listing order).
pub(crate) fn render(rows: &[Value], now: chrono::DateTime<chrono::Utc>) -> String {
    let rulings: Vec<&Value> = rows.iter().filter(|r| is_ruling(r)).collect();
    let live: Vec<&Value> = rulings
        .iter()
        .copied()
        .filter(|r| field(r, "lifecycle") == "live")
        .collect();
    // Subjects keep the order of their newest live ruling.
    let mut order: Vec<&str> = Vec::new();
    let mut by_subject: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in &live {
        let subject = match field(row, "subject") {
            "" => "(no subject)",
            s => s,
        };
        if !by_subject.contains_key(subject) {
            order.push(subject);
        }
        by_subject.entry(subject).or_default().push(row);
    }
    let cutoff = (now - chrono::Duration::days(RETIRED_DAYS))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let retired: Vec<&Value> = rulings
        .iter()
        .copied()
        .filter(|r| {
            matches!(
                field(r, "lifecycle"),
                "superseded" | "retracted" | "expired"
            )
        })
        .filter(|r| field(r, "ts") >= cutoff.as_str())
        .collect();
    let mut out = format!(
        "---\ntitle: Decision log\ngenerated: {}\nsource: fno backlog decisions\nnote: generated on every law write; regenerate with `fno backlog decisions --log`, never edit\n---\n\n# Decision log\n\n{} live ruling(s) under {} subject(s). Read one with `fno backlog decisions <id>`.\n\n## Live rulings\n",
        now.format("%Y-%m-%dT%H:%M:%SZ"),
        live.len(),
        order.len()
    );
    for subject in order {
        out.push_str(&format!("\n### {subject}\n\n"));
        for row in &by_subject[subject] {
            out.push_str(&bullet(row));
            out.push('\n');
        }
    }
    out.push_str(&format!("\n## Retired in the last {RETIRED_DAYS} days\n\n"));
    if retired.is_empty() {
        out.push_str("None.\n");
    }
    for row in retired {
        out.push_str(&format!("{} [{}]\n", bullet(row), field(row, "lifecycle")));
    }
    out
}

/// Rewrite the log. `Err` names what failed.
pub(crate) fn write_log() -> Result<PathBuf, String> {
    let path = log_path().ok_or("no plans dir resolves for this project")?;
    if cfg!(test) && !crate::paths::under_temp_dir(&path) {
        return Err(format!("{} is outside the test temp dir", path.display()));
    }
    let rows = crate::backlog::decisions_cli::all_rows_for_log()?;
    let text = render(&rows, chrono::Utc::now());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The best-effort rewrite after a law write: a failure is one stderr line.
pub(crate) fn refresh() {
    if let Err(reason) = write_log() {
        eprintln!("law: decision log not rewritten ({reason})");
    }
}

/// [`refresh`] after a door that changed a law's state; the door's exit code
/// passes through.
pub(crate) fn refresh_after(code: i32) -> i32 {
    if code == 0 {
        refresh();
    }
    code
}

/// `fno backlog decisions --log`.
pub(crate) fn run_log() -> i32 {
    match write_log() {
        Ok(path) => {
            println!("{}", path.display());
            0
        }
        Err(reason) => {
            eprintln!("backlog decisions: cannot write the decision log: {reason}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_log_lists_live_rulings_by_subject_and_recent_retirements() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z")
            .expect("ts")
            .with_timezone(&chrono::Utc);
        let rows = vec![
            json!({"decision_id": "d-8c65f3c6", "number": "D3322", "lane": "law",
                   "lifecycle": "live", "subject": "release:fno-app-version",
                   "ts": "2026-11-12T23:48:18Z", "authority_source": "chat_attested",
                   "scope": "project:fno", "decision": "fno-app has its own version line"}),
            json!({"decision_id": "d-afb64731", "number": "D3310", "lane": "unattributed",
                   "lifecycle": "retracted", "subject": "x-dddd",
                   "ts": "2026-11-12T08:30:14Z", "authority_source": "role",
                   "decision": "fno-app versions in lockstep with fno"}),
            json!({"decision_id": "d-00000001", "lane": "coord", "lifecycle": "live",
                   "subject": "pr-1", "ts": "2026-11-12T08:30:14Z", "decision": "hold"}),
            json!({"decision_id": "d-00000002", "lane": "law", "lifecycle": "superseded",
                   "subject": "old", "ts": "2026-08-01T00:00:00Z", "decision": "too old"}),
        ];
        let text = render(&rows, now);
        assert!(
            text.contains("1 live ruling(s) under 1 subject(s)"),
            "{text}"
        );
        assert!(text.contains("### release:fno-app-version\n\n- **D3322** 2026-11-12, chat_attested, project:fno: fno-app has its own version line\n"), "{text}");
        assert!(text.contains("- **D3310** 2026-11-12, role, project: fno-app versions in lockstep with fno [retracted]"), "{text}");
        assert!(!text.contains("hold"), "coordination rows stay out");
        assert!(
            !text.contains("too old"),
            "a retirement past the window drops"
        );
    }
}
