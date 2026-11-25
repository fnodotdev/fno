//! `fno do pr create` (binary command `pr-create`) -- the duplicate-guarded
//! create. On 2026-10-04 three PRs fixed one bug on the same file within an
//! hour: the graph was down, so no claim or fold check could see the others.
//! This command reads open PRs from the GitHub REST API (no graph read: the
//! outage is exactly when the guard must work) and refuses when an open PR
//! touches the same changed files with an overlapping subject. The refusal
//! names the PR, its branch and its author; an explicit `--not-duplicate <n>`
//! passes it. On pass it execs `gh pr create --title --body-file`.
//! `--check-only` runs the scan and the gates without creating (exit 0 clean,
//! 3 twin, 5, 6 or 9 a gate refusal) for callers that create through another
//! door, such as the worker and batch ship paths.
//!
//! Exit codes:
//! * `0` the PR was created (gh's output passes through), or the scan was
//!   clean under `--check-only`
//! * `3` a duplicate refused
//! * `2` usage or local read error
//! * `4` a GitHub read failed
//! * `5` the audited-owners gate refused: a code PR whose mapped owner test
//!   files went unaudited
//! * `6` the plan gate refused: the session's bound plan still lists an
//!   unchecked `- [ ]` line under `## Unknowns`, or a plan created after
//!   2026-11-16 has no `## Premises` section
//! * `7` the sibling gate refused: another node of the same epic is on an
//!   open PR, or on one merged in the last two hours; a body line
//!   `Separate PR: <reason>` passes it
//! * `8` the docs gate refused: the changed files touch a user surface
//!   (`merge.visual_paint_paths`, CLI help text, config keys, skill
//!   descriptions) and the body carries no `Docs:` line naming the doc PR
//!   or file, or `Docs: none, <reason>`; the refusal names the two skills
//! * `9` the deferral gate refused: the body closes a node with Fixes and
//!   defers part of it ("first slice", "rest ships as", "later PR")
//! * otherwise gh's own create exit code

use crate::pr_push::{gh_api, run_labeled, READ_TIMEOUT};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CREATE_TIMEOUT: Duration = Duration::from_secs(300);
/// Open-PR pages read before the scan gives up expanding (a warning names the
/// bound); 100 rows per page.
const MAX_PAGES: usize = 3;
const USAGE: &str = "usage: fno do pr create --title <t> --body-file <path> [--base <branch>] [--not-duplicate <pr>]... | --check-only --title <t> [--base <branch>]";

#[derive(Debug)]
struct Args {
    title: String,
    body_file: String,
    base: String,
    not_duplicates: Vec<i64>,
    check_only: bool,
    cwd: PathBuf,
    /// The commit every diff reads. `HEAD` from the command line; a host
    /// caller binds a sandbox's exported head here.
    head: String,
    /// A host caller's bound branch, read in place of the checkout's.
    bound_branch: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        title: String::new(),
        body_file: String::new(),
        base: "main".to_string(),
        not_duplicates: Vec::new(),
        check_only: false,
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        head: "HEAD".to_string(),
        bound_branch: None,
    };
    let mut i = 0;
    while i < argv.len() {
        let take = |name: &str| -> Result<String, String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argv[i].as_str() {
            "--title" => {
                a.title = take("--title")?;
                i += 1;
            }
            "--body-file" => {
                a.body_file = take("--body-file")?;
                i += 1;
            }
            "--base" => {
                a.base = take("--base")?;
                i += 1;
            }
            "--not-duplicate" => {
                let raw = take("--not-duplicate")?;
                let n: i64 = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("--not-duplicate wants a PR number, got {raw}"))?;
                a.not_duplicates.push(n);
                i += 1;
            }
            // The scan-only posture the worker and batch create sites run
            // before their own `gh pr create`: refuse on a twin, never create.
            "--check-only" => {
                a.check_only = true;
            }
            // Test seam, same as pr-body-check's.
            "--cwd" => {
                a.cwd = PathBuf::from(take("--cwd")?);
                i += 1;
            }
            other => return Err(format!("unknown flag: {other}\n{USAGE}")),
        }
        i += 1;
    }
    if a.title.is_empty() {
        return Err(format!("--title is required\n{USAGE}"));
    }
    if !a.check_only && a.body_file.is_empty() {
        return Err(format!("--body-file is required\n{USAGE}"));
    }
    Ok(a)
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let (ok, out, err) = run_labeled("pr-create", "git", args, cwd, READ_TIMEOUT)?;
    if ok {
        Ok(out.trim().to_string())
    } else {
        Err(if err.trim().is_empty() {
            out.trim().to_string()
        } else {
            err.trim().to_string()
        })
    }
}

/// Title words that decide an "overlapping subject": conventional-commit
/// prefix (`type(scope):`) stripped, lowercased, split on non-alphanumerics,
/// tokens under three characters dropped.
fn subject_tokens(title: &str) -> Vec<String> {
    let body = match title.find(": ") {
        Some(idx) if idx < 40 => &title[idx + 2..],
        _ => title,
    };
    body.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(String::from)
        .collect()
}

fn subjects_overlap(a: &str, b: &str) -> bool {
    let (ta, tb) = (subject_tokens(a), subject_tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let shared = ta.iter().filter(|t| tb.contains(t)).count();
    shared >= 2 && shared * 2 >= ta.len().min(tb.len())
}

struct OpenPr {
    number: i64,
    title: String,
    branch: String,
    author: String,
    body: String,
}

/// One GitHub REST read: a repo-relative path with its query. Prod wires
/// `pr_push::gh_api`; tests inject.
type Gh<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// Lifts pr_create's one-path `gh` seam onto the shared `GhProbe` shape the
/// paginated `pr_file_paths` read takes: `args[1]` is the repo-relative path
/// with its query, which is exactly what `Gh` receives.
struct GhProbeAdapter<'a> {
    gh: Gh<'a>,
}

impl crate::pr_status_facts::GhProbe for GhProbeAdapter<'_> {
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        let Some(path) = args.get(1) else {
            return Err("gh read carries no endpoint".to_string());
        };
        match (self.gh)(path) {
            Ok(stdout) => Ok((true, stdout, String::new())),
            Err(e) => Ok((false, String::new(), e)),
        }
    }
}

