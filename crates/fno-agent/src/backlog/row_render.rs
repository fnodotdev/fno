//! The pr-line renderers a node row feeds: the primary from `pr_number`/
//! `pr_url` with the row's `merge_status`, one line per `additional_prs`
//! entry, and the four-key json reduction readers consume. The provenance
//! text and json arms share them.

use serde_json::{json, Value};

use super::workflows::text_at;

/// The repo slug a recorded url names, verbatim from what was recorded.
fn pr_repo_slug(url: Option<&str>) -> Option<String> {
    url.and_then(|u| crate::backlog::pr_link::repo_slug_from_url(Some(u)))
}

/// The parts a pr line renders beyond the link: the repo slug, the merge
/// status and the note. A line with nothing to say keeps the bare shape.
fn pr_suffix_parts(slug: Option<&str>, status: Option<&str>, note: Option<&str>) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(slug) = slug {
        parts.push(slug.to_string());
    }
    if let Some(status) = status {
        parts.push(status.to_string());
    }
    if let Some(note) = note {
        parts.push(format!("note: {note}"));
    }
    parts
}

/// The pr lines a row renders: the primary from `pr_number`/`pr_url` with
/// the row's `merge_status`, then one line per `additional_prs` entry.
/// Bare-int and bare-url entry shapes render like `pr_refs_of` reads them.
pub(crate) fn render_pr_lines(row: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let number = crate::backlog::row_facts::int_field(row, "pr_number");
    let url = text_at(row, "pr_url").filter(|u| !u.is_empty());
    let status = text_at(row, "merge_status").filter(|s| !s.is_empty());
    let slug = pr_repo_slug(url.as_deref());
    lines.push(format!(
        "  pr: {}{}",
        render_pr_ref(number, url.as_deref()),
        suffix(&pr_suffix_parts(slug.as_deref(), status.as_deref(), None))
    ));
    let entries = crate::backlog::row_facts::list_field(row, "additional_prs");
    for entry in entries {
        let (number, url, status, note) = match entry {
            Value::Object(_) => (
                crate::backlog::row_facts::int_field(entry, "number"),
                crate::backlog::row_facts::str_field(entry, "url").filter(|u| !u.is_empty()),
                crate::backlog::row_facts::str_field(entry, "merge_status")
                    .filter(|s| !s.is_empty()),
                crate::backlog::row_facts::str_field(entry, "note").filter(|n| !n.is_empty()),
            ),
            Value::Number(n) => (n.as_i64(), None, None, None),
            Value::String(url) => (None, Some(url.as_str()), None, None),
            _ => (None, None, None, None),
        };
        let slug = pr_repo_slug(url);
        lines.push(format!(
            "  pr: {}{}",
            render_pr_ref(number, url),
            suffix(&pr_suffix_parts(slug.as_deref(), status, note))
        ));
    }
    lines
}

/// The `  pr: ` reference half of a pr line: `#<n> <url>`, `#<n>`, `<url>`,
/// or `(none)`.
fn render_pr_ref(number: Option<i64>, url: Option<&str>) -> String {
    match (number, url) {
        (None, None) => "(none)".to_string(),
        (None, Some(url)) => url.to_string(),
        (Some(n), None) => format!("#{n}"),
        (Some(n), Some(url)) => format!("#{n} {url}"),
    }
}

/// Wrap parts in ` (...)`, empty when there are no parts.
fn suffix(parts: &[String]) -> String {
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// The json `additional_prs` array: every entry reduced to the four keys a
/// reader consumes, `[]` when the row carries none.
pub(crate) fn pr_entries_json(row: &Value) -> Value {
    let list = crate::backlog::row_facts::list_field(row, "additional_prs");
    Value::Array(
        list.iter()
            .map(|entry| {
                json!({
                    "number": crate::backlog::row_facts::int_field(entry, "number"),
                    "url": crate::backlog::row_facts::str_field(entry, "url"),
                    "merge_status": crate::backlog::row_facts::str_field(entry, "merge_status"),
                    "note": crate::backlog::row_facts::str_field(entry, "note"),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_renders_primary_and_every_additional_pr() {
        // AC1-HP: the primary line carries the repo slug and merge status,
        // and every additional_prs entry renders its own line with status
        // and note.
        let row = json!({
            "pr_number": 289,
            "pr_url": "https://github.com/fnodotdev/fno/pull/289",
            "merge_status": "merged",
            "additional_prs": [
                {"number": 351, "url": "https://github.com/fnodotdev/fno/pull/351", "merge_status": "merged", "note": "folded"},
                {"number": 244, "url": "https://github.com/footnote-sh/fno-web/pull/244", "merge_status": "merged", "note": "docs"},
                {"number": 7}
            ]
        });
        assert_eq!(
            render_pr_lines(&row),
            vec![
                "  pr: #289 https://github.com/fnodotdev/fno/pull/289 (fnodotdev/fno, merged)",
                "  pr: #351 https://github.com/fnodotdev/fno/pull/351 (fnodotdev/fno, merged, note: folded)",
                "  pr: #244 https://github.com/footnote-sh/fno-web/pull/244 (footnote-sh/fno-web, merged, note: docs)",
                "  pr: #7",
            ]
        );
    }
}
