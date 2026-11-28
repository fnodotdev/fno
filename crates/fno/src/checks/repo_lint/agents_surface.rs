//! The fno-agent binary's command surface for the command ratchet.
//!
//! Two independent readings of one surface, cross-checked:
//! - the dispatch: every command the client answers, read by
//!   `menu_caps::fno_agent_commands` (the one reading menu-caps counts with),
//!   plus the claim family's op arms in `claim_commands.rs`;
//! - the refusal: `ALL_CLIENT_ACTIONS`, the exact slice the
//!   unknown-command error prints, so the refusal can never name a command
//!   nothing dispatches.
//!
//! A dispatched command the refusal does not name is operator-invisible. The
//! ruling that owns this surface (2026-09-25: hidden top-level commands retire
//! into nested groups) is carried by `menu-caps-debt.tsv`'s fno-agent row;
//! until a command folds, its invisibility is deliberate and recorded in
//! `scripts/ci/agents-hidden-commands.tsv`, one row per command with a reason.
//!
//! Declared boundary: the scan reaches one level of dispatch tables, not the
//! deeper group CLIs (`backlog::cli`, the mail family's own parser, the
//! `do`/`pr` fronts' leaves, the closure produce commands). A group CLI's
//! leaves defend nothing yet; the fold that re-shapes a group regenerates
//! `command-baseline.txt` in the same PR, which is where its rows appear.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

use crate::menu_caps;

const CLAIM_COMMANDS_REL: &str = "crates/fno-agent/src/runtime/claim_commands.rs";
pub const HIDDEN_REL: &str = "scripts/ci/agents-hidden-commands.tsv";