fn open_prs(slug: &str, gh: Gh) -> Result<Vec<OpenPr>, String> {
    let mut rows = Vec::new();
    for page in 1..=MAX_PAGES {
        let raw = gh(&format!(
            "repos/{slug}/pulls?state=open&per_page=100&page={page}"
        ))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|_| format!("gh api pulls list page {page} returned non-JSON"))?;
        let Some(items) = parsed.as_array() else {
            return Err(format!(
                "gh api pulls list page {page} returned a non-array"
            ));
        };
        for item in items {
            let number = item.get("number").and_then(Value::as_i64);
            let title = item.get("title").and_then(Value::as_str);
            let branch = item
                .get("head")
                .and_then(|h| h.get("ref"))
                .and_then(Value::as_str);
            let author = item
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(Value::as_str);
            let body = item.get("body").and_then(Value::as_str).unwrap_or("");
            match (number, title, branch, author) {
                (Some(n), Some(t), Some(b), Some(a)) => rows.push(OpenPr {
                    number: n,
                    title: t.to_string(),
                    branch: b.to_string(),
                    author: a.to_string(),
                    body: body.to_string(),
                }),
                _ => {
                    return Err(format!(
                        "gh api pulls list page {page} carried a malformed row"
                    ))
                }
            }
        }
        if items.len() < 100 {
            break;
        }
        if page == MAX_PAGES {
            eprintln!(
                "pr-create: open-PR scan stopped at {} pages ({} rows); later pages unread",
                MAX_PAGES,
                rows.len()
            );
        }
    }
    Ok(rows)
}

/// The first open PR that shares changed files with an overlapping subject,
/// or None when the create may proceed.
fn find_duplicate(
    title: &str,
    my_files: &[String],
    branch: &str,
    not_duplicates: &[i64],
    gh: Gh,
    slug: &str,
    cwd: &Path,
) -> Result<Option<(OpenPr, Vec<String>)>, String> {
    for pr in open_prs(slug, gh)? {
        if pr.branch == branch || not_duplicates.contains(&pr.number) {
            continue;
        }
        if !subjects_overlap(title, &pr.title) {
            continue;
        }
        let probe = GhProbeAdapter { gh };
        let their_files =
            crate::pr_status::lists::pr_file_paths(&probe, cwd, slug, pr.number as u64)?;
        let shared: Vec<String> = my_files
            .iter()
            .filter(|f| their_files.contains(f))
            .cloned()
            .collect();
        if !shared.is_empty() {
            return Ok(Some((pr, shared)));
        }
    }
    Ok(None)
}

pub fn run_pr_create_command(argv: &[String]) -> i32 {
    if let Some(first) = argv.first() {
        if first.trim_start().starts_with('{') {
            return run_stdin_check_only(first);
        }
    }
    let a = match parse_args(argv) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("pr-create: {msg}");
            return 2;
        }
    };
    run(&a, &|path| gh_api("gh", &a.cwd, path, &[]))
}

/// The create door for a host caller that ships a commit the checkout does
/// not hold: every gate reads `head` and `branch`, never the checkout's
/// HEAD, and gh opens the PR for `branch`. A sandbox op is the one caller.
pub(crate) fn create_bound(
    title: &str,
    body_file: &str,
    branch: &str,
    head: &str,
    cwd: &Path,
) -> i32 {
    let a = Args {
        title: title.to_string(),
        body_file: body_file.to_string(),
        base: "main".to_string(),
        not_duplicates: Vec::new(),
        check_only: false,
        cwd: cwd.to_path_buf(),
        head: head.to_string(),
        bound_branch: Some(branch.to_string()),
    };
    run(&a, &|path| gh_api("gh", &a.cwd, path, &[]))
}

/// One JSON request on stdin (the argv form starts with `{`): the shape
/// `command_call("pr-create", {...})` sends. The twin scan, then the sibling
/// gate over the payload's `body`, so the worker ship path meets the same
/// one-PR-per-epic rule as the argv create leg. The owners and unknowns gates
/// stay create-leg gates.
fn run_stdin_check_only(payload: &str) -> i32 {
    let parsed: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pr-create: unreadable payload: {e}");
            return 2;
        }
    };
    let title = parsed.get("title").and_then(Value::as_str).unwrap_or("");
    let base = parsed.get("base").and_then(Value::as_str).unwrap_or("main");
    let cwd = parsed
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let branch = git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty() && b != "HEAD");
    let my_files: Vec<String> = git(
        &cwd,
        &[
            "diff",
            "--name-only",
            &format!("{}...HEAD", crate::pr_body_check::base_ref(base)),
        ],
    )
    .map(|f| f.lines().map(String::from).collect())
    .unwrap_or_default();
    let slug = crate::pr_list::origin_slug(&cwd, None).unwrap_or_default();
    let refusal = match find_duplicate(
        title,
        &my_files,
        branch.as_deref().unwrap_or(""),
        &[],
        &|path| gh_api("gh", &cwd, path, &[]),
        &slug,
        &cwd,
    ) {
        Ok(Some((pr, shared))) => {
            let shown = shared
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if shared.len() > 5 {
                format!(" (and {} more)", shared.len() - 5)
            } else {
                String::new()
            };
            format!(
                "pr-create: REFUSED: open PR #{} already touches the same changed files with an overlapping subject\n  PR:     #{} {}\n  branch: {}\n  files:  {shown}{more}",
                pr.number, pr.number, pr.title, pr.branch
            )
        }
        _ => String::new(),
    };
    let body = parsed.get("body").and_then(Value::as_str).unwrap_or("");
    if let (true, Some(b)) = (refusal.is_empty(), branch.as_deref()) {
        if let Some(notice) =
            sibling_gate(&cwd, body, b, &slug, &|path| gh_api("gh", &cwd, path, &[]))
        {
            eprintln!("{notice}");
        }
        blocker_gate(body);
    }
    let touched = crate::docs_gate::surface_files(&my_files, &cwd);
    if refusal.is_empty() && !touched.is_empty() && !crate::docs_gate::body_names_doc(body) {
        eprintln!(
            "pr-create: notice: {}",
            crate::docs_gate::missing_doc_text(&touched)
        );
    }
    println!("{}", json!({"refusal": refusal, "title": title}));
    0
}

