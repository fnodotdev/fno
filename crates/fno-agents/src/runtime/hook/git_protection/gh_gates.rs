//! The two `gh` text gates: the GraphQL reserve (direct PR reads route
//! through `fno do pr`) and the `gh pr create` closure line, a notice since
//! the guard purge (the merge gate still enforces the trailer).

use super::segments::{self, base_lower, effective_argv, Segment};
use super::Ctx;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// A wrong `--body-file` path must never make the hook read something large.
const BODY_FILE_CAP: u64 = 1 << 20;

/// Drop the repo and host options gh takes before its command words.
pub(super) fn strip_gh_global_opts(argv: &[String]) -> Vec<String> {
    fn strip(mut tokens: &[String]) -> &[String] {
        while let Some(token) = tokens.first() {
            if matches!(token.as_str(), "-R" | "--repo" | "--hostname") {
                if tokens.len() < 2 {
                    return &[];
                }
                tokens = &tokens[2..];
            } else if (token.starts_with("-R") && token != "-R")
                || token.starts_with("--repo=")
                || token.starts_with("--hostname=")
            {
                tokens = &tokens[1..];
            } else {
                break;
            }
        }
        tokens
    }
    let mut out: Vec<String> = argv.iter().take(1).cloned().collect();
    let command = strip(&argv[argv.len().min(1)..]);
    if command.first().map(String::as_str) == Some("pr") {
        out.push("pr".to_string());
        out.extend(strip(&command[1..]).iter().cloned());
    } else {
        out.extend(command.iter().cloned());
    }
    out
}
fn closure_marker() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)CLOSURE_TRAILER|Backlog-Closure|Fixes\s+[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}")
            .expect("static regex")
    })
}

fn find_pr_create_segments(segs: &[Segment]) -> Vec<&Segment> {
    segs.iter()
        .filter(|seg| {
            let argv = effective_argv(seg);
            if argv.first().map(|t| base_lower(t)) != Some("gh".to_string()) {
                return false;
            }
            let argv = strip_gh_global_opts(&argv);
            argv.len() >= 3
                && argv[1].eq_ignore_ascii_case("pr")
                && argv[2].eq_ignore_ascii_case("create")
        })
        .collect()
}

/// `(hatch, head, body_files)` for one create segment, each by argv
/// position: a `--body` quoting `--head x` or `FNO_PR_CLOSURE_OK=1` in
/// prose is a word, never the flag.
fn pr_create_signals(seg: &Segment) -> (bool, Option<String>, Vec<String>) {
    let (argv, assignments) = segments::effective_argv_and_assignments(seg);
    let hatch = assignments.iter().any(|a| a == "FNO_PR_CLOSURE_OK=1");
    let argv = strip_gh_global_opts(&argv);
    let mut head = None;
    let mut body_files = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let tok = argv[i].as_str();
        if matches!(tok, "--head" | "-H") && i + 1 < argv.len() {
            head = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        if let Some(v) = tok.strip_prefix("--head=") {
            head = Some(v.to_string());
        } else if matches!(tok, "--body-file" | "-F") && i + 1 < argv.len() {
            body_files.push(argv[i + 1].clone());
            i += 2;
            continue;
        } else if let Some(v) = tok.strip_prefix("--body-file=") {
            body_files.push(v.to_string());
        }
        i += 1;
    }
    (hatch, head, body_files)
}

