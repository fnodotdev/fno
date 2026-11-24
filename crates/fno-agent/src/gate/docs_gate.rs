//! The docs gate: a PR that changes what a user sees or types names its doc.
//! Until 2026-10-30 the rule was prose only, so user-facing PRs merged with
//! no doc. `fno do pr create` prints a notice and creates the PR. The merge
//! door warns when the changed files touch a user surface and
//! the PR body carries no `Docs:` line naming the doc PR or file, or the
//! `Docs: none, <reason>` escape. The text names the two skills that
//! write and check the doc (fno-doc-writing, asd-ste100).
//!
//! A user surface is `merge.visual_paint_paths` (read live, so a project's
//! render surface counts) plus the built-in list below. The built-in list
//! names footnote's own layout, so it arms only where the mux client tree
//! exists; any other project stays disarmed and spends no changed-file read.

use crate::authorized_merge::{Blocker, BlockerClass, PrFacts};
use crate::merge_gates::GateReads;
use std::path::Path;

const BUILTIN_DOCS_SURFACE_PATHS: &[&str] = &[
    // CLI help text: the clap command tables and the legacy Python CLI tree.
    "crates/fno/src/cli_args.rs",
    "crates/fno-agent/src/cli_args.rs",
    "cli/src/fno/**",
    // Config keys: the resolved schema and its doctor.
    "crates/fno-agent/src/spawn/agents_config.rs",
    "crates/fno/src/config/config_doctor/**",
    "crates/fno/config_schema.json",
    "crates/fno/src/config/config_cli/model/fields.json",
    // Setup and the wizard a new user walks through.
    "crates/fno/src/setup/**",
    // Account switching and login a user types.
    "crates/fno-agent/src/spawn/accounts/**",
    "crates/fno-agent/src/runtime/claude_vault.rs",
    // Skill, agent and command descriptions users read.
    "skills/**",
    "agents/**",
    "commands/**",
    // Hooks a user sees fire.
    "hooks/**",
];
const BUILTIN_DOCS_SURFACE_ANCHOR: &str = "crates/fno/src/ui/client";

/// The paths whose changed files demand a `Docs:` line: the configured
/// paint surface plus the built-in user-surface list where this checkout
/// carries the mux client tree.
pub fn user_surface_paths(cwd: &Path) -> Vec<String> {
    let mut paths = crate::agents_config::visual_paint_paths(cwd);
    let root = crate::paths::canonical_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    if root.join(BUILTIN_DOCS_SURFACE_ANCHOR).is_dir() {
        paths.extend(BUILTIN_DOCS_SURFACE_PATHS.iter().map(|p| (*p).to_string()));
    }
    paths
}

/// The changed files that touch a user surface.
pub fn surface_files(files: &[String], cwd: &Path) -> Vec<String> {
    let patterns = user_surface_paths(cwd);
    files
        .iter()
        .filter(|f| crate::merge_gates::path_matches_paint(f, &patterns))
        .cloned()
        .collect()
}

/// The body's `Docs:` value, when some line carries one with content.
fn docs_value(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| {
            let t = line.trim();
            t.strip_prefix("Docs:")
                .or_else(|| t.strip_prefix("docs:"))
                .map(str::trim)
                .filter(|v| !v.is_empty())
        })
        .map(String::from)
}

/// Strip the punctuation and the backticks around a token: ``(`next.mdx`),``.
fn trim_token(t: &str) -> &str {
    t.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '(' | ')' | '[' | ']' | ',' | ';' | ':' | '.' | '"' | '\''
        )
    })
}