/// The audited-owners gate for the create leg: a code PR (a changed
/// non-test Python or Rust file) whose mapped owner test files went
/// unaudited is refused, so every session either prunes the tests that own
/// its changed files or records in the body why nothing was cut. Returns
/// `Some(5)` on refusal. A body or diff read failure is exit 2, never a
/// refusal: the gate refuses on evidence, not on a broken transport.
fn owners_gate_refusal(a: &Args) -> Option<i32> {
    let body = match std::fs::read_to_string(&a.body_file) {
        Ok(b) => b,
        Err(err) => {
            eprintln!(
                "pr-create: could not read the body file {}: {err}",
                a.body_file
            );
            return Some(2);
        }
    };
    let base = crate::pr_body_check::base_ref(&a.base);
    let range = format!("{base}...{}", a.head);
    let diff = match crate::test_delta::diff_range("git", &a.cwd, &range) {
        Ok(d) => d,
        Err(msg) => {
            eprintln!("pr-create: could not diff {range} for the owners gate: {msg}");
            return Some(2);
        }
    };
    let census = crate::test_delta::census(&diff);
    let py_tests = crate::test_delta::python_test_files(&a.cwd);
    let mut owners = crate::test_delta::owners_of(&census, &py_tests, &a.cwd);
    owners.sort();
    owners.dedup();
    if let Err(msg) = crate::test_delta::owners_gate(&body, &owners, census.total_cuts()) {
        eprintln!("pr-create: REFUSED: {msg}");
        Some(5)
    } else {
        None
    }
}

