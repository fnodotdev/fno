//! The `## Gate records` body section: consolidation, surface and
//! decisions_acknowledged leave frontmatter (Obsidian properties cannot show
//! nested maps) and land as one fenced YAML block under that fixed heading.
//! One reader lifts those keys into the map every validator sees. A key still
//! in frontmatter is legacy: it is read as today, and `lift` refuses the
//! both-places shape that would make two copies of one fact.

use serde_json::{Map, Value};
use serde_yaml_ng::Value as Yaml;

/// The heading, exactly. It sits after `## Execution Strategy`.
pub const GATE_SECTION: &str = "Gate records";

/// The only top-level keys the record fence may hold. Any other key refuses,
/// naming this list.
pub const GATE_KEYS: &[&str] = &["consolidation", "surface", "decisions_acknowledged"];

/// A `## Gate records` defect. `BothPlaces` names the key twice-held and the
/// command that fixes it; `UnknownKey` names the stranger in the fence.
#[derive(Debug, Clone)]
pub enum RecordsError {
    Malformed(String),
    UnknownKey(String),
    BothPlaces(String),
}

impl std::fmt::Display for RecordsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "## {GATE_SECTION} is malformed: {m}"),
            Self::UnknownKey(k) => write!(
                f,
                "## {GATE_SECTION} carries unknown key `{k}` - only {} may sit in the fence",
                GATE_KEYS.join(", ")
            ),
            Self::BothPlaces(k) => write!(
                f,
                "{k} is in both frontmatter and ## {GATE_SECTION} - run fno do plan \
                 migrate-keys --gate-records --apply"
            ),
        }
    }
}

/// The inner text of the section's first ```yaml fence. The opening fence
/// must carry the `yaml` info string; a fence of the same or greater backtick
/// run closes it.
fn fence_yaml(section_body: &str) -> Result<String, RecordsError> {
    let mut inner: Vec<&str> = Vec::new();
    let mut marker = String::new();
    let mut in_fence = false;
    let mut opened = false;
    for raw in section_body.lines() {
        let stripped = raw.trim();
        if !in_fence {
            if !stripped.starts_with("```") {
                continue;
            }
            let ticks: String = stripped.chars().take_while(|&c| c == '`').collect();
            let info = stripped[ticks.len()..].trim();
            if ticks != "```" || info != "yaml" {
                return Err(RecordsError::Malformed(
                    "the first fence must open with ```yaml".into(),
                ));
            }
            in_fence = true;
            opened = true;
            marker = ticks;
            continue;
        }
        let closes = stripped == marker
            || (stripped.starts_with(&marker)
                && stripped[marker.len()..].chars().all(|c| c == '`'));
        if closes {
            return Ok(inner.join("\n"));
        }
        inner.push(raw);
    }
    if opened {
        Err(RecordsError::Malformed(
            "the ```yaml fence never closes".into(),
        ))
    } else {
        Err(RecordsError::Malformed(
            "the heading carries no ```yaml fence".into(),
        ))
    }
}

/// Parse one record fence into the JSON mapping validators read.
fn parse_record(inner: &str) -> Result<Map<String, Value>, RecordsError> {
    let parsed: Yaml = serde_yaml_ng::from_str(inner).map_err(|e| {
        // A repeated key would silently discard the earlier value, the same
        // defect the frontmatter reader names.
        let text = e.to_string();
        if text.contains("duplicate entry with key") {
            let tail = &text[text.find("duplicate entry with key").unwrap_or(0)..];
            let key = tail.split('"').nth(1).unwrap_or("?").to_string();
            RecordsError::Malformed(format!(
                "more than one `{key}:` line - a repeated key silently discards the earlier value"
            ))
        } else {
            RecordsError::Malformed(format!("the fence does not parse as YAML: {e}"))
        }
    })?;
    let value = super::doc::yaml_to_json(&parsed);
    let map = value.as_object().cloned().ok_or_else(|| {
        RecordsError::Malformed(format!(
            "the fence must hold a YAML mapping, got {}",
            super::doc::yaml_type_name(&parsed)
        ))
    })?;
    if let Some(stranger) = map.keys().find(|k| !GATE_KEYS.contains(&k.as_str())) {
        return Err(RecordsError::UnknownKey(stranger.clone()));
    }
    Ok(map)
}

/// The gate records from already-split sections. `Ok(None)` when no
/// `## Gate records` section exists - a legacy plan, never an error.
pub fn records_from_sections(
    sections: &[(String, String)],
) -> Result<Option<Map<String, Value>>, RecordsError> {
    let Some((_, body)) = sections.iter().find(|(h, _)| h == GATE_SECTION) else {
        return Ok(None);
    };
    parse_record(&fence_yaml(body)?).map(Some)
}