/// True when a value points at a doc: a `#<n>` PR ref, `PR <n>`, a URL, or a
/// path or markdown file name. Trailing punctuation never hides the doc. A
/// bare word such as `owed` or `todo` points at none.
fn value_points_at_doc(value: &str) -> bool {
    let tokens: Vec<&str> = value.split_whitespace().map(trim_token).collect();
    tokens.iter().enumerate().any(|(i, t)| {
        t.contains("://")
            || t.ends_with(".md")
            || t.ends_with(".mdx")
            || t.split('#')
                .skip(1)
                .any(|n| n.chars().next().is_some_and(|c| c.is_ascii_digit()))
            || (t.eq_ignore_ascii_case("pr")
                && tokens
                    .get(i + 1)
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
    })
}

/// True when the body names its doc: a doc PR or file, or the
/// `Docs: none, <reason>` escape with a reason. A bare `Docs: none` lacks
/// the reason, and a value naming no doc (`Docs: owed`) does not pass.
pub fn body_names_doc(body: &str) -> bool {
    let Some(value) = docs_value(body) else {
        return false;
    };
    if value == "none" {
        return false;
    }
    match value.strip_prefix("none,") {
        Some(reason) => !reason.trim().is_empty(),
        None => value_points_at_doc(&value),
    }
}

/// The PR a `Docs:` line names, with its number and the repo slug
/// canonicalized against the rename aliases (None for a same-repo PR or
/// `PR <n>`). None when the line escapes with a reason, names a
/// `.md`/`.mdx` path, or names no PR. Without a readable alias store the
/// verbatim slug stands.
pub fn docs_pr_ref(body: &str, cwd: Option<&Path>) -> Option<(Option<String>, i64)> {
    let value = docs_value(body)?;
    if value == "none" || value.starts_with("none,") {
        return None;
    }
    let mut prev_pr = false;
    for token in value.split_whitespace() {
        let t = trim_token(token);
        if t.eq_ignore_ascii_case("pr") {
            prev_pr = true;
            continue;
        }
        if let Some((prefix, n)) = t.rsplit_once('#') {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && n.parse::<i64>().is_ok() {
                let slug = canonical_slug((!prefix.is_empty()).then(|| prefix.to_string()), cwd);
                return Some((slug, n.parse().unwrap_or_default()));
            }
        }
        if prev_pr && !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            return Some((None, t.parse().unwrap_or_default()));
        }
        if t.contains("://") {
            let slug = crate::backlog::pr_link::repo_slug_from_url(Some(t));
            let number = crate::backlog::pr_link::pr_number_from_url(Some(t));
            if let (Some(slug), Some(number)) = (slug, number) {
                return Some((canonical_slug(Some(slug), cwd), number));
            }
        }
    }
    None
}

/// The slug under the rename aliases, read from the caller's config and
/// cache rows; a miss keeps the verbatim slug.
fn canonical_slug(slug: Option<String>, cwd: Option<&Path>) -> Option<String> {
    let s = slug?;
    let aliases = crate::backlog::pr_link::aliases_with_probe(cwd, None, std::iter::empty());
    Some(crate::backlog::pr_link::canonical_repo_slug(&s, &aliases))
}

/// The missing-doc text: the touched files, the two accepted body forms,
/// and the two skills that write and check the doc. Create prints it as a
/// notice and proceeds; the merge doors print it as a warning.
pub fn missing_doc_text(files: &[String]) -> String {
    let mut out = format!(
        "{} changed file(s) are a user surface and the PR body names no doc",
        files.len()
    );
    for f in files.iter().take(5) {
        out.push_str(&format!("\n  {f}"));
    }
    if files.len() > 5 {
        out.push_str(&format!("\n  (and {} more)", files.len() - 5));
    }
    out.push_str("\n  add a `Docs:` line naming the doc PR or file, or `Docs: none, <reason>`");
    out.push_str("\n  write the doc with the fno-doc-writing skill; check it with asd-ste100");
    out
}

/// The merge-side blocker over facts already read. `files` is `None` when
/// the changed-file list was unreadable: with no `Docs:` line the gate
/// cannot answer (unknown, never a verdict); a satisfied `Docs:` line is
/// proof the obligation was met whatever the files were.
pub fn docs_blocker(files: Option<&[String]>, body: &str, cwd: &Path, pr: u64) -> Option<Blocker> {
    let patterns = user_surface_paths(cwd);
    if patterns.is_empty() {
        return None;
    }
    if body_names_doc(body) {
        return None;
    }
    let Some(files) = files else {
        return Some(Blocker {
            code: "docs_surface_unknown".to_string(),
            class: BlockerClass::Unknown,
            detail: format!(
                "PR {pr}: the changed-file list was unreadable, so the docs gate cannot answer"
            ),
        });
    };
    let touched = surface_files(files, cwd);
    if touched.is_empty() {
        return None;
    }
    Some(Blocker {
        code: "docs_line_missing".to_string(),
        class: BlockerClass::Held,
        detail: format!(
            "PR {pr} changed user-surface files with no `Docs:` line in the body: {}",
            missing_doc_text(&touched)
        ),
    })
}