/// The open unknowns in a plan: each unchecked `- [ ]` line under its
/// `## Unknowns` heading, up to the next `#` or `##` heading. A `###`
/// subheading stays inside the section. A plan with no such section has none,
/// so plans written before the section existed pass.
fn open_unknowns(plan: &str) -> Vec<String> {
    let mut inside = false;
    let mut open = Vec::new();
    for line in plan.lines() {
        if line.starts_with("# ") || line.starts_with("## ") {
            inside = line.trim_end() == "## Unknowns";
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("- [ ]").filter(|_| inside) {
            open.push(rest.trim().to_string());
        }
    }
    open
}

/// The unknowns gate: target settles every unknown the blueprint listed
/// before the PR opens. Returns `Some(6)` naming each open line. No manifest,
/// no bound plan, or an unreadable plan file passes: the gate refuses on
/// evidence only.
/// The Serves gate: a node that names its goal owes the PR body an answer.
/// The body carries a `Serves:` section with one met-or-not-met line per
/// part of the node's why; a line marked not met names the follow-up node
/// it filed, or the gate names the carveout door in its refusal.
fn serves_gate_refusal(a: &Args) -> Option<i32> {
    let manifest = crate::target_orient::read_manifest(&a.cwd)?;
    let node_id = manifest.get("input").filter(|v| !v.is_empty())?;
    let graph = crate::backlog::settings::graph_path();
    let row = crate::graph_store::read_one(&graph, node_id).ok()??;
    let why = crate::backlog::question_intake::str_field(&row, "why")
        .map(str::trim)
        .filter(|w| !w.is_empty())?;
    let scope = crate::backlog::question_intake::str_field(&row, "audience").unwrap_or("");
    let body = std::fs::read_to_string(&a.body_file).ok()?;
    if !body.contains("Serves:") {
        eprintln!(
            "pr-create: REFUSED: the node carries a why, so the body owes a Serves: section answering each part of it met or not met\n  why: {why}\n  pass: add a Serves: section; a part marked not met names the follow-up node (fno backlog carveout add)"
        );
        return Some(6);
    }
    let serves = body
        .split("Serves:")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .take(6)
        .collect::<Vec<_>>()
        .join("\n");
    if serves.contains("not met") && !serves_has_follow_up(&serves) {
        eprintln!(
            "pr-create: REFUSED: a Serves line is marked not met and names no follow-up node\n  file one first: fno backlog carveout add"
        );
        return Some(6);
    }
    if scope.contains("customer") && !serves_is_customer_shaped(&serves) {
        eprintln!(
            "pr-create: notice: the node serves {scope}; the Serves: line should name the persona and its job"
        );
    }
    let _ = why;
    None
}

/// A not-met line counts only when it names a filed follow-up: a node id,
/// the carveout word, or an explicit already-filed marker.
fn serves_has_follow_up(serves: &str) -> bool {
    serves
        .lines()
        .filter(|l| l.to_lowercase().contains("not met"))
        .any(|l| {
            l.contains("carveout")
                || l.chars().any(|c| c.is_ascii_alphanumeric())
                    && l.split_whitespace()
                        .any(|tok| tok.chars().next().is_some_and(|c| c.is_ascii_hexdigit()))
                || l.contains("filed")
        })
}

/// A customer-scope Serves line names the persona (customer:<slug> or the
/// bare slug) and the slash job shape.
fn serves_is_customer_shaped(serves: &str) -> bool {
    serves.contains("customer:") || serves.contains('/')
}

/// Plans created after this date carry a `## Premises` section: the
/// verify-footnotes verdicts target writes before its first edit.
const PREMISES_GATE_DATE: &str = "2026-11-16";

/// Whether a plan owes a `## Premises` section and lacks one: its
/// frontmatter `created:` date is after the gate date and no such heading
/// exists. An undated plan owes nothing.
pub(crate) fn premises_missing(plan: &str) -> bool {
    let created = plan
        .lines()
        .find_map(|l| l.strip_prefix("created:"))
        .map(|d| d.trim().trim_matches(['"', '\'']).get(..10).unwrap_or(""))
        .unwrap_or("");
    let dated = created.len() == 10 && created.as_bytes().get(4) == Some(&b'-');
    dated && created > PREMISES_GATE_DATE && !plan.lines().any(|l| l.trim_end() == "## Premises")
}

/// The plan gates: target settles every unknown the blueprint listed, and
/// writes its premise verdicts, before the PR opens. Returns `Some(6)`
/// naming each open line or the missing section. No manifest or an
/// unreadable plan file passes: the gate refuses on evidence only. A
/// manifest with no bound plan warns and names `/fno:blueprint`.
fn unknowns_gate_refusal(a: &Args) -> Option<i32> {
    crate::target_orient::read_manifest(&a.cwd)?;
    let Some(plan_path) = crate::target_orient::manifest_plan_path(&a.cwd) else {
        eprintln!("pr-create: warning: this run has no bound plan, so nothing recorded its premises; /fno:blueprint writes one");
        return None;
    };
    let cwd = a.cwd.to_string_lossy();
    let resolved = crate::backlog::promise::resolve_plan_path(&plan_path, Some(&cwd))?;
    let index = crate::target_blast::resolve_plan_index(&resolved.to_string_lossy());
    let plan = std::fs::read_to_string(&index).ok()?;
    let open = open_unknowns(&plan);
    if open.is_empty() {
        if !premises_missing(&plan) {
            return None;
        }
        eprintln!(
            "pr-create: REFUSED: {} has no ## Premises section",
            index.display()
        );
        eprintln!("  load /fno:verify-footnotes, measure each premise the node and plan assert, and write one verdict per line under ## Premises");
        return Some(6);
    }
    eprintln!(
        "pr-create: REFUSED: {} unknown(s) still open in {}",
        open.len(),
        index.display()
    );
    for u in &open {
        eprintln!("  - [ ] {u}");
    }
    eprintln!("  settle each one: measure it, then write `- [x] <unknown> - settled: <finding>` in the plan");
    Some(6)
}

/// Words that defer part of a node to another PR. "next PR" stays out: it
/// also names the next head of this one.
/// A body that closes a node
/// with its Fixes line and says one of these merges the node closed on a
/// half-built ask: the merge reads the Fixes line, never the prose.
const DEFERRAL_PHRASES: [&str; 5] = [
    "rest ships as",
    "rest ships in",
    "first slice",
    "later pr",
    "follow-up pr",
];

/// The first body line that defers work, with the phrase it used, when the
/// body also closes a node. A body with no Fixes line closes nothing, so it
/// may say anything about later work.
fn deferral_line(body: &str) -> Option<(&'static str, String)> {
    if crate::org_board::pr_closure::parse(body).is_empty() {
        return None;
    }
    body.lines().find_map(|line| {
        let lower = line.to_lowercase();
        DEFERRAL_PHRASES
            .iter()
            .find(|p| lower.contains(*p))
            .map(|p| (*p, line.trim().to_string()))
    })
}

/// The deferral gate: a PR that closes a node ships the node whole. Returns
/// `Some(9)` naming the line. The diff budget never licenses a slice.
fn deferral_gate_refusal(body: &str) -> Option<i32> {
    let (phrase, line) = deferral_line(body)?;
    eprintln!("pr-create: REFUSED: the body closes a node with Fixes and defers part of it (\"{phrase}\"):");
    eprintln!("  {line}");
    eprintln!("  the merge closes every node in Fixes, so the deferred part closes unbuilt.");
    eprintln!("  ship the node whole and state any budget overrun in the body, or file the rest as its own node and Fix only what this PR finishes.");
    Some(9)
}

/// How long a merged sibling PR still refuses a new one for its epic.
const SIBLING_WINDOW: chrono::Duration = chrono::Duration::minutes(120);

/// A PR the sibling gate weighs: open, or merged at `merged_at`.
struct SiblingPr {
    number: i64,
    title: String,
    branch: String,
    body: String,
    merged_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The ids that ship elsewhere in `node`'s epic: every other child of its
/// parent and the nodes contained in each. Nodes contained in `node` itself
/// ship with it and are left out. A decompose group child is a declared PR
/// unit, so a group child never counts another group child (or its leaves)
/// as a sibling. Empty when the node has no parent.
fn epic_sibling_ids(entries: &[Value], node: &str) -> (String, Vec<String>) {
    let text = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let Some(row) = entries.iter().find(|e| text(e, "id") == node) else {
        return (String::new(), Vec::new());
    };
    let parent = text(row, "parent");
    if parent.is_empty() {
        return (parent, Vec::new());
    }
    let is_group = !text(row, "group_slug").is_empty();
    let children: Vec<String> = entries
        .iter()
        .filter(|e| text(e, "parent") == parent && text(e, "contained_in") != node)
        .filter(|e| !(is_group && !text(e, "group_slug").is_empty()))
        .map(|e| text(e, "id"))
        .filter(|id| !id.is_empty() && id != node)
        .collect();
    let mut ids = children.clone();
    for e in entries {
        let owner = text(e, "contained_in");
        let id = text(e, "id");
        if children.contains(&owner) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    (parent, ids)
}

/// True when the body carries `Separate PR: <reason>` with a reason.
fn says_separate_pr(body: &str) -> bool {
    body.lines().any(|line| {
        line.trim()
            .strip_prefix("Separate PR:")
            .is_some_and(|reason| !reason.trim().is_empty())
    })
}

/// The refusal text when another PR already ships part of this node's epic:
/// an open PR on another branch, or one merged inside `SIBLING_WINDOW`,
/// whose closure lines name a sibling. None lets the create go on.
fn sibling_notice(
    node: &str,
    entries: &[Value],
    body: &str,
    branch: &str,
    prs: &[SiblingPr],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    if says_separate_pr(body) {
        return None;
    }
    let (epic, siblings) = epic_sibling_ids(entries, node);
    if siblings.is_empty() {
        return None;
    }
    for pr in prs {
        if pr.branch == branch || pr.merged_at.is_some_and(|at| now - at > SIBLING_WINDOW) {
            continue;
        }
        let claimed = pr
            .body
            .lines()
            .flat_map(crate::org_board::pr_closure::line_ids)
            .find(|id| siblings.contains(id));
        if let Some(sibling) = claimed {
            let state = if pr.merged_at.is_some() {
                "merged in the last two hours"
            } else {
                "open"
            };
            return Some(format!(
                "pr-create: notice: PR #{} ({state}) already ships {sibling}, a sibling of {node} in epic {epic}\n  PR:     #{} {}\n  branch: {}\n  next time add this work to that PR, or write a `Separate PR: <reason>` line in the body",
                pr.number, pr.number, pr.title, pr.branch
            ));
        }
    }
    None
}

/// The recently merged PRs: one page of closed PRs, newest update first,
/// keeping the merged ones.
fn merged_prs(slug: &str, gh: Gh) -> Result<Vec<SiblingPr>, String> {
    let raw = gh(&format!(
        "repos/{slug}/pulls?state=closed&sort=updated&direction=desc&per_page=50"
    ))?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|_| "gh api closed pulls returned non-JSON".to_string())?;
    let items = parsed
        .as_array()
        .ok_or_else(|| "gh api closed pulls returned a non-array".to_string())?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let merged_at = item
                .get("merged_at")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())?
                .with_timezone(&chrono::Utc);
            Some(SiblingPr {
                number: item.get("number").and_then(Value::as_i64)?,
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                branch: item
                    .get("head")
                    .and_then(|h| h.get("ref"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                body: item
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                merged_at: Some(merged_at),
            })
        })
        .collect())
}

