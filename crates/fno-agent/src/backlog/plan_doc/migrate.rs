//! Synonym frontmatter-key collapse, byte-preserving, the port of
//! `cli/src/fno/plan/migrate_keys.py`. Line-level rename over a plans dir:
//! one canonical key per axis, no YAML round-trip, body byte-intact.

use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

use super::codec::atomic_write;

/// Legacy synonym -> canonical key. `deliverable_type` is intentionally absent
/// (a different axis with live readers, not a synonym of `type`).
pub const RENAME: &[(&str, &str)] = &[
    ("graph_node_id", "node"),
    ("created_at", "created"),
    ("depends_on", "blocked_by"),
    ("kind", "type"),
];

fn front_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    // Non-greedy so the FIRST frontmatter block wins even if the body carries
    // a `---` rule.
    CELL.get_or_init(|| Regex::new(r"\A(---\n)(?s)(.*?)\n---(.*)\z").unwrap())
}

fn key_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^([ \t]*)([A-Za-z0-9_]+)([ \t]*:)(.*)$").unwrap())
}

fn rename_target(key: &str) -> Option<&'static str> {
    RENAME.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

/// Strip surrounding whitespace, then one pair of matching quotes.
fn scalar(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.len() >= 2 {
        let b = raw.as_bytes();
        if b[0] == b[raw.len() - 1] && (b[0] == b'\'' || b[0] == b'"') {
            return &raw[1..raw.len() - 1];
        }
    }
    raw
}

/// Top-level keys of a frontmatter block (indented lines are nested, not keys).
fn top_level_keys(fm: &str) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for line in fm.lines() {
        if let Some(c) = key_re().captures(line) {
            if c[1].is_empty() {
                keys.insert(c[2].to_string());
            }
        }
    }
    keys
}

/// Return (new_text, notes). new_text is None when nothing changed.
///
/// notes describe every rename / drop / kept-for-review decision, so the CLI
/// receipt is a faithful account of what the migration did to each file.
pub fn migrate_text(text: &str) -> (Option<String>, Vec<String>) {
    let m = match front_re().captures(text) {
        Some(m) => m,
        None => return (None, Vec::new()),
    };
    let fm = &m[2];
    let mut present = top_level_keys(fm);

    // Node identity resolves from `node` OR its legacy `graph_node_id` (they
    // collapse to the same key), so a file carrying graph_node_id + claims
    // drops the duplicate claims in the SAME pass as the rename: single-pass
    // idempotent.
    let mut node_val: Option<String> = None;
    for wanted in ["node", "graph_node_id"] {
        for line in fm.lines() {
            if let Some(c) = key_re().captures(line) {
                if c[1].is_empty() && &c[2] == wanted {
                    node_val = Some(scalar(&c[4]).to_string());
                    break;
                }
            }
        }
        if node_val.is_some() {
            break;
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut changed = false;
    for line in fm.lines() {
        let km = match key_re().captures(line) {
            Some(c) if c[1].is_empty() => c,
            // Indented or non-key lines pass through untouched.
            _ => {
                out.push(line.to_string());
                continue;
            }
        };
        let key = &km[2];
        let target = rename_target(key);
        if let Some(target) = target {
            if present.contains(target) {
                notes.push(format!("kept {key} ({target} also present) - review"));
                out.push(line.to_string());
            } else {
                out.push(format!("{}{}{}{}", &km[1], target, &km[3], &km[4]));
                present.remove(key);
                present.insert(target.to_string());
                changed = true;
                notes.push(format!("{key} -> {target}"));
            }
            continue;
        }
        if key == "claims" {
            let cval = scalar(&km[4]);
            if let Some(node) = &node_val {
                if cval == node {
                    changed = true;
                    notes.push("dropped claims (== node)".to_string());
                    continue; // drop the duplicate line
                }
                notes.push("kept claims (differs from node) - review".to_string());
            }
            out.push(line.to_string());
            continue;
        }
        out.push(line.to_string());
    }

    if !changed {
        return (None, notes);
    }
    let text = format!("---\n{}\n---{}", out.join("\n"), &m[3]);
    (Some(text), notes)
}

/// The scan result the sweep returns.
#[derive(Debug, Default)]
pub struct MigrateResult {
    pub migrated: usize,
    pub skipped: usize,
    /// Files with a kept legacy key needing a human decision.
    pub review: usize,
    /// (path, notes) per migrated file.
    pub changes: Vec<(String, Vec<String>)>,
    /// (path, review list) per file a human must reconcile.
    pub review_files: Vec<(String, Vec<String>)>,
    pub warnings: Vec<String>,
    /// The `--gate-records` walk: cuts lift instead of RENAME, recursive.
    pub gate_records_mode: bool,
}

impl MigrateResult {
    pub fn summary(&self) -> String {
        format!(
            "{} migrated, {} need review, {} skipped",
            self.migrated, self.review, self.skipped
        )
    }
}

/// Scan every `*.md` in *plans_dir*, collapse synonym keys byte-preservingly.
pub fn migrate(plans_dir: &Path, apply: bool) -> MigrateResult {
    let mut res = MigrateResult::default();
    let entries = match std::fs::read_dir(plans_dir) {
        Ok(e) => e,
        Err(_) => {
            res.warnings
                .push(format!("plans dir not found: {}", plans_dir.display()));
            return res;
        }
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
        .collect();
    paths.sort();
    migrate_paths(&mut res, paths, apply);
    res
}

/// Apply one text migration over a sorted path list: the shared walk of both
/// migration modes.
fn migrate_paths(res: &mut MigrateResult, paths: Vec<std::path::PathBuf>, apply: bool) {
    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                res.skipped += 1;
                res.warnings
                    .push(format!("skip (unreadable): {}: {e}", path.display()));
                continue;
            }
        };
        let (new_text, notes) = if res.gate_records_mode {
            gate_records_text(&text)
        } else {
            migrate_text(&text)
        };
        let review_notes: Vec<String> = notes
            .iter()
            .filter(|n| n.contains("review"))
            .cloned()
            .collect();
        let had_review = !review_notes.is_empty();
        if had_review {
            // Record the path so the receipt can name which files a human must
            // reconcile - a bare count is useless without the list.
            res.review += 1;
            res.review_files
                .push((path.display().to_string(), review_notes));
        }
        let new_text = match new_text {
            Some(t) => t,
            None => {
                res.skipped += 1;
                continue;
            }
        };
        res.migrated += 1;
        res.changes.push((path.display().to_string(), notes));
        if apply {
            // Atomic (tmp + rename): an interrupted migration never leaves a
            // plan half-written, so a re-run stays idempotent and recoverable.
            let _ = atomic_write(&path, &new_text);
        }
    }
}

