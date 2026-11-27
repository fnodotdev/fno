//! The `## Gate records` body section: the nested and list keys (the gate
//! blocks, kill_criteria, done_probes, code_index, urls, session_ids and
//! sources) leave frontmatter (Obsidian properties cannot show nested maps)
//! and land as one fenced YAML block under that fixed heading. One reader
//! lifts those keys into the map every validator sees. A key still in
//! frontmatter is legacy: it is read as today, and `lift` refuses the
//! both-places shape that would make two copies of one fact.

use serde_json::{Map, Value};
use serde_yaml_ng::Value as Yaml;

/// The heading, exactly. It sits after `## Execution Strategy`.
pub const GATE_SECTION: &str = "Gate records";

/// The only top-level keys the record fence may hold. Any other key refuses,
/// naming this list.
pub const GATE_KEYS: &[&str] = &[
    "consolidation",
    "surface",
    "decisions_acknowledged",
    "kill_criteria",
    "done_probes",
    "code_index",
    "urls",
    "session_ids",
    "sources",
];

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

/// The raw inner text of the record fence, for the line readers that keep
/// their own text-parity contract (kill-check, the probe parser). `None` when
/// the section is absent or malformed; `body_records` names the defect.
pub fn fence_text(text: &str) -> Option<String> {
    let sections = super::doc::extract_sections(text);
    let (_, body) = sections.iter().find(|(h, _)| h == GATE_SECTION)?;
    fence_yaml(body).ok()
}

/// A string list from the record fence: `None` when the key is not there, a
/// scalar as one item. Used by the stamp, which reads lists, not YAML.
pub fn body_list(text: &str, key: &str) -> Result<Option<Vec<String>>, RecordsError> {
    let Some(records) = body_records(text)? else {
        return Ok(None);
    };
    Ok(records.get(key).map(|v| match v {
        Value::String(s) if s.is_empty() => Vec::new(),
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().map(|i| super::doc::py_str(Some(i))).collect(),
        _ => Vec::new(),
    }))
}

/// `key:` plus one `  - item` line per item, each item a YAML-safe scalar.
pub fn list_block(key: &str, items: &[String]) -> String {
    if items.is_empty() {
        return format!("{key}: []");
    }
    let mut out = format!("{key}:");
    for item in items {
        let scalar = serde_yaml_ng::to_string(item)
            .map(|t| t.trim_end().to_string())
            .unwrap_or_else(|_| format!("{item:?}"));
        out.push_str("\n  - ");
        out.push_str(&scalar);
    }
    out
}