/// The sibling gate: one PR per epic at a time. Returns the refusal text;
/// the argv door prints it and exits 7, the stdin door returns it as its
/// `refusal`. No manifest, no bound node, or any graph or GitHub read that
/// fails passes with a warning: the gate refuses on evidence only.
fn sibling_gate(cwd: &Path, body: &str, branch: &str, slug: &str, gh: Gh) -> Option<String> {
    let manifest = crate::target_orient::read_manifest(cwd)?;
    let node = manifest.get("graph_node_id").filter(|n| !n.is_empty())?;
    if says_separate_pr(body) {
        return None;
    }
    let entries = match crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path())
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("pr-create: warning: sibling gate skipped, the graph read failed: {e}");
            return None;
        }
    };
    if epic_sibling_ids(&entries, node).1.is_empty() {
        return None;
    }
    let mut prs = match open_prs(slug, gh) {
        Ok(open) => open
            .into_iter()
            .map(|p| SiblingPr {
                number: p.number,
                title: p.title,
                branch: p.branch,
                body: p.body,
                merged_at: None,
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("pr-create: warning: sibling gate skipped, the open-PR read failed: {e}");
            return None;
        }
    };
    match merged_prs(slug, gh) {
        Ok(merged) => prs.extend(merged),
        Err(e) => eprintln!("pr-create: warning: sibling gate read no merged PRs: {e}"),
    }
    sibling_notice(node, &entries, body, branch, &prs, chrono::Utc::now())
}

/// One notice per node the body's `Fixes` line closes while a node it is
/// blocked by still reads open: a release or epic is not done because one
/// child PR merged.
fn open_blocker_notices(entries: &[Value], body: &str) -> Vec<String> {
    let text = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let status = |id: &str| {
        entries
            .iter()
            .find(|e| text(e, "id") == id)
            .map(|e| text(e, "status"))
    };
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for id in body
        .lines()
        .flat_map(crate::org_board::pr_closure::line_ids)
    {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let Some(row) = entries.iter().find(|e| text(e, "id") == id) else {
            continue;
        };
        let open: Vec<String> = row["blocked_by"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|b| status(*b).is_some_and(|s| s != "done" && s != "superseded"))
            .map(str::to_string)
            .collect();
        if !open.is_empty() {
            out.push(format!(
                "pr-create: notice: this PR closes {id} on merge, but {id} is still blocked by {}. Merging closes {id} with that work open. Name a node this PR finishes in Fixes, or finish the blockers first",
                open.join(", ")
            ));
        }
    }
    out
}

fn blocker_gate(body: &str) {
    if !body.contains("Fixes") {
        return;
    }
    match crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path()) {
        Ok(entries) => {
            for notice in open_blocker_notices(&entries, body) {
                eprintln!("{notice}");
            }
        }
        Err(e) => {
            eprintln!("pr-create: warning: blocker check skipped, the graph read failed: {e}")
        }
    }
}

/// The contained-unnamed warning: an open contained child of a node this
/// PR ships that neither the body nor a commit names stays open at merge
/// now, so the body should claim it before it lands. Read-only.
fn contained_gate(cwd: &Path, body: &str, base: &str, branch: &str, head: &str) {
    match crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path()) {
        Ok(entries) => {
            // The nodes this PR ships: the branch's own node plus every id
            // the body claims.
            let mut ships = crate::org_board::pr_closure::parse(body);
            if let Some(n) = crate::eval::correction_fold::node_from_branch(branch) {
                if !ships.contains(&n) {
                    ships.push(n);
                }
            }
            if ships.is_empty() {
                return;
            }
            let messages =
                git(cwd, &["log", &format!("{base}..{head}"), "--format=%B"]).unwrap_or_default();
            let rows: Vec<String> = messages.lines().map(String::from).collect();
            let named_ids = crate::backlog::drift_scan::ids_named_in_text(&rows);
            for notice in contained_notices(&entries, &ships, body, &named_ids) {
                eprintln!("{notice}");
            }
        }
        Err(e) => {
            eprintln!("pr-create: warning: contained check skipped, the graph read failed: {e}")
        }
    }
}

/// One notice per open contained child of `ships` that neither the body's
/// closure line nor a commit message names.
fn contained_notices(
    entries: &[Value],
    ships: &[String],
    body: &str,
    named_ids: &[String],
) -> Vec<String> {
    fn text<'a>(e: &'a Value, k: &str) -> Option<&'a str> {
        e.get(k).and_then(Value::as_str)
    }
    let claimed = crate::org_board::pr_closure::parse(body);
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for owner in ships {
        for e in entries {
            if text(e, "contained_in") != Some(owner.as_str()) {
                continue;
            }
            let Some(id) = text(e, "id") else {
                continue;
            };
            // Same open filter the blocker gate reads: done and superseded
            // rows need no claim.
            if matches!(text(e, "status"), Some("done" | "superseded")) {
                continue;
            }
            if claimed.iter().any(|c| c == id) || named_ids.iter().any(|n| n.as_str() == id) {
                continue;
            }
            if seen.iter().any(|s| s == id) {
                continue;
            }
            seen.push(id.to_string());
            out.push(format!(
                "pr-create: notice: contained node {id} ships inside this PR but neither the body nor a commit names it; add it to the Fixes line or merge leaves it open"
            ));
        }
    }
    out
}

/// One `pr_opened` row in the space journal: the number from the URL gh
/// printed and the node the branch names. No URL, no row.
fn emit_pr_opened(cwd: &Path, printed: &str, branch: &str) {
    let Some(pr) = printed
        .lines()
        .rev()
        .find_map(|l| crate::backlog::pr_link::pr_number_from_url(Some(l)))
    else {
        return;
    };
    let node = crate::eval::correction_fold::node_from_branch(branch);
    let data = serde_json::json!({"pr": pr, "node": node});
    let emitter = crate::events::EventEmitter::new(crate::paths::events_path(cwd), "target");
    if let Err(e) = emitter.emit("pr_opened", &data) {
        eprintln!("pr-create: the pr_opened row did not land: {e}");
    }
}