/// The `--gate-records` migration: lift the record keys (`records::GATE_KEYS`)
/// out of frontmatter into the `## Gate records` fence, appending the section
/// when the body has none. Line-level and byte-preserving,
/// like `migrate_text`; the RENAME table never runs here.
pub fn gate_records(plans_dir: &Path, apply: bool) -> MigrateResult {
    let mut res = MigrateResult::default();
    res.gate_records_mode = true;
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    collect_md(plans_dir, &mut paths);
    paths.sort();
    if paths.is_empty() && !plans_dir.is_dir() {
        res.warnings
            .push(format!("plans dir not found: {}", plans_dir.display()));
    }
    migrate_paths(&mut res, paths, apply);
    res
}

/// Recursive `*.md` collect: block-bearing plans sit in subfolders too.
fn collect_md(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_md(&p, out);
        } else if p.extension().map(|x| x == "md").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// The keys this mode lifts. Top-level only: nested keys stay.
const GATE_BLOCK_KEYS: &[&str] = super::records::GATE_KEYS;

/// Cut `key:` plus every following indented, blank or comment line, up to the
/// next top-level key. Returns the cut lines and the fm-index range end.
fn cut_block(fm_lines: &[&str], start: usize) -> (Vec<String>, usize) {
    let mut cut: Vec<String> = Vec::new();
    let mut i = start;
    while let Some(line) = fm_lines.get(i) {
        if i > start && migrate_key(line).is_some() {
            break;
        }
        cut.push((*line).to_string());
        i += 1;
    }
    (cut, i)
}

/// One YAML-safe plain or quoted scalar.
fn yaml_scalar(text: &str) -> String {
    serde_yaml_ng::to_string(text)
        .map(|t| t.trim_end().to_string())
        .unwrap_or_else(|_| format!("{text:?}"))
}

/// The top-level key of a frontmatter line, via migrate.rs's own key regex.
fn migrate_key(line: &str) -> Option<String> {
    key_re()
        .captures(line)
        .filter(|c| c[1].is_empty())
        .map(|c| c[2].to_string())
}