/// The gate records from whole plan text (the fence-aware split runs here).
pub fn body_records(text: &str) -> Result<Option<Map<String, Value>>, RecordsError> {
    records_from_sections(&super::doc::extract_sections(text))
}

/// Merge the body records into `frontmatter`, refusing a key held in both
/// places. The result is what validators read; it is never a map writers
/// re-emit.
pub fn lift(
    frontmatter: &Map<String, Value>,
    text: &str,
) -> Result<Map<String, Value>, RecordsError> {
    lift_from_sections(frontmatter, &super::doc::extract_sections(text))
}

/// `lift` over sections the caller already split (PlanDocument keeps them).
pub fn lift_from_sections(
    frontmatter: &Map<String, Value>,
    sections: &[(String, String)],
) -> Result<Map<String, Value>, RecordsError> {
    let Some(records) = records_from_sections(sections)? else {
        return Ok(frontmatter.clone());
    };
    let mut out = frontmatter.clone();
    for key in GATE_KEYS {
        let in_fm = frontmatter.get(*key).is_some_and(|v| !v.is_null());
        match (records.get(*key), in_fm) {
            (Some(_), true) => return Err(RecordsError::BothPlaces((*key).to_string())),
            (Some(v), false) => {
                out.insert((*key).to_string(), v.clone());
            }
            (None, _) => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC1-HP: a body-only plan lifts; the raw frontmatter stays clean.
    #[test]
    fn body_records_lift_and_frontmatter_stays_raw() {
        let text = "---\nstatus: ready\nnode: x-1\n---\n\n## Overview\n\nbody\n\n## Gate records\n\n```yaml\nconsolidation:\n  outcome: proceed_alone\n  proceed_alone_against: []\nsurface:\n  question: \"why?\"\n  sweep: \"grep\"\n  answerers:\n    - at: a.rs:1 check\n  count: 1\n  count_after: 0\n```\n";
        let doc = super::super::doc::load_plan_text(text).unwrap();
        let v = serde_json::Value::Object(doc.validation_map().unwrap());
        assert_eq!(
            v.pointer("/consolidation/outcome")
                .and_then(serde_json::Value::as_str),
            Some("proceed_alone")
        );
        assert_eq!(
            v.pointer("/surface/count")
                .and_then(serde_json::Value::as_i64),
            Some(1)
        );
        assert!(!doc
            .frontmatter
            .as_object()
            .unwrap()
            .contains_key("consolidation"));
        // Legacy form: no section reads as the frontmatter itself.
        let legacy = super::super::doc::load_plan_text(
            "---\nstatus: ready\nconsolidation:\n  outcome: absorb\n---\nbody\n",
        )
        .unwrap();
        let v = serde_json::Value::Object(legacy.validation_map().unwrap());
        assert_eq!(
            v.pointer("/consolidation/outcome")
                .and_then(serde_json::Value::as_str),
            Some("absorb")
        );
    }

    /// AC1-ERR: both places, a stranger key, and a malformed fence refuse.
    #[test]
    fn both_places_unknown_key_and_malformed_fences_refuse() {
        let both = "---\nstatus: ready\nconsolidation:\n  outcome: absorb\n---\n\n## Gate records\n\n```yaml\nconsolidation:\n  outcome: proceed_alone\n```\n";
        let doc = super::super::doc::load_plan_text(both).unwrap();
        let err = doc.validation_map().unwrap_err();
        assert!(err.to_string().contains(
            "consolidation is in both frontmatter and ## Gate records - run fno do plan migrate-keys --gate-records --apply"
        ), "{err}");
        let stranger = "---\nstatus: ready\n---\n\n## Gate records\n\n```yaml\nkill_criteria:\n  - name: x\n```\n";
        let doc = super::super::doc::load_plan_text(stranger).unwrap();
        assert!(matches!(
            doc.validation_map(),
            Err(RecordsError::UnknownKey(ref k)) if k == "kill_criteria"
        ));
        let malformed =
            "---\nstatus: ready\n---\n\n## Gate records\n\n```text\nconsolidation: x\n```\n";
        let doc = super::super::doc::load_plan_text(malformed).unwrap();
        assert!(matches!(
            doc.validation_map(),
            Err(RecordsError::Malformed(_))
        ));
        // A `## Gate records` quoted inside another fence is body, not the
        // section (this plan's own Changes block quotes the heading).
        let quoted = "---\nstatus: ready\n---\n\n## Changes\n\n````md\n## Gate records\n\n```yaml\nconsolidation: x\n```\n````\n";
        let doc = super::super::doc::load_plan_text(quoted).unwrap();
        assert!(!doc.validation_map().unwrap().contains_key("consolidation"));
        assert_eq!(body_records("---\n---\nno section\n").unwrap(), None);
    }
}