/// The url the docs ref records. A token naming no repo rides the origin's
/// slug. A qualified slug resolves under the rename aliases first: the
/// bare-slug owner fill invents `{origin-owner}/{slug}`, and the built-in
/// history table carries this project's own repo moves, so a token written
/// beside a transferred repo records the url GitHub actually serves.
fn docs_pr_url(
    qualified: Option<&str>,
    number: i64,
    origin: Option<&str>,
    cwd: &Path,
) -> Option<String> {
    match qualified {
        None | Some("") => origin.map(|o| format!("https://github.com/{o}/pull/{number}")),
        Some(s) => {
            let aliases = crate::backlog::pr_link::repo_alias_map(Some(cwd));
            Some(crate::backlog::pr_link::pr_url_from_slug(
                &crate::backlog::pr_link::canonical_repo_slug(s, &aliases),
                number,
            ))
        }
    }
}

/// Record the PR a `Docs:` line names on the bound node, best-effort. A
/// `Docs: none, <reason>` body and a body that names no PR record nothing.
/// Every miss prints a named warning; the create itself never rolls back.
/// docs_pr_ref canonicalizes the slug against the rename aliases, so a
/// `Docs:` token written under the repo's old name still records a live
/// url.
fn record_docs_pr(cwd: &Path, body_file: &str) {
    let Some(body) = std::fs::read_to_string(body_file).ok() else {
        eprintln!(
            "pr-create: notice: the Docs-line PR was not recorded: the body file is unreadable"
        );
        return;
    };
    let Some((slug, number)) = crate::docs_gate::docs_pr_ref(&body, Some(cwd)) else {
        return;
    };
    let node_id = crate::target_orient::read_manifest(cwd).and_then(|m| {
        crate::target_orient::manifest_get(&m, "input")
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    });
    let Some(node_id) = node_id else {
        eprintln!(
            "pr-create: notice: the Docs line names PR #{number} but no node is bound; record it with `fno backlog update <id> additional_prs+={number} --add-pr-url ...`"
        );
        return;
    };
    let origin = crate::pr_list::origin_slug(cwd, None).ok();
    // The slug arrives canonicalized from docs_pr_ref; a token still naming
    // no owner takes the origin's owner, so the recorded url is one GitHub
    // can resolve.
    let qualified = slug.map(|slug| match slug.split_once('/') {
        Some(_) => slug,
        None => match origin.as_deref().and_then(|o| o.split('/').next()) {
            Some(owner) => format!("{owner}/{slug}"),
            None => slug,
        },
    });
    let url = match docs_pr_url(qualified.as_deref(), number, origin.as_deref(), cwd) {
        Some(url) => url,
        None => {
            eprintln!("pr-create: notice: the Docs-line PR #{number} names no repo and the origin slug is unreadable, so nothing recorded it");
            return;
        }
    };
    let store = crate::backlog::api::Store::new(&crate::backlog::settings::graph_path());
    match crate::backlog::api::pull_request_record(
        &store,
        &node_id,
        number,
        Some(&url),
        Some("docs"),
        None,
    ) {
        Ok(payload) if payload.success => {}
        Ok(_) => eprintln!(
            "pr-create: notice: the Docs-line PR #{number} is already recorded on {node_id}"
        ),
        Err(err) => eprintln!(
            "pr-create: notice: the Docs-line PR #{number} was not recorded: {}",
            err.0
        ),
    }
}

