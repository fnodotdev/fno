//! The lead check-in's `skill_use` reading: of the sessions that ran
//! `fno:target` in the window, how many also loaded `fno:verify-footnotes`
//! and `fno:review`. Measured 2026-11-15 over 122 worker transcripts: 60
//! invoked review and none invoked verify-footnotes, while the name sat in
//! 116 of them as injected text. Naming a skill is not firing it; this
//! counts the `skill_invoked` rows the rules hook writes on each Skill call.

use crate::lead_checkin_beat_budget::ReadResult;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// One `skill_invoked` row; rows written before the hook carried the
/// session have no `session` and count nowhere.
#[derive(serde::Deserialize)]
struct SkillRow {
    #[serde(default)]
    data: SkillData,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct SkillData {
    skill: Option<String>,
    session: Option<String>,
}

/// How far back the reading looks when the beat names no previous one.
const DEFAULT_WINDOW_MS: i64 = 86_400_000;

pub(crate) fn reading(events_paths: &[PathBuf], since: Option<&str>) -> ReadResult {
    let since_ms = match since {
        Some(ts) => chrono::DateTime::parse_from_rfc3339(ts)
            .map_err(|e| format!("previous beat ts unreadable: {e}"))?
            .timestamp_millis(),
        None => chrono::Utc::now().timestamp_millis() - DEFAULT_WINDOW_MS,
    };
    let query = crate::event_store::EventQuery {
        types: vec!["skill_invoked".into()],
        since_ms: Some(since_ms),
        ..Default::default()
    };
    let mut by_skill: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for path in events_paths {
        for row in crate::event_store::query_events(path, &query).unwrap_or_default() {
            let Ok(SkillRow { data }) = serde_json::from_str::<SkillRow>(&row.line) else {
                continue;
            };
            let (Some(skill), Some(session)) = (data.skill, data.session) else {
                continue;
            };
            by_skill.entry(skill).or_default().insert(session);
        }
    }
    let count = |skill: &str| by_skill.get(skill).map_or(0, BTreeSet::len);
    Ok(json!({
        "target_sessions": count("fno:target"),
        "verify_footnotes": count("fno:verify-footnotes"),
        "review": count("fno:review"),
    }))
}

pub(crate) fn lines(readings: &[crate::lead_checkin::Reading]) -> Vec<String> {
    let Some(reading) = readings.iter().find(|r| r.name == "skill_use") else {
        return Vec::new();
    };
    if !reading.ok {
        return vec![format!("READER FAILED skill_use: {}", reading.error)];
    }
    let n = |k: &str| reading.value.get(k).and_then(Value::as_u64).unwrap_or(0);
    let sessions = n("target_sessions");
    vec![format!(
        "skill_use: verify-footnotes {}/{sessions}, review {}/{sessions} target sessions this window",
        n("verify_footnotes"),
        n("review"),
    )]
}