/// The merge-side gate: the PR's own body plus its changed files, read
/// through the caller's probes. Disarmed projects spend no GitHub call.
pub fn docs_gate_blocker<P: GateReads>(probes: &P, cwd: &Path, facts: &PrFacts) -> Option<Blocker> {
    if user_surface_paths(cwd).is_empty() {
        return None;
    }
    let files = crate::merge_gates::pr_file_paths(probes, cwd, facts.number);
    docs_blocker(
        files.as_deref(),
        facts.body.as_deref().unwrap_or(""),
        cwd,
        facts.number,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn docs_pr_ref_names_the_url_repo_the_hash_ref_and_the_bare_pr() {
        // AC2-HP: the slug canonicalizes against the rename aliases, so a
        // token written under the repo's old name returns the canonical
        // repo. The hash ref, the full url, the comma annotation and
        // PR <n> all name a PR. AC2-EDGE: the reasoned none, the bare none,
        // a file path and no line name none.
        let dir = tempfile::tempdir().unwrap();
        let fno = dir.path().join(".fno");
        std::fs::create_dir_all(&fno).unwrap();
        std::fs::write(
            fno.join("config.toml"),
            "[repo_aliases]\n\"fnodotdev/fno-web\" = \"footnote-sh/fno-web\"\n",
        )
        .unwrap();
        let cwd = Some(dir.path());
        assert_eq!(
            docs_pr_ref("Docs: fnodotdev/fno-web#244", cwd),
            Some((Some("footnote-sh/fno-web".to_string()), 244))
        );
        assert_eq!(
            docs_pr_ref("Docs: https://github.com/fnodotdev/fno-web/pull/9", cwd),
            Some((Some("footnote-sh/fno-web".to_string()), 9))
        );
        // A token under the canonical name passes through, and a slug with
        // no alias row keeps its verbatim lowercased form.
        assert_eq!(
            docs_pr_ref("Docs: fno-web#45", cwd),
            Some((Some("fno-web".to_string()), 45))
        );
        assert_eq!(
            docs_pr_ref(
                "Docs: fnodotdev/fno-web#244 docs/guides/manage-backlog.next.mdx",
                cwd
            ),
            Some((Some("footnote-sh/fno-web".to_string()), 244))
        );
        assert_eq!(
            docs_pr_ref("Docs: fno-web#244, merged", cwd),
            Some((Some("fno-web".to_string()), 244))
        );
        assert_eq!(docs_pr_ref("Docs: PR 45", cwd), Some((None, 45)));
        assert_eq!(
            docs_pr_ref("Docs: `fno-web#45`", cwd),
            Some((Some("fno-web".to_string()), 45))
        );
        assert_eq!(docs_pr_ref("Docs: none, no user surface", cwd), None);
        assert_eq!(docs_pr_ref("Docs: none\n", cwd), None);
        assert_eq!(docs_pr_ref("Docs: docs/x.next.mdx", cwd), None);
        assert_eq!(docs_pr_ref("no Docs line", cwd), None);
    }

    #[test]
    fn a_docs_line_passes_only_with_a_doc_or_a_reasoned_none() {
        // A doc reference passes; none without a reason does not.
        assert!(body_names_doc("## Summary\n\nfixes x\nDocs: fno-web#45\n"));
        assert!(body_names_doc("Docs: docs/reference/pr.mdx\n"));
        assert!(!body_names_doc("Docs: none\n"));
        // A promise of a doc is no doc.
        assert!(!body_names_doc("Docs: owed\n"));
        assert!(!body_names_doc("Docs: todo, after merge\n"));
        assert!(!body_names_doc("Docs: n/a\n"));
        assert!(body_names_doc(
            "Docs: https://github.com/o/fno-web/pull/9\n"
        ));
        // Punctuation around the doc token never hides it; `PR <n>` names a PR.
        assert!(body_names_doc("Docs: docs/next.mdx,\n"));
        assert!(body_names_doc("Docs: (docs/next.mdx)\n"));
        assert!(body_names_doc("Docs: fno-web#244, merged\n"));
        assert!(body_names_doc("Docs: PR 244\n"));
        assert!(body_names_doc("Docs: see pr 244.\n"));
        assert!(!body_names_doc("Docs: PR owed\n"));
        // Backticks around a path or a PR ref never hide the doc either.
        assert!(body_names_doc("Docs: `docs/next.mdx`\n"));
        assert!(body_names_doc(
            "Docs: `docs/a.md` and `docs/b.md`, both updated in this PR\n"
        ));
        assert!(!body_names_doc("Docs: `owed`\n"));
        // The lowercase spelling carries the same meaning.
        assert!(body_names_doc("docs: docs/reference/pr.mdx\n"));
        assert!(body_names_doc(
            "Docs: none, internal gate, no user surface copy\n"
        ));
        // The first Docs: line with content decides; empty ones never count.
        assert!(!body_names_doc("Docs:\nDocs: none\n"));
        assert!(body_names_doc("Docs:\nDocs: docs/x.mdx\n"));
        // No Docs line at all is the common refusal case.
        assert!(!body_names_doc("## Summary\n\nfixes x\n"));
    }

    #[test]
    fn surface_files_match_paint_and_builtin_but_stay_disarmed_elsewhere() {
        let bare = tempfile::tempdir().unwrap();
        // A bare directory carries no mux client tree: no patterns arm.
        assert!(surface_files(&paths(&["skills/x/SKILL.md"]), bare.path()).is_empty());
        // A footnote-shaped tree arms the built-ins; the paint list joins
        // them through the same config resolver.
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("crates/fno/src/ui/client")).unwrap();
        let touched = surface_files(
            &paths(&[
                "crates/fno-agent/src/cli_args.rs",
                "crates/fno-agent/src/gate/pr_create.rs",
                "crates/fno/src/setup/wizard.rs",
                "crates/fno/config_schema.json",
                "crates/fno-agent/src/spawn/accounts/use_switch.rs",
                "skills/ship/SKILL.md",
                "commands/ship.md",
                "cli/src/fno/ship.py",
                "notes.txt",
            ]),
            repo.path(),
        );
        assert_eq!(
            touched,
            paths(&[
                "crates/fno-agent/src/cli_args.rs",
                "crates/fno/src/setup/wizard.rs",
                "crates/fno/config_schema.json",
                "crates/fno-agent/src/spawn/accounts/use_switch.rs",
                "skills/ship/SKILL.md",
                "commands/ship.md",
                "cli/src/fno/ship.py",
            ])
        );
    }

    #[test]
    fn the_blocker_holds_an_undocumented_surface_and_names_both_skills() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("crates/fno/src/ui/client")).unwrap();
        let files = paths(&[
            "crates/fno-agent/src/cli_args.rs",
            "crates/fno-agent/src/lib.rs",
        ]);
        let held = docs_blocker(Some(&files), "## Summary\n", repo.path(), 42)
            .expect("an undocumented surface holds");
        assert_eq!(held.code, "docs_line_missing");
        assert!(matches!(held.class, BlockerClass::Held));
        assert!(
            held.detail.contains("Docs: none, <reason>"),
            "{}",
            held.detail
        );
        assert!(held.detail.contains("fno-doc-writing"), "{}", held.detail);
        assert!(held.detail.contains("asd-ste100"), "{}", held.detail);
        // A satisfied Docs line clears the same diff.
        assert!(docs_blocker(Some(&files), "Docs: fno-web#9\n", repo.path(), 42).is_none());
        // An unreadable file list is unknown, never a pass, until a Docs
        // line proves the obligation met.
        let unknown = docs_blocker(None, "## Summary\n", repo.path(), 42).unwrap();
        assert_eq!(unknown.code, "docs_surface_unknown");
        assert!(matches!(unknown.class, BlockerClass::Unknown));
        assert!(docs_blocker(None, "Docs: docs/x.mdx\n", repo.path(), 42).is_none());
        // An internal-only diff never fires.
        let internal = paths(&["crates/fno-agent/src/lib.rs"]);
        assert!(docs_blocker(Some(&internal), "## Summary\n", repo.path(), 42).is_none());
        // The refusal text carries the same contract for the create leg.
        let text = missing_doc_text(&files);
        assert!(
            text.contains("`Docs:` line naming the doc PR or file"),
            "{text}"
        );
        assert!(
            text.contains("fno-doc-writing") && text.contains("asd-ste100"),
            "{text}"
        );
    }
}