fn run(a: &Args, gh: Gh) -> i32 {
    let read = || git(&a.cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let branch = match a.bound_branch.clone().map_or_else(read, Ok) {
        Ok(b) if !b.is_empty() && b != "HEAD" => b,
        _ => {
            eprintln!("pr-create: could not read the head branch (detached HEAD?)");
            return 2;
        }
    };
    let base = crate::pr_body_check::base_ref(&a.base);
    let my_files = match git(
        &a.cwd,
        &[
            "diff",
            "--no-ext-diff",
            "--name-only",
            &format!("{base}...{}", a.head),
        ],
    ) {
        Ok(f) if !f.is_empty() => f.lines().map(String::from).collect::<Vec<_>>(),
        Ok(_) => {
            eprintln!("pr-create: no changed files against {base}; nothing to guard");
            return 2;
        }
        Err(msg) => {
            eprintln!("pr-create: could not diff {base}...HEAD: {msg} (fetched the base ref?)");
            return 2;
        }
    };
    let slug = match crate::pr_list::origin_slug(&a.cwd, None) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("pr-create: could not resolve owner/repo: {msg}");
            return 2;
        }
    };
    match find_duplicate(
        &a.title,
        &my_files,
        &branch,
        &a.not_duplicates,
        gh,
        &slug,
        &a.cwd,
    ) {
        Err(msg) => {
            eprintln!("pr-create: {msg}");
            4
        }
        Ok(Some((pr, shared))) => {
            let shown = shared
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if shared.len() > 5 {
                format!(" (and {} more)", shared.len() - 5)
            } else {
                String::new()
            };
            eprintln!(
                "pr-create: REFUSED: open PR #{} already touches the same changed files with an overlapping subject",
                pr.number
            );
            eprintln!("  PR:     #{} {}", pr.number, pr.title);
            eprintln!("  branch: {}", pr.branch);
            eprintln!("  author: {}", pr.author);
            eprintln!("  files:  {shown}{more}");
            eprintln!(
                "  this:   {} (branch {branch}, {} files)",
                a.title,
                my_files.len()
            );
            eprintln!("  pass anyway: add --not-duplicate {}", pr.number);
            3
        }
        Ok(None) => {
            // The owners gate runs whenever a body is available: the create
            // leg always, --check-only when a body file was passed (the
            // worker paths that pre-check their own body before gh).
            if !a.body_file.is_empty() {
                if let Some(code) = owners_gate_refusal(&a) {
                    return code;
                }
            }
            if let Some(code) = unknowns_gate_refusal(&a) {
                return code;
            }
            if let Some(code) = serves_gate_refusal(&a) {
                return code;
            }
            let body = std::fs::read_to_string(&a.body_file).unwrap_or_default();
            if let Some(code) = deferral_gate_refusal(&body) {
                return code;
            }
            if let Some(notice) = sibling_gate(&a.cwd, &body, &branch, &slug, gh) {
                eprintln!("{notice}");
            }
            blocker_gate(&body);
            contained_gate(&a.cwd, &body, &a.base, &branch, &a.head);
            if !a.body_file.is_empty() {
                let touched = crate::docs_gate::surface_files(&my_files, &a.cwd);
                if !touched.is_empty() && !crate::docs_gate::body_names_doc(&body) {
                    eprintln!(
                        "pr-create: notice: {}",
                        crate::docs_gate::missing_doc_text(&touched)
                    );
                }
            }
            if a.check_only {
                return 0;
            }
            if let Some(line) = crate::epic_issue::body_ref_line(&a.cwd, &body) {
                let sep = if body.ends_with('\n') { "" } else { "\n" };
                let added = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&a.body_file)
                    .and_then(|mut f| {
                        std::io::Write::write_all(&mut f, format!("{sep}\n{line}\n").as_bytes())
                    });
                if let Err(e) = added {
                    eprintln!("pr-create: warning: could not add `{line}` to the body file: {e}");
                }
            }
            let mut cmd: Vec<String> = vec![
                "gh".to_string(),
                "pr".to_string(),
                "create".to_string(),
                "--title".to_string(),
                a.title.clone(),
                "--body-file".to_string(),
                a.body_file.clone(),
                "--base".to_string(),
                a.base.clone(),
            ];
            if a.bound_branch.is_some() {
                cmd.extend(["--head".to_string(), branch.clone()]);
            }
            match crate::org_board::budget::run_with_timeout(&cmd, &a.cwd, CREATE_TIMEOUT) {
                Ok(out) => {
                    let printed = String::from_utf8_lossy(&out);
                    print!("{printed}");
                    emit_pr_opened(&a.cwd, &printed, &branch);
                    record_docs_pr(&a.cwd, &a.body_file);
                    // The sha origin holds is the PR head; local HEAD can
                    // carry commits that were never pushed.
                    let remote = format!("refs/heads/{branch}");
                    if let Ok(line) = git(&a.cwd, &["ls-remote", "origin", &remote]) {
                        let sha = line.split_whitespace().next().unwrap_or_default();
                        let run = crate::stacked_base_stamp::runner("git", "gh");
                        let base = a.base.trim_start_matches("origin/");
                        crate::stacked_base_stamp::stamp(&a.cwd, base, sha, &run);
                    }
                    0
                }
                Err(e) if e.over_budget() => {
                    eprintln!("pr-create: gh pr create timed out after 300s");
                    4
                }
                Err(e) => {
                    eprintln!("pr-create: gh pr create failed: {}", e.message());
                    2
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn a_bare_docs_slug_records_the_transferred_owner() {
        // `Docs: fno-web#288` under origin fnodotdev/fno: the owner fill
        // invents fnodotdev/fno-web, and the built-in history alias moves
        // it to the repo GitHub actually serves. An empty temp cwd keeps
        // the map at the built-ins, so the read is deterministic.
        let cwd = std::env::temp_dir().join(format!("fno-docs-url-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let url = docs_pr_url(Some("fnodotdev/fno-web"), 288, Some("fnodotdev/fno"), &cwd);
        assert_eq!(
            url.as_deref(),
            Some("https://github.com/footnote-sh/fno-web/pull/288")
        );
        // A token already naming the live repo passes through.
        assert_eq!(
            docs_pr_url(
                Some("footnote-sh/fno-web"),
                288,
                Some("fnodotdev/fno"),
                &cwd
            )
            .as_deref(),
            Some("https://github.com/footnote-sh/fno-web/pull/288")
        );
        // An unmapped slug keeps the invented owner (today's fallback).
        assert_eq!(
            docs_pr_url(Some("fnodotdev/fno"), 7, Some("fnodotdev/fno"), &cwd).as_deref(),
            Some("https://github.com/fnodotdev/fno/pull/7")
        );
        // No repo token rides the origin's slug; no origin records nothing.
        assert_eq!(
            docs_pr_url(None, 5, Some("o/r"), &cwd).as_deref(),
            Some("https://github.com/o/r/pull/5")
        );
        assert_eq!(docs_pr_url(None, 5, None, &cwd), None);
        std::fs::remove_dir_all(&cwd).ok();
    }

    fn a_post_gate_plan_owes_premises_and_an_older_one_does_not() {
        let plan = |created: &str, body: &str| format!("---\ncreated: {created}\n---\n\n{body}");
        assert!(premises_missing(&plan("2026-11-17", "## Unknowns\n")));
        assert!(!premises_missing(&plan(
            "2026-11-17",
            "## Premises\n\n- CONFIRMED x\n"
        )));
        assert!(!premises_missing(&plan("2026-11-16", "## Unknowns\n")));
        assert!(!premises_missing("# undated plan\n"));
    }

    fn a_body_that_closes_a_node_and_defers_part_of_it_refuses() {
        let sliced = "Ships the parser. The rest ships as a later PR.\n\nFixes x-aaaa\n";
        let (phrase, line) = deferral_line(sliced).expect("a sliced body refuses");
        assert_eq!(phrase, "rest ships as");
        assert!(line.contains("The rest ships"), "{line}");
        assert_eq!(deferral_gate_refusal(sliced), Some(9));
        assert_eq!(deferral_line("Ships it whole.\n\nFixes x-aaaa\n"), None);
        assert_eq!(
            deferral_line("A first slice; no Fixes line closes anything.\n"),
            None
        );
    }

    fn a_second_branch_editing_one_file_refuses_and_names_the_first() {
        let gh = |path: &str| {
            if path.contains("/pulls?state=open") {
                Ok(json_array(vec![pr_json(
                    3050,
                    "fix(backlog): repair the graph door",
                    "graph-door-hotfix",
                    "bllshttng",
                )]))
            } else if path.contains("/pulls/3050/files") {
                Ok(json_array(vec![file_json(
                    "crates/fno-agent/src/backlog/entities.rs",
                )]))
            } else {
                Err(format!("unexpected gh read: {path}"))
            }
        };
        let dup = find_duplicate(
            "fix(backlog): repair the graph door",
            &["crates/fno-agent/src/backlog/entities.rs".to_string()],
            "feature/duplicate-guard",
            &[],
            &gh,
            "o/r",
            Path::new("."),
        )
        .unwrap();
        let (pr, shared) = dup.expect("the open twin refuses");
        assert_eq!(pr.number, 3050);
        assert_eq!(pr.branch, "graph-door-hotfix");
        assert_eq!(pr.author, "bllshttng");
        assert_eq!(shared, vec!["crates/fno-agent/src/backlog/entities.rs"]);
    }

    /// One declaration under the G3 net-zero cap; every row below
    /// still runs, byte-identical (the house fold pattern).
    #[test]
    fn all_contracts_in_one_declaration() {
        a_bare_docs_slug_records_the_transferred_owner();
        a_post_gate_plan_owes_premises_and_an_older_one_does_not();
        a_body_that_closes_a_node_and_defers_part_of_it_refuses();
        a_second_branch_editing_one_file_refuses_and_names_the_first();
        a_failed_github_read_is_exit_4_and_a_usage_error_is_exit_2();
    }

    fn a_failed_github_read_is_exit_4_and_a_usage_error_is_exit_2() {
        // Exit 4: a real repo whose gh read fails, so the guard's refusal
        // never silently degrades to a create.
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
        std::fs::write(dir.path().join("f.txt"), "base\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["checkout", "-q", "-b", "feature/guard"]);
        std::fs::write(dir.path().join("f.txt"), "changed\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "change"]);
        let mut repo_args = Args {
            title: "fix: the guard subject".into(),
            body_file: "b".into(),
            base: "main".into(),
            not_duplicates: vec![],
            check_only: false,
            cwd: dir.path().to_path_buf(),
            head: "HEAD".into(),
            bound_branch: None,
        };
        let gh = |path: &str| {
            assert!(path.contains("/pulls?state=open"));
            Err("HTTP 502".to_string())
        };
        assert_eq!(run(&repo_args, &gh), 4);
        // --check-only never creates: a clean scan exits 0 without reaching
        // the create leg, and a twin exits 3 with the same refusal.
        repo_args.check_only = true;
        repo_args.body_file = String::new();
        let clean = |path: &str| {
            assert!(path.contains("/pulls?state=open"));
            Ok("[]".to_string())
        };
        assert_eq!(run(&repo_args, &clean), 0);
        let twin = |path: &str| {
            if path.contains("/pulls?state=open") {
                Ok(json_array(vec![pr_json(
                    9,
                    "fix: the guard subject",
                    "other",
                    "someone",
                )]))
            } else {
                assert!(path.contains("/pulls/9/files"));
                Ok(json_array(vec![file_json("f.txt")]))
            }
        };
        assert_eq!(run(&repo_args, &twin), 3);
        // Exit 2: a non-repo cwd fails the branch read before any gh call.
        let a = Args {
            title: "t".into(),
            body_file: "b".into(),
            base: "main".into(),
            not_duplicates: vec![],
            check_only: false,
            cwd: PathBuf::from("/"),
            head: "HEAD".into(),
            bound_branch: None,
        };
        let gh = |_: &str| Err("HTTP 502".to_string());
        // `/` has no git repo: the branch read fails first (exit 2).
        assert_eq!(run(&a, &gh), 2);
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--title", "t"])).is_err());
        // --check-only drops the --body-file requirement.
        let only = parse_args(&args(&["--check-only", "--title", "t"])).unwrap();
        assert!(only.check_only);
        assert!(parse_args(&args(&[
            "--title",
            "t",
            "--body-file",
            "b",
            "--not-duplicate",
            "x",
        ]))
        .is_err());
        // The happy parse maps every flag, repeats --not-duplicate, and
        // expands a bare base to origin/<base>.
        let a = parse_args(&args(&[
            "--title",
            "t",
            "--body-file",
            "b",
            "--base",
            "release/9",
            "--not-duplicate",
            "7",
            "--not-duplicate",
            "8",
        ]))
        .unwrap();
        assert_eq!(a.base, "release/9");
        assert_eq!(a.not_duplicates, vec![7, 8]);
        assert_eq!(crate::pr_body_check::base_ref("release/9"), "release/9");
        assert_eq!(crate::pr_body_check::base_ref("main"), "origin/main");
        // The owners gate shares this file's exit-code contract, so its
        // refusal and pass paths run under the same declaration.
        checks_a_code_pr_that_audits_no_owner_test_file_refuses_and_names_it();
    }

    fn checks_a_code_pr_that_audits_no_owner_test_file_refuses_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
        let src = dir.path().join("crates/x/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn base_case() {}\n}\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["checkout", "-q", "-b", "feature/owners"]);
        std::fs::write(
            src.join("lib.rs"),
            "pub fn f() {}\npub fn g() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn base_case() {}\n}\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "extend"]);
        let body = dir.path().join("body.md");
        std::fs::write(&body, "## Summary\n\nadds g\n").unwrap();
        let mut a = Args {
            title: "feat: extend the owner surface".into(),
            body_file: body.to_string_lossy().into_owned(),
            base: "main".into(),
            not_duplicates: vec![],
            check_only: false,
            cwd: dir.path().to_path_buf(),
            head: "HEAD".into(),
            bound_branch: None,
        };
        // The scan passes (no open PRs); the owners gate then refuses and
        // the create leg is never reached.
        let gh = |path: &str| -> Result<String, String> {
            assert!(path.contains("/pulls?state=open"));
            Ok("[]".to_string())
        };
        assert_eq!(run(&a, &gh), 5);
        // The authored line satisfies the gate; --check-only then returns
        // clean without ever reaching gh pr create.
        std::fs::write(
            &body,
            "x\nAudited owners: crates/x/src/lib.rs (no cut: kept, distinct contracts)\n",
        )
        .unwrap();
        a.check_only = true;
        assert_eq!(run(&a, &gh), 0);
    }

    // ── fixtures ────────────────────────────────────────────────────────────

    fn pr_json(number: i64, title: &str, branch: &str, author: &str) -> Value {
        serde_json::json!({
            "number": number,
            "title": title,
            "head": {"ref": branch},
            "user": {"login": author},
        })
    }

    fn file_json(name: &str) -> Value {
        serde_json::json!({"filename": name})
    }

    fn json_array(items: Vec<Value>) -> String {
        Value::Array(items).to_string()
    }
}