/// One file's gate-records lift: (new_text, notes). new_text is None when
/// nothing changed; a review note names why the file stays untouched.
pub fn gate_records_text(text: &str) -> (Option<String>, Vec<String>) {
    let mut notes: Vec<String> = Vec::new();
    let m = match front_re().captures(text) {
        Some(m) => m,
        None => return (None, notes),
    };
    let fm = &m[2];
    let fm_lines: Vec<&str> = fm.lines().collect();

    // A body that already carries the section takes the cut inside its fence;
    // a key the fence already holds would make two copies, so a human decides.
    let body = &m[3];
    let existing = match super::records::body_records(body) {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("kept ({e}) - review"));
            return (None, notes);
        }
    };

    // Find the cut ranges: each gate key's block, in line order.
    let mut keep: Vec<String> = Vec::new();
    let mut cut: Vec<String> = Vec::new();
    let mut per_key: Vec<(&str, String)> = Vec::new();
    let mut blocks = 0usize;
    let mut i = 0usize;
    while let Some(line) = fm_lines.get(i) {
        match migrate_key(line) {
            Some(k) if GATE_BLOCK_KEYS.contains(&k.as_str()) => {
                if existing.as_ref().is_some_and(|r| r.contains_key(&k)) {
                    notes.push(format!(
                        "kept ({k} is in both frontmatter and ## Gate records) - review"
                    ));
                    return (None, notes);
                }
                let (mut part, next) = cut_block(&fm_lines, i);
                // A fence inside frontmatter would break the record fence.
                if part.iter().any(|l| l.trim_start().starts_with("```")) {
                    notes.push("kept (a gate block carries a ``` fence line) - review".to_string());
                    return (None, notes);
                }
                blocks += 1;
                let key = GATE_BLOCK_KEYS
                    .iter()
                    .find(|g| **g == k)
                    .copied()
                    .unwrap_or_default();
                per_key.push((key, part.join("\n")));
                cut.append(&mut part);
                i = next;
            }
            _ => {
                keep.push((*line).to_string());
                i += 1;
            }
        }
    }
    if blocks == 0 {
        return (None, notes);
    }

    // Parse back: the cut text must yield the same YAML values the
    // frontmatter carried, or the file needs a human.
    let fm_value: serde_yaml_ng::Value = match serde_yaml_ng::from_str(fm) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("kept (frontmatter does not parse: {e}) - review"));
            return (None, notes);
        }
    };
    let cut_text = cut.join("\n");
    let parsed: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&cut_text) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("kept (the cut does not parse: {e}) - review"));
            return (None, notes);
        }
    };
    let parsed_map = match parsed.as_mapping() {
        Some(m) => m,
        None => {
            notes.push("kept (the cut is not a YAML mapping) - review".to_string());
            return (None, notes);
        }
    };
    for (k, v) in parsed_map {
        let key = k.as_str().unwrap_or_default();
        let original = fm_value
            .get(key)
            .cloned()
            .unwrap_or(serde_yaml_ng::Value::Null);
        if super::doc::yaml_to_json(v) != super::doc::yaml_to_json(&original) {
            notes.push(format!("kept ({key} cut does not round-trip) - review"));
            return (None, notes);
        }
    }

    let new_body = if existing.is_some() {
        match super::records::set_blocks(body, &per_key) {
            Ok(b) => b,
            Err(e) => {
                notes.push(format!("kept ({e}) - review"));
                return (None, notes);
            }
        }
    } else {
        format!(
            "{}\n## Gate records\n\n```yaml\n{}\n```\n",
            body.trim_end_matches('\n'),
            cut_text
        )
    };
    // The Obsidian PR column keeps one scalar link: the newest url.
    if !top_level_keys(fm).contains("pr_url") {
        let urls = super::records::body_list(&new_body, "urls").ok().flatten();
        if let Some(last) = urls.as_ref().and_then(|u| u.last()) {
            keep.push(format!("pr_url: {}", yaml_scalar(last)));
        }
    }
    let new_text = format!("---\n{}\n---{}", keep.join("\n"), new_body);
    // The migrated file must read back clean: no both-places, no stranger.
    match super::doc::load_plan_text(&new_text).map(|d| d.validation_map().map(|_| ())) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            notes.push(format!(
                "kept (the migrated file does not lift: {e}) - review"
            ));
            return (None, notes);
        }
        Err(e) => {
            notes.push(format!(
                "kept (the migrated file does not parse: {e}) - review"
            ));
            return (None, notes);
        }
    }
    for key in GATE_BLOCK_KEYS {
        if cut.iter().any(|l| migrate_key(l).as_deref() == Some(*key)) {
            notes.push(format!("{key} -> ## Gate records"));
        }
    }
    (Some(new_text), notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(fm_lines: &str) -> String {
        format!("---\n{fm_lines}\n---\n\n# Title\n\nbody text\n")
    }

    #[test]
    fn renames_all_legacy_keys_body_intact() {
        let text =
            &plan("graph_node_id: x-1\ncreated_at: 2026-07-08\ndepends_on: [x-2]\nkind: feature");
        let (out, notes) = migrate_text(text);
        let out = out.unwrap();
        assert!(out.contains("\nnode: x-1\n"), "{out}");
        assert!(out.contains("\ncreated: 2026-07-08\n"), "{out}");
        assert!(out.contains("\nblocked_by: [x-2]\n"), "{out}");
        assert!(out.contains("\ntype: feature\n"), "{out}");
        assert!(!out.contains("graph_node_id") && !out.contains("created_at"));
        assert!(out.ends_with("\n\n# Title\n\nbody text\n")); // body byte-intact
        assert_eq!(notes.len(), 4);
    }

    #[test]
    fn no_rename_when_canonical_target_present() {
        // kind + type both present -> keep kind (never create a duplicate type key).
        let text = &plan("type: feature\nkind: task\nnode: x-1");
        let (out, notes) = migrate_text(text);
        assert!(out.is_none()); // nothing safely renamable
        assert!(
            notes.iter().any(|n| n.contains("kind (type also present)")),
            "{notes:?}"
        );
    }

    /// AC5-HP + AC5-ERR: the gate-records lift is byte-preserving and reads
    /// back to the same values; a body already carrying the section refuses
    /// with a review note and an untouched file.
    #[test]
    fn gate_records_lift_round_trips_and_refuses_an_existing_section() {
        let text = "---\nstatus: ready\nnode: x-1\nconsolidation:\n  outcome: proceed_alone\n  rejected:\n    - id: x-2\n      reason: r\nsurface:\n  question: \"why?\"\n  sweep: s\n  answerers: []\n  count: 1\n  count_after: 0\n---\n\n# T\n\nbody\n";
        let (out, notes) = gate_records_text(text);
        let out = out.unwrap();
        let fm = &out[..out.find("\n---\n").unwrap()];
        assert!(!fm.contains("consolidation"), "{fm}");
        assert!(!fm.contains("surface:"), "{fm}");
        assert!(out.contains("status: ready\nnode: x-1"), "{fm}");
        let fence = "```yaml\nconsolidation:\n  outcome: proceed_alone\n  rejected:\n    - id: x-2\n      reason: r\nsurface:\n  question: \"why?\"\n  sweep: s\n  answerers: []\n  count: 1\n  count_after: 0\n```";
        assert!(out.ends_with(&format!("{fence}\n")), "{out}");
        assert!(notes.contains(&"consolidation -> ## Gate records".to_string()));
        assert!(notes.contains(&"surface -> ## Gate records".to_string()));
        // Validation parity: the lifted map reads the same values.
        let doc = crate::plan_doc::doc::load_plan_text(&out).unwrap();
        let v = serde_json::Value::Object(doc.validation_map().unwrap());
        assert_eq!(
            v.pointer("/consolidation/outcome")
                .and_then(serde_json::Value::as_str),
            Some("proceed_alone")
        );
        assert_eq!(
            v.pointer("/surface/question")
                .and_then(serde_json::Value::as_str),
            Some("why?")
        );
        // Re-running on a migrated file lifts nothing.
        let (again, _) = gate_records_text(&out);
        assert!(again.is_none());
        // AC6-ERR: a key in both frontmatter and the fence stays untouched.
        let guarded = "---\nstatus: ready\nconsolidation:\n  outcome: absorb\n---\n\n## Gate records\n\n```yaml\nconsolidation:\n  outcome: absorb\n```\n";
        let (out2, notes2) = gate_records_text(guarded);
        assert!(out2.is_none());
        assert!(notes2.iter().any(|n| n.contains("review")), "{notes2:?}");
        // AC6-HP: the six moved keys splice into an existing fence byte for
        // byte, the newest url stays as a scalar pr_url, and a re-run is a no-op.
        let moved = "kill_criteria:\n  - name: iteration_ceiling\n    predicate: iteration > 15\n    reason: r\ncode_index:\n  main_sha: ee5e0754ad\n  providers: []\ndone_probes:\n  - \"true\"\nurls:\n- https://github.com/o/r/pull/1\n- https://github.com/o/r/pull/2\nsession_ids: [s-1]\nsources:\n  - a.md";
        let text = format!("---\nstatus: ready\n{moved}\npriority: p2\n---\n\n# T\n\n## Gate records\n\n```yaml\nconsolidation:\n  outcome: absorb\n```\n\n## After\n\ntail\n");
        let before = crate::plan_doc::doc::load_plan_text(&text).unwrap();
        let (out3, notes3) = gate_records_text(&text);
        let out3 = out3.unwrap_or_else(|| panic!("{notes3:?}"));
        assert!(
            out3.starts_with(
                "---\nstatus: ready\npriority: p2\npr_url: https://github.com/o/r/pull/2\n---\n"
            ),
            "{out3}"
        );
        assert!(
            out3.contains(&format!(
                "```yaml\nconsolidation:\n  outcome: absorb\n{moved}\n```\n\n## After\n\ntail\n"
            )),
            "{out3}"
        );
        let after = crate::plan_doc::doc::load_plan_text(&out3).unwrap();
        let lifted = after.validation_map().unwrap();
        for key in [
            "kill_criteria",
            "code_index",
            "done_probes",
            "urls",
            "session_ids",
            "sources",
        ] {
            assert_eq!(lifted.get(key), before.frontmatter.get(key), "{key}");
        }
        assert!(gate_records_text(&out3).0.is_none());
    }
}