/// `os.path.normpath`, lexical: collapse `//`, `.` and `..`.
fn normpath(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// True (allow) unless the file exists, reads under the cap and carries no
/// closure marker. A path another segment of the same command names allows:
/// the hook runs before that write-then-create flow writes it.
fn body_file_allows(path: &str, cwd: &std::path::Path, touched: &HashSet<String>) -> bool {
    use std::io::Read;
    let full = cwd.join(path);
    let mut bytes = Vec::new();
    match std::fs::File::open(&full) {
        Ok(file) => {
            if file.take(BODY_FILE_CAP).read_to_end(&mut bytes).is_err() {
                return true;
            }
        }
        Err(_) => return true,
    }
    let meta = std::fs::metadata(&full);
    if touched.contains(&normpath(path).to_lowercase())
        || !meta.as_ref().is_ok_and(|m| m.is_file())
        || meta.is_ok_and(|m| m.len() > BODY_FILE_CAP)
    {
        return true;
    }
    let text = String::from_utf8_lossy(&bytes).replace('\0', "");
    closure_marker().is_match(&text)
}

/// The refusal for a `gh pr create` whose branch names a node and whose
/// command shows no composed closure line. It asserts a positive marker
/// that the composition step ran, never an absence: a false allow is CI's
/// to catch, a false deny is the defect.
pub(super) fn closure_refusal(
    command: &str,
    segs: Option<&[Segment]>,
    ctx: &Ctx,
) -> Option<String> {
    static FALLBACK: OnceLock<Regex> = OnceLock::new();
    let creates: Vec<(bool, Option<String>, Vec<String>)> = match segs {
        Some(segs) => find_pr_create_segments(segs)
            .into_iter()
            .map(pr_create_signals)
            .collect(),
        None => {
            let re =
                FALLBACK.get_or_init(|| Regex::new(r"(?i)gh\s+pr\s+create").expect("static regex"));
            re.find_iter(command)
                .map(|_| (false, None, Vec::new()))
                .collect()
        }
    };
    if creates.is_empty() {
        return None;
    }
    let create_segs: Vec<&Segment> = segs.map(find_pr_create_segments).unwrap_or_default();
    let touched: HashSet<String> = segs
        .unwrap_or_default()
        .iter()
        .filter(|seg| !create_segs.contains(seg))
        .flatten()
        .map(|t| normpath(t).to_lowercase())
        .collect();
    for (hatch, head, body_files) in creates {
        if let Some(reason) = one_closure_refusal(command, hatch, head, &body_files, &touched, ctx)
        {
            return Some(reason);
        }
    }
    None
}

fn one_closure_refusal(
    command: &str,
    hatch: bool,
    head: Option<String>,
    body_files: &[String],
    touched: &HashSet<String>,
    ctx: &Ctx,
) -> Option<String> {
    // The hook is a separate process: an inline `FNO_PR_CLOSURE_OK=1 gh pr
    // create` never reaches this env, so the assignment prefix is read too.
    if hatch || std::env::var("FNO_PR_CLOSURE_OK").as_deref() == Ok("1") {
        return None;
    }
    let head_ref = head
        .or_else(|| ctx.probes.current_branch())
        .unwrap_or_default();
    let ids = ctx.probes.branch_node_ids(&head_ref);
    if ids.is_empty() || closure_marker().is_match(command) {
        return None;
    }
    let judged: Vec<&String> = body_files
        .iter()
        .filter(|p| !body_file_allows(p, &ctx.cwd, touched))
        .collect();
    if !body_files.is_empty() && judged.is_empty() {
        return None;
    }
    let detail = judged
        .first()
        .map(|p| {
            format!(
                "the body file {p} exists and carries no Fixes line; or open the whole-path door /fno:ship pr create.\n"
            )
        })
        .unwrap_or_default();
    // Names candidates, never a trailer to paste: a branch segment can fit
    // the id grammar without being a node, and the generator reads the graph.
    // A notice since the 2026-11-07 guard purge: the merge gate still reds
    // the PR, so the advice arrives before that cost without refusing.
    Some(format!(
        "[fno closure trailer] notice: branch segments that fit the node-id grammar: {}. \
         check-pr-node-closure reds this PR unless the body claims at least one REAL node.\n\
         {detail}Generate the ONE line (graph-checked, with contained_in descendants) via \
         `fno do pr closure-trailer <node-id> --extra <id> [...]` and paste its output; the \
         gate reads only the last closure line, so replace every line in the body with this \
         one.\nDo NOT paste a candidate from this message: a segment can match the grammar \
         without being a node, and one unknown id voids the whole binding at merge.\n\
         Ad-hoc PR that closes nothing: re-run with FNO_PR_CLOSURE_OK=1.",
        ids.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fake::{bash, Fake};
    use super::super::{judge_verdict, Ctx, Verdict};
    use super::*;

    fn ctx<'a>(dir: &std::path::Path, fake: &'a Fake) -> Ctx<'a> {
        Ctx {
            home: dir.join("home"),
            cwd: dir.to_path_buf(),
            probes: fake,
        }
    }

    #[test]
    fn pr_create_on_a_node_branch_notices_a_missing_closure_marker() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake {
            branch: Some("feature/x-aaaa-port".into()),
            node_ids: vec!["x-aaaa".into()],
            ..Fake::default()
        };
        let c = ctx(dir.path(), &fake);
        let verdict = |cmd: &str| judge_verdict(&bash(cmd), &c);
        // A notice, never a refusal (the 2026-11-07 guard-purge ruling): the merge gate
        // still reds the PR.
        match verdict("gh pr create --title t --body \"$BODY\"") {
            Verdict::Warn(notice) => {
                assert!(
                    notice.contains("x-aaaa") && notice.contains("FNO_PR_CLOSURE_OK=1"),
                    "{notice}"
                );
            }
            other => panic!("expected a notice, got {other:?}"),
        }
        assert!(matches!(
            verdict("FNO_PR_CLOSURE_OK=1 gh pr create -t t"),
            Verdict::Allow
        ));
        assert!(matches!(
            verdict("gh pr create -t t --body \"Fixes x-aaaa\""),
            Verdict::Allow
        ));
        assert!(matches!(
            verdict("gh pr create -t t --body \"FNO_PR_CLOSURE_OK=1\""),
            Verdict::Warn(_)
        ));
        // A body file is judged on its own trailer; a missing one allows.
        std::fs::write(dir.path().join("body.md"), "no trailer").unwrap();
        match verdict("gh pr create -t t --body-file body.md") {
            Verdict::Warn(notice) => {
                assert!(notice.contains("the body file body.md exists"), "{notice}");
            }
            other => panic!("expected a notice, got {other:?}"),
        }
        std::fs::write(dir.path().join("ok.md"), "Fixes x-aaaa\n").unwrap();
        assert!(matches!(
            verdict("gh pr create -t t --body-file ok.md"),
            Verdict::Allow
        ));
        assert!(matches!(
            verdict("gh pr create -t t -F missing.md"),
            Verdict::Allow
        ));
        // A file another segment writes first is not judged stale.
        assert!(matches!(
            verdict("printf x > body.md && gh pr create -t t -F body.md"),
            Verdict::Allow
        ));
    }

    #[test]
    fn pr_create_on_a_branch_naming_no_node_is_not_gated() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake {
            branch: Some("chore/docs".into()),
            ..Fake::default()
        };
        assert!(matches!(
            judge_verdict(&bash("gh pr create -t t"), &ctx(dir.path(), &fake)),
            Verdict::Allow
        ));
        assert_eq!(normpath("./a//b/../c"), "a/c");
    }
}