/// The top-level key of a fence line: `name:` at column 0.
fn top_key(line: &str) -> Option<&str> {
    let (key, _) = line.split_once(':')?;
    (!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(key)
}

/// The (opening, closing) line indexes of the record fence in `lines`,
/// skipping a heading quoted inside another fence. `Ok(None)` when the body
/// has no section.
fn fence_span(lines: &[&str]) -> Result<Option<(usize, usize)>, RecordsError> {
    let mut outer_fence: Option<String> = None;
    let mut heading = None;
    for (i, raw) in lines.iter().enumerate() {
        let stripped = raw.trim();
        if let Some(marker) = &outer_fence {
            if stripped.starts_with(marker.as_str())
                && stripped
                    .chars()
                    .all(|c| c == marker.chars().next().unwrap_or('`'))
            {
                outer_fence = None;
            }
            continue;
        }
        if stripped.starts_with("```") || stripped.starts_with("~~~") {
            let first = stripped.chars().next().unwrap_or('`');
            outer_fence = Some(stripped.chars().take_while(|&c| c == first).collect());
            continue;
        }
        // The last heading wins, as in extract_sections: the fence every
        // reader lifts is the one the writer edits.
        if raw.starts_with("## ") && raw[3..].trim_end() == GATE_SECTION {
            heading = Some(i);
        }
    }
    let Some(h) = heading else {
        return Ok(None);
    };
    let mut open = None;
    for (i, raw) in lines.iter().enumerate().skip(h + 1) {
        let stripped = raw.trim();
        match open {
            None if raw.starts_with("## ") => break,
            None if stripped == "```yaml" => open = Some(i),
            None => {}
            Some(o) if stripped.starts_with("```") && stripped.chars().all(|c| c == '`') => {
                return Ok(Some((o, i)));
            }
            Some(_) => {}
        }
    }
    Err(RecordsError::Malformed(
        "the section carries no closed ```yaml fence".into(),
    ))
}

/// Replace each `key:` block inside the record fence, or append it at the
/// fence end; a body with no section gains one at its end. Line-level, so
/// every other block in the fence keeps its bytes. Each block is the full
/// `key:` text (see `list_block`).
pub fn set_blocks(body: &str, blocks: &[(&str, String)]) -> Result<String, RecordsError> {
    let lines: Vec<&str> = body.lines().collect();
    let Some((open, close)) = fence_span(&lines)? else {
        let inner: Vec<&str> = blocks.iter().map(|(_, b)| b.as_str()).collect();
        return Ok(format!(
            "{}\n\n## {GATE_SECTION}\n\n```yaml\n{}\n```\n",
            body.trim_end_matches('\n'),
            inner.join("\n")
        ));
    };
    let mut inner: Vec<String> = lines
        .iter()
        .skip(open + 1)
        .take(close - open - 1)
        .map(|l| l.to_string())
        .collect();
    for (key, block) in blocks {
        let new_lines: Vec<String> = block.lines().map(str::to_string).collect();
        match inner.iter().position(|l| top_key(l) == Some(*key)) {
            Some(start) => {
                let mut end = start + 1;
                while inner.get(end).is_some_and(|l| top_key(l).is_none()) {
                    end += 1;
                }
                while end > start + 1 && inner.get(end - 1).is_some_and(|l| l.trim().is_empty()) {
                    end -= 1;
                }
                inner.splice(start..end, new_lines);
            }
            None => inner.extend(new_lines),
        }
    }
    let mut out: Vec<String> = lines.iter().take(open + 1).map(|l| l.to_string()).collect();
    out.extend(inner);
    out.extend(lines.iter().skip(close).map(|l| l.to_string()));
    let mut text = out.join("\n");
    if body.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
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

    fn load_plan_text_map(text: &str) -> Map<String, Value> {
        let doc = super::super::doc::load_plan_text(text).unwrap();
        let mut map = doc.validation_map().unwrap();
        map.remove("status");
        map
    }

    /// AC1-HP + AC1-ERR: the moved keys lift to their frontmatter values; both
    /// places, a stranger key, and a malformed fence refuse.
    #[test]
    fn both_places_unknown_key_and_malformed_fences_refuse() {
        let both = "---\nstatus: ready\nconsolidation:\n  outcome: absorb\n---\n\n## Gate records\n\n```yaml\nconsolidation:\n  outcome: proceed_alone\n```\n";
        let doc = super::super::doc::load_plan_text(both).unwrap();
        let err = doc.validation_map().unwrap_err();
        assert!(err.to_string().contains(
            "consolidation is in both frontmatter and ## Gate records - run fno do plan migrate-keys --gate-records --apply"
        ), "{err}");
        let stranger =
            "---\nstatus: ready\n---\n\n## Gate records\n\n```yaml\nwaves:\n  - 1\n```\n";
        let doc = super::super::doc::load_plan_text(stranger).unwrap();
        assert!(matches!(
            doc.validation_map(),
            Err(RecordsError::UnknownKey(ref k)) if k == "waves"
        ));
        // AC1-ERR: a moved list key held twice refuses the same way.
        let twice = "---\ndone_probes: [\"true\"]\n---\n\n## Gate records\n\n```yaml\ndone_probes:\n  - \"true\"\n```\n";
        let doc = super::super::doc::load_plan_text(twice).unwrap();
        assert!(matches!(
            doc.validation_map(),
            Err(RecordsError::BothPlaces(ref k)) if k == "done_probes"
        ));
        // AC1-HP: the six moved keys lift to the values frontmatter gave.
        let keys = "kill_criteria:\n  - name: iteration_ceiling\n    predicate: iteration > 15\n    reason: r\ndone_probes:\n  - \"true\"\n  - \"test -f x\"\ncode_index:\n  main_sha: ee5e0754ad\n  providers: []\nurls:\n  - https://github.com/o/r/pull/1\nsession_ids:\n  - s-1\nsources:\n  - a.md";
        let fm_form = load_plan_text_map(&format!("---\nstatus: ready\n{keys}\n---\n\n# T\n"));
        let body_form = load_plan_text_map(&format!(
            "---\nstatus: ready\n---\n\n# T\n\n## Gate records\n\n```yaml\n{keys}\n```\n"
        ));
        assert_eq!(fm_form, body_form);
        let lifted = Value::Object(body_form.clone());
        assert_eq!(
            lifted
                .pointer("/code_index/main_sha")
                .and_then(Value::as_str),
            Some("ee5e0754ad")
        );
        // set_blocks replaces one block in place and keeps its neighbours.
        let body = format!("# T\n\n## Gate records\n\n```yaml\n{keys}\n```\n");
        let urls = list_block("urls", &["u1".into(), "u2".into()]);
        let out = set_blocks(&body, &[("urls", urls)]).unwrap();
        assert!(
            out.contains("urls:\n  - u1\n  - u2\nsession_ids:\n  - s-1\nsources:\n  - a.md\n```\n"),
            "{out}"
        );
        assert!(
            out.contains("code_index:\n  main_sha: ee5e0754ad\n  providers: []\nurls:"),
            "{out}"
        );
        let fresh = set_blocks("# T\n\nbody\n", &[("urls", list_block("urls", &[]))]).unwrap();
        assert!(
            fresh.ends_with("body\n\n## Gate records\n\n```yaml\nurls: []\n```\n"),
            "{fresh}"
        );
        assert_eq!(
            body_list(&out, "urls").unwrap(),
            Some(vec!["u1".to_string(), "u2".to_string()])
        );
        assert_eq!(body_list(&out, "absent").unwrap(), None);
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