/// Pinned sub-command seams: baseline rows the scanner asserts exist in the
/// client source by their distinctive spellings. A seam the source no longer
/// matches is a removed sub-surface; the ratchet refuses until the row goes
/// in the same PR that removed it. A NEW seam is invisible to this list -
/// the recorded boundary above, retired by the fold the debt row tracks.
const SEAMS: &[(&str, &str)] = &[
    ("claim birth", r#"== Some\("birth"\)"#),
    (
        "claude-birth-exec revive-proof",
        r#"== Some\("revive-proof"\)"#,
    ),
    ("pr-heal list", r#"== Some\("list"\)"#),
    (
        "do pr",
        r#"== Some\("do"\)\s*\n\s*&& args\.get\(1\)\.map\(String::as_str\) == Some\("pr"\)"#,
    ),
    (
        "do ci",
        r#"== Some\(&"do"\.to_string\(\)\) && args\.get\(1\) == Some\(&"ci"\.to_string\(\)\)"#,
    ),
    ("do show", r#"Some\("show" \| "diff"\)"#),
    ("do diff", r#"Some\("show" \| "diff"\)"#),
    ("test-run env", r#"Some\("env"\) => return"#),
    ("test-run sweep", r#"Some\("sweep"\) => return"#),
    ("test-run doctor", r#"Some\("doctor"\) => return"#),
];

/// One cross-checked reading of the binary's surface.
pub struct AgentsSurface {
    /// Every dispatched command, as `menu_caps::fno_agent_commands` reads it.
    pub top: BTreeSet<String>,
    /// The `ALL_CLIENT_ACTIONS` slice: what the unknown-command refusal
    /// prints.
    pub registered: BTreeSet<String>,
    /// The claim family's op arms, `birth` included.
    pub claim_ops: BTreeSet<String>,
    /// What the claim no-operation refusal names.
    pub claim_refusal_ops: Vec<String>,
    /// Seam rows present in the source (SEAMS entries that matched).
    pub seams: Vec<String>,
    /// SEAMS entries whose pattern the source no longer matches.
    pub missing_seams: Vec<String>,
}

fn read(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel))
        .map_err(|e| format!("agents-surface: cannot read {rel}: {e}"))
}

fn static_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

/// The quoted lowercase tokens of a source region.
fn literals(text: &str) -> BTreeSet<String> {
    let arm = literal_regex();
    arm.captures_iter(text).map(|c| c[1].to_string()).collect()
}

/// The lowercase-token literal regex, shared by the const readers.
fn literal_regex() -> Regex {
    static_regex(r#""([a-z][a-z0-9-]*)""#)
}

/// The bracket-balanced region a `const ... = &[...]` declaration holds.
/// `braced_region` cannot read it: the slice closes with `]`, and the first
/// `{` after the marker is the next function's body.
fn bracket_region<'a>(text: &'a str, marker: &str) -> Result<&'a str, String> {
    let start = text
        .find(marker)
        .ok_or_else(|| format!("agents-surface: no marker {marker:?}"))?;
    // Anchor on the value bracket: the type annotation `&[&str]` closes
    // first, so the first `[` after the marker is not the slice.
    let open = start
        + text[start..]
            .find("= &[")
            .ok_or_else(|| format!("agents-surface: no slice literal after {marker:?}"))?
        + 3;
    let mut depth = 0i64;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&text[open..=open + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!("agents-surface: no ] after {marker:?}"))
}

impl AgentsSurface {
    pub fn read(root: &Path) -> Result<Self, String> {
        // One reading of the top level, shared with menu-caps: the debt row
        // and these rows count the same words.
        let sources = menu_caps::AgentSources::read(root).map_err(|e| e.to_string())?;
        let top = menu_caps::fno_agent_commands(&sources)?;
        let client = &sources.client;
        // The seams read the binary's dispatch as one text: run's doors moved
        // to their own module and dispatch exactly as before.
        let dispatch = format!("{client}\n{}", sources.doors);
        let claim = read(root, CLAIM_COMMANDS_REL)?;

        // The refusal's registered spellings.
        let client_stripped = menu_caps::strip_comments(client);
        let all_region = bracket_region(&client_stripped, "const ALL_CLIENT_ACTIONS")?;
        let registered = literals(all_region);

        // The claim family: op arms plus what the no-operation refusal names.
        let claim_stripped = menu_caps::strip_comments(&claim);
        let claim_region =
            menu_caps::braced_region(&claim_stripped, CLAIM_COMMANDS_REL, "pub fn run_claim")?;
        let op_arm = static_regex(r#"if op == "([a-z][a-z0-9-]*)""#);
        let mut claim_ops: BTreeSet<String> = op_arm
            .captures_iter(claim_region)
            .map(|c| c[1].to_string())
            .collect();
        let refusal = static_regex(r#"claim requires an operation:\s*([a-z0-9-|]+)"#);
        let claim_refusal_ops: Vec<String> = refusal
            .captures(&claim)
            .map(|c| c[1].split('|').map(str::trim).map(str::to_string).collect())
            .unwrap_or_default();

        // The seams: each pinned pattern must still match the client source.
        let mut seams = Vec::new();
        let mut missing_seams = Vec::new();
        for (row, pattern) in SEAMS {
            if static_regex(pattern).is_match(&dispatch) {
                seams.push((*row).to_string());
            } else {
                missing_seams.push((*row).to_string());
            }
        }
        claim_ops.extend(
            seams
                .iter()
                .filter(|r| r.starts_with("claim "))
                .map(|r| r.trim_start_matches("claim ").to_string()),
        );

        Ok(Self {
            top,
            registered,
            claim_ops,
            claim_refusal_ops,
            seams,
            missing_seams,
        })
    }

    /// The commands the dispatch answers but the refusal does not name.
    pub fn hidden(&self) -> BTreeSet<String> {
        self.top.difference(&self.registered).cloned().collect()
    }

    /// The baseline rows the live surface earns: the binary leaf, one row per
    /// dispatched command, the claim family's ops, and the seams.
    pub fn rows(&self) -> Vec<String> {
        let mut rows: BTreeSet<String> = BTreeSet::new();
        rows.insert("fno-agent".to_string());
        for command in &self.top {
            rows.insert(format!("fno-agent {command}"));
        }
        for op in &self.claim_ops {
            rows.insert(format!("fno-agent claim {op}"));
        }
        for row in &self.seams {
            rows.insert(format!("fno-agent {row}"));
        }
        rows.into_iter().collect()
    }

    /// The cross-checks between the two readings and the hidden-command record.
    /// Each refusal names the row and the fix.
    pub fn drift(&self, hidden_rows: &BTreeMap<String, String>) -> Vec<String> {
        let mut failures = Vec::new();
        for command in &self.hidden() {
            if !hidden_rows.contains_key(command) {
                failures.push(format!(
                    "agents-surface: `{command}` dispatches but the refusal never names it, and \
                     {HIDDEN_REL} has no row. Record the deliberate invisibility as \
                     `{command}<TAB><why it has no operator surface>`, register the spelling, or \
                     remove the command in the same PR."
                ));
            }
        }
        for command in hidden_rows.keys() {
            let stale = if let Some(op) = command.strip_prefix("claim ") {
                !self.claim_ops.contains(op) || self.claim_refusal_ops.iter().any(|r| r == op)
            } else {
                !self.top.contains(command) || self.registered.contains(command)
            };
            if stale {
                failures.push(format!(
                    "agents-surface: {HIDDEN_REL} row `{command}` is stale - the command now \
                     dispatches registered, no longer dispatches, or its op is refusal-named. \
                     Delete the row in the same PR that changed the command."
                ));
            }
        }
        for command in &self.registered {
            if !self.top.contains(command) {
                failures.push(format!(
                    "agents-surface: the refusal names `{command}` but no dispatch arm answers it. \
                     A refused command must never read as registered: restore the arm, or \
                     remove the spelling from ALL_CLIENT_ACTIONS."
                ));
            }
        }
        for op in &self.claim_refusal_ops {
            if !self.claim_ops.contains(op) {
                failures.push(format!(
                    "agents-surface: the claim refusal names op `{op}` but no `if op ==` arm \
                     answers it. The refusal must not lie: restore the arm or drop the token."
                ));
            }
        }
        for row in &self.missing_seams {
            failures.push(format!(
                "agents-surface: seam row `fno-agent {row}` has no matching dispatch shape in \
                 the client source. The sub-command was removed or re-spelled: delete its baseline \
                 row in the same PR, or fix the seam pattern."
            ));
        }
        failures
    }
}

/// Parse `agents-hidden-commands.tsv`: `path<TAB>reason`, comments skipped.
pub fn hidden_rows(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut rows = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        match (cols.first(), cols.get(1)) {
            (Some(path), Some(reason)) if !path.trim().is_empty() && !reason.trim().is_empty() => {
                rows.insert(path.trim().to_string(), reason.trim().to_string());
            }
            _ => {
                return Err(format!(
                    "agents-surface: {HIDDEN_REL}:{} is not `path<TAB>reason`",
                    n + 1
                ))
            }
        }
    }
    Ok(rows)
}

/// The two-sided baseline check for the fno-agent rows: commands the binary
/// dispatches that no baseline row defends, and rows whose command is gone.
pub fn baseline_drift(live: &[String], baseline: &[String]) -> (Vec<String>, Vec<String>) {
    let live: BTreeSet<String> = live.iter().cloned().collect();
    let baseline_agents: BTreeSet<String> = baseline
        .iter()
        .filter(|r| r.starts_with("fno-agent"))
        .map(|r| {
            r.split_whitespace()
                .filter(|w| !w.starts_with('!'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let added: Vec<String> = live.difference(&baseline_agents).cloned().collect();
    let removed: Vec<String> = baseline_agents.difference(&live).cloned().collect();
    (added, removed)
}

/// The shrink-only record of `fno-agent <command>` mentions outside crates/:
/// `path<TAB>count`, one row per file that still names the client binary.
pub const CALLERS_REL: &str = "scripts/ci/fno-agent-callers.tsv";

/// Each tracked file outside crates/ that names a live client command as
/// `fno-agent <command>`, with the 1-based line of every mention.
fn outside_callers(
    root: &Path,
    top: &BTreeSet<String>,
) -> std::io::Result<BTreeMap<String, Vec<usize>>> {
    let listed = super::git_capture(root, &["ls-files", "-z"])
        .ok_or_else(|| std::io::Error::other("git ls-files exited non-zero"))?;
    let mention = static_regex(r"\bfno-agent[ \t]+([a-z][a-z0-9-]*)");
    let mut found = BTreeMap::new();
    for rel in listed.split('\0') {
        if rel.is_empty()
            || rel.starts_with("crates/")
            || [CALLERS_REL, HIDDEN_REL, "scripts/ci/command-baseline.txt"].contains(&rel)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let lines: Vec<usize> = text
            .lines()
            .enumerate()
            .flat_map(|(n, line)| {
                mention
                    .captures_iter(line)
                    .filter(|c| top.contains(&c[1]))
                    .map(move |_| n + 1)
                    .collect::<Vec<_>>()
            })
            .collect();
        if !lines.is_empty() {
            found.insert(rel.to_string(), lines);
        }
    }
    Ok(found)
}

/// The ratchet over the outside callers: a file may not pass its row, a file
/// with no row may name none, and a row above its file's count comes down in
/// the same change, so the count only falls.
pub fn caller_drift(
    found: &BTreeMap<String, Vec<usize>>,
    rows: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (path, lines) in found {
        let ceiling = rows.get(path).copied().unwrap_or(0);
        if lines.len() > ceiling {
            let at: Vec<String> = lines.iter().map(|n| format!("{path}:{n}")).collect();
            failures.push(format!(
                "agents-surface: {path} names the fno-agent client {} time(s), above its {CALLERS_REL} \
                 ceiling {ceiling}. Spell each as its fno command (`fno agent ...`): {}",
                lines.len(),
                at.join(", ")
            ));
        }
    }
    for (path, &ceiling) in rows {
        let count = found.get(path).map_or(0, Vec::len);
        if count < ceiling {
            failures.push(format!(
                "agents-surface: {path} names the fno-agent client {count} time(s), below its \
                 {CALLERS_REL} ceiling {ceiling}. Lower the row to {count} (delete it at 0) in \
                 this change."
            ));
        }
    }
    failures
}

/// The caller ratchet against the live tree and its committed record.
pub fn caller_failures(root: &Path, top: &BTreeSet<String>) -> Vec<String> {
    let rows = match std::fs::read_to_string(root.join(CALLERS_REL)) {
        Ok(text) => {
            let mut rows = BTreeMap::new();
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut cols = line.split('\t');
                match (
                    cols.next(),
                    cols.next().and_then(|c| c.trim().parse::<usize>().ok()),
                ) {
                    (Some(path), Some(count)) if !path.is_empty() => {
                        rows.insert(path.to_string(), count);
                    }
                    _ => {
                        return vec![format!(
                            "agents-surface: {CALLERS_REL}:{} is not `path<TAB>count`",
                            n + 1
                        )]
                    }
                }
            }
            rows
        }
        Err(e) => return vec![format!("agents-surface: cannot read {CALLERS_REL}: {e}")],
    };
    match outside_callers(root, top) {
        Ok(found) => caller_drift(&found, &rows),
        Err(e) => vec![format!(
            "agents-surface: git ls-files failed under {}: {e}",
            root.display()
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_surface_cross_checks_refuse_each_drift() {
        let surface = AgentsSurface {
            top: BTreeSet::from(["spawn".to_string(), "claim".to_string()]),
            registered: BTreeSet::from(["spawn".to_string(), "gone".to_string()]),
            claim_ops: BTreeSet::from(["acquire".to_string(), "sweep".to_string()]),
            claim_refusal_ops: vec!["acquire".to_string(), "dead-op".to_string()],
            seams: vec!["claim birth".to_string()],
            missing_seams: vec!["test-run env".to_string()],
        };
        // A hidden command with no recorded row refuses, naming the fix.
        let rows = hidden_rows("claim sweep\thidden transport\n").unwrap();
        let failures = surface.drift(&rows);
        assert!(
            failures.iter().any(|f| f.contains("`claim` dispatches")),
            "{failures:?}"
        );
        // A registered spelling nothing dispatches refuses.
        assert!(
            failures.iter().any(|f| f.contains("`gone`")),
            "{failures:?}"
        );
        // A refusal op with no arm refuses.
        assert!(
            failures.iter().any(|f| f.contains("`dead-op`")),
            "{failures:?}"
        );
        // A seam pattern the source lost refuses.
        assert!(
            failures.iter().any(|f| f.contains("test-run env")),
            "{failures:?}"
        );
        // A stale hidden row refuses: the op became refusal-named. The map
        // also records the fixture's one hidden command (`claim`), so the
        // stale row is the only NEW failure relative to the always-on set
        // (dead registered spelling, dead refusal op, lost seam).
        let stale =
            hidden_rows("claim\tcore command surface\nclaim acquire\tnow listed\n").unwrap();
        let failures = surface.drift(&stale);
        assert!(
            failures
                .iter()
                .any(|f| f.contains("stale") && f.contains("claim acquire")),
            "{failures:?}"
        );
        assert_eq!(failures.len(), 4, "{failures:?}");

        // The outside-caller ratchet: a new `fno-agent <command>` line in a
        // file with no row refuses by file and line, and a row above its
        // file's count must come down.
        let found = BTreeMap::from([("skills/x/SKILL.md".to_string(), vec![3])]);
        let callers = caller_drift(&found, &BTreeMap::new());
        assert!(
            matches!(callers.as_slice(), [only] if only.contains("skills/x/SKILL.md:3")),
            "{callers:?}"
        );
        let slack = BTreeMap::from([("skills/x/SKILL.md".to_string(), 2)]);
        let lower = caller_drift(&found, &slack);
        assert!(
            lower.iter().any(|f| f.contains("Lower the row to 1")),
            "{lower:?}"
        );
        assert!(caller_drift(
            &found,
            &BTreeMap::from([("skills/x/SKILL.md".to_string(), 1)])
        )
        .is_empty());
    }
    #[test]
    fn agents_surface_holds_on_the_committed_tree() {
        // The CI gate: the real sources, the real hidden record, the real
        // baseline. Pins the surface to its rows - the node's specimen,
        // `fno-agent claim sweep`, is defended here by name.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let surface = AgentsSurface::read(&root).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            surface.rows().iter().any(|r| r == "fno-agent claim sweep"),
            "the claim sweep specimen row is missing: {:?}",
            surface.claim_ops
        );
        // Every word the live client answers has an fno spelling.
        if let Err(homeless) = menu_caps::check_homes(&surface.top, &menu_caps::menu_forwarded()) {
            panic!("homeless fno-agent words:\n  {}", homeless.join("\n  "));
        }
        let callers = caller_failures(&root, &surface.top);
        assert!(callers.is_empty(), "{}", callers.join("\n"));
        let hidden_text = std::fs::read_to_string(root.join(HIDDEN_REL))
            .unwrap_or_else(|e| panic!("agents-surface: cannot read {HIDDEN_REL}: {e}"));
        let rows = hidden_rows(&hidden_text).unwrap();
        let failures = surface.drift(&rows);
        assert!(
            failures.is_empty(),
            "surface drift:\n  {}",
            failures.join("\n  ")
        );
        let baseline =
            std::fs::read_to_string(root.join("scripts/ci/command-baseline.txt")).unwrap();
        let baseline_rows: Vec<String> = baseline
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        let (added, removed) = baseline_drift(&surface.rows(), &baseline_rows);
        assert!(
            added.is_empty(),
            "commands missing baseline rows: {added:?}"
        );
        assert!(
            removed.is_empty(),
            "baseline rows with no command: {removed:?}"
        );
        let _ = surface;
    }
}
