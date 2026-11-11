//! The merge gates that used to live in the Python merge command, ported so
//! `authorized_merge::decide` is the one merge decision. Each gate
//! evaluates one input and answers `Option<Blocker>`: None clears. The pure
//! halves take their facts as arguments so unit tests need no filesystem or
//! network; the fetching halves ride the caller's `Probes` handle.

use crate::authorized_merge::{
    node_carries_tag, pr_bound_entry, Blocker, PrFacts, ProbeOutcome, Probes, MAIN_REPAIR_TAG,
};
use crate::backlog::row_facts::RowFacts;
use crate::loopcheck::coverage_gate::{refusal_line, GateState};
use crate::visual_screen;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Only the external reads a gate needs; merge authority remains with its caller.
pub trait GateReads {
    fn gh_read(&self, cwd: &Path, args: &[String]) -> Result<(bool, String), String>;
    fn fno_read(
        &self,
        cwd: &Path,
        args: &[String],
    ) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String>;
}

impl<T: Probes> GateReads for T {
    fn gh_read(&self, cwd: &Path, args: &[String]) -> Result<(bool, String), String> {
        Probes::run_gh(self, cwd, args)
    }
    fn fno_read(
        &self,
        cwd: &Path,
        args: &[String],
    ) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
        Probes::fno_shell(self, cwd, args)
    }
}

pub(crate) struct ScopedReads {
    repo: Option<String>,
}

impl ScopedReads {
    pub(crate) fn for_checkout(cwd: &Path) -> Self {
        Self {
            repo: crate::finalize::repo_identity_from_git_remote(cwd)
                .and_then(|identity| identity.strip_prefix("github.com/").map(str::to_string)),
        }
    }

    pub(crate) fn program(
        &self,
        program: &std::ffi::OsStr,
        args: &[&str],
        cwd: &Path,
        timeout: std::time::Duration,
    ) -> Result<crate::loopcheck::BoundedOutput, String> {
        let repo = self
            .repo
            .as_deref()
            .ok_or("read repository is unresolved")?;
        let env = [("GH_REPO", repo), ("GH_HOST", "github.com")];
        let read = if Path::new(program).file_name() == Some(std::ffi::OsStr::new("gh")) {
            crate::loopcheck::bounded_read_env
        } else {
            crate::loopcheck::bounded_read_env_unadmitted
        };
        read(program, args, cwd, "scoped merge read", timeout, &env)
            .map_err(|error| crate::loopcheck::bounded_read_diagnostic("scoped merge read", &error))
    }

    pub(crate) fn info(&self, cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String> {
        let repo = self
            .repo
            .as_deref()
            .ok_or("read repository is unresolved")?;
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let client = crate::merge_command::hop_client_bin(&exe)
            .ok_or("candidate reader binary is unavailable")?;
        let payload =
            serde_json::json!({"op":"status-pr-info", "cwd":cwd, "repo":repo, "pr":pr}).to_string();
        let out = self.program(
            client.as_os_str(),
            &["authorized-merge", &payload],
            cwd,
            std::time::Duration::from_secs(crate::agents_config::merge_gh_read_secs(cwd)),
        )?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr_tail).into_owned());
        }
        let info: Value = serde_json::from_slice(&out.stdout).map_err(|error| error.to_string())?;
        if let Some(error) = info.get("error").and_then(Value::as_str) {
            return Err(error.into());
        }
        crate::authorized_merge::parse_pr_facts(&info)
    }
}

impl GateReads for ScopedReads {
    fn gh_read(&self, cwd: &Path, args: &[String]) -> Result<(bool, String), String> {
        let repo = self
            .repo
            .as_deref()
            .ok_or("read repository is unresolved")?;
        let mut args: Vec<_> = args
            .iter()
            .map(|arg| arg.replace("{owner}/{repo}", repo))
            .collect();
        if args.first().map(String::as_str) == Some("api") {
            args.extend(["--hostname".into(), "github.com".into()]);
        } else {
            args.extend(["--repo".into(), repo.into()]);
        }
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        let out = self.program(
            "gh".as_ref(),
            &refs,
            cwd,
            std::time::Duration::from_secs(crate::agents_config::merge_gh_read_secs(cwd)),
        )?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            text.push_str(&String::from_utf8_lossy(&out.stderr_tail));
        }
        Ok((out.status.success(), text))
    }

    fn fno_read(
        &self,
        cwd: &Path,
        args: &[String],
    ) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
        let repo = self
            .repo
            .as_deref()
            .ok_or("read repository is unresolved")?;
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        crate::authorized_merge::RealProbes::fno_env(
            cwd,
            &refs,
            &[("GH_REPO", repo), ("GH_HOST", "github.com")],
        )
    }
}

/// The coverage gate, polarity kept. The native verdict is the one
/// computation: it applies the operator waiver overlay inside itself, so a
/// waived PR reads Covered with the `override: ` note, and the waiver
/// travels with the gate's answer instead of a caller re-deriving it.
/// Returns (blocker, waiver).
pub(crate) fn coverage_gate<P: Probes>(
    probes: &P,
    cwd: &Path,
    pr: u64,
) -> (Option<Blocker>, Option<String>) {
    let v = probes.coverage_verdict(cwd, pr);
    match v.state {
        GateState::Covered => {
            // The prefix literal matches Python's OVERRIDE_NOTE_PREFIX.
            (None, v.note.strip_prefix("override: ").map(str::to_string))
        }
        GateState::Refused => {
            let line = refusal_line(&v.refusal, &v.note);
            // Advisory evidence, never a refusal: both walks demote this
            // blocker to a warning (law d-3173 - process conditions never
            // stop a merge; the architect pass is the guard), so the detail
            // must not claim a refusal that never happened.
            (
                Some(Blocker::held(
                    "review_coverage_uncovered",
                    if line.is_empty() {
                        "no review coverage at this head".to_string()
                    } else {
                        format!("no review coverage at this head: {line}")
                    },
                )),
                None,
            )
        }
        GateState::Unanswered => (
            Some(Blocker::unknown(
                "review_coverage_unknown",
                if v.note.is_empty() {
                    "coverage probe failed".to_string()
                } else {
                    format!("coverage probe failed: {}", v.note)
                },
            )),
            None,
        ),
    }
}

/// The posted-review gate. Fires only when an attestation already exists on
/// `facts.head_ref` (the no-attestation gap is `coverage_gate`'s job, via
/// `review_coverage_uncovered` - the two blockers never name the same gap)
/// but no GitHub review yet carries that round's marker. Reads the newest
/// BRANCH attestation, not an exact-head one: a round law d-608344c1 still
/// carries forward after a rebase (`review_freshness::Freshness::counts()`)
/// never gets a fresh attestation event at the new head, so an exact-head
/// lookup would go silent on exactly the rounds this gate exists to catch.
/// `publish_review::publish_round` resolves the SAME newest-branch round
/// when it posts, so the two stay pinned to one definition of "the round
/// that needs posting". `Held`, never `Refused`: `fno do pr publish-review`
/// already runs automatically at ship-create time and at every `finalize`
/// terminal decision (`crates/fno-agents/src/gate/finalize.rs`), so this normally
/// self-clears with no operator action - it never re-derives what counts as
/// "reviewed" (law d-608344c1 owns that), it only checks that a round the
/// ledger already counted got PUBLISHED.
pub(crate) fn review_posted_gate<P: Probes>(
    probes: &P,
    cwd: &Path,
    facts: &PrFacts,
) -> Option<Blocker> {
    let journals = [crate::paths::events_path(cwd)];
    let Some(attestation) =
        crate::publish_review::newest_branch_attestation(&journals, &facts.head_ref)
    else {
        return None;
    };
    let reviewed_head = attestation
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or(facts.head_sha.as_str());
    match probes.review_posted(cwd, facts, reviewed_head) {
        ProbeOutcome::Clear => None,
        ProbeOutcome::Refused(reason) => Some(Blocker::held("review_not_posted", reason)),
        ProbeOutcome::Inconclusive(reason) => {
            Some(Blocker::unknown("review_posted_unknown", reason))
        }
    }
}

/// `<root>/.fno/stub-manifest-<node>.json` (stub_manifest.py's own layout).
fn stub_manifest_path(root: &Path, node_id: &str) -> PathBuf {
    root.join(".fno")
        .join(format!("stub-manifest-{node_id}.json"))
}

/// The node-side half of the stub-manifest gate, pure over graph entries: the
/// `dep=contract` node this PR closes, or None. PR numbers are per-repo and
/// the graph store is global, so a ref whose pr_url names another repo never
/// answers (the shared-number specimen the ledger leg fixed); a ref with no
/// url, or one naming this checkout's remote, does. The graph read degrades
/// to None (the default hard merge path), exactly as the Python `dep` did.
pub(crate) fn contract_node_for_pr(entries: &[Value], pr: u64, slug: &str) -> Option<String> {
    for entry in entries {
        let dep = entry.get("dep").and_then(Value::as_str);
        if dep != Some("contract") {
            continue;
        }
        let answers = pr_refs_of(entry).iter().any(|r| {
            r.number == pr
                && match r.url.as_deref() {
                    None => true,
                    Some(url) => pr_url_names_this_repo(url, slug),
                }
        });
        if answers {
            return entry.get("id").and_then(Value::as_str).map(str::to_owned);
        }
    }
    None
}

/// One PR a graph row records, with the URL it names when it names one.
pub(crate) struct PrRef {
    pub(crate) number: u64,
    pub(crate) url: Option<String>,
    pub(crate) primary: bool,
}

/// Every PR ref a graph row carries: `pr_number` with its `pr_url`, then the
/// `additional_prs` entries (typed rows carry `{number}` or `{url}` objects;
/// legacy rows carry bare ints or `/pull/<n>` URL strings).
pub(crate) fn pr_refs_of(entry: &Value) -> Vec<PrRef> {
    let mut out = Vec::new();
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    if let Some(n) = entry.get("pr_number").and_then(Value::as_u64) {
        out.push(PrRef {
            number: n,
            url: text(entry.get("pr_url")),
            primary: true,
        });
    }
    if let Some(list) = entry.get("additional_prs").and_then(Value::as_array) {
        for raw in list {
            let (number, url) = match raw {
                Value::Number(n) => (n.as_u64(), None),
                Value::Object(o) => {
                    let url = text(o.get("url"));
                    let number = o.get("number").and_then(Value::as_u64);
                    (
                        number.or_else(|| url.as_deref().and_then(pull_number_from_url)),
                        url,
                    )
                }
                Value::String(url) => (pull_number_from_url(url), Some(url.clone())),
                _ => (None, None),
            };
            if let Some(number) = number {
                out.push(PrRef {
                    number,
                    url,
                    primary: false,
                });
            }
        }
    }
    out
}

/// The PR number a `/pull/<n>` URL names, if it names one.
fn pull_number_from_url(url: &str) -> Option<u64> {
    let rest = url.split("/pull/").nth(1)?;
    rest.split('/').next()?.parse::<u64>().ok()
}

/// The stub-manifest gate, pure over an already-read manifest value: a
/// contract dependent whose manifest is not `reconciled: true` holds the
/// merge (mocks would ship). An unreadable manifest of a known contract node
/// fails CLOSED, as the Python did.
pub(crate) fn stub_manifest_blocker(root: &Path, node_id: &str) -> Option<Blocker> {
    let path = stub_manifest_path(root, node_id);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(Blocker::held(
                "stub_manifest_unreconciled",
                format!(
                    "contract dependent {node_id} carries a malformed stub-manifest \
                     (cannot prove stubs are gone): {e}; reconcile before merge"
                ),
            ))
        }
    };
    let manifest: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(e) => {
            return Some(Blocker::held(
                "stub_manifest_unreconciled",
                format!(
                    "contract dependent {node_id} carries a malformed stub-manifest \
                     (cannot prove stubs are gone): {e}; reconcile before merge"
                ),
            ))
        }
    };
    if manifest.get("reconciled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let stubs = manifest
        .get("stubs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Some(Blocker::held(
        "stub_manifest_unreconciled",
        format!(
            "contract dependent {node_id} carries a unreconciled stub-manifest \
             ({stubs} stub(s)); reconcile before merge"
        ),
    ))
}

/// The whole stub-manifest gate over graph entries. An unreadable store
/// degrades to clear (never block a normal merge on our own read).
pub(crate) fn stub_manifest_gate(root: &Path, entries: &[Value], pr: u64) -> Option<Blocker> {
    let slug = origin_slug(root);
    contract_node_for_pr(entries, pr, &slug).and_then(|node| stub_manifest_blocker(root, &node))
}

/// Whether a path is documentation (loopcheck's own classifier; one copy).
fn is_documentation_path(path: &str) -> bool {
    crate::loopcheck::is_documentation_path(path)
}

/// Mechanical plan-fidelity evidence for the merge warning list. An
/// explicit shortfall is reported; unreadable probes add no invented finding.
pub(crate) fn plan_fidelity_blocker<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
) -> Option<Blocker> {
    let plan_path = ledger_plan_path(cwd, pr)?;
    let plan_path = plan_path.trim();
    if plan_path.is_empty() {
        return None;
    }
    if !pr_payload_is_code(probes, cwd, pr) {
        return None;
    }
    // The gate computes in process now (the `fno do plan fidelity` shell hop
    // is retired with its Python leg). Only an explicit shortfall is reported.
    let decision = crate::loopcheck::plan_fidelity::compute_plan_fidelity(plan_path, cwd, 180);
    if decision.get("refused").and_then(Value::as_bool) == Some(true) {
        let reason = decision
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("uncovered shortfall");
        Some(Blocker::refused(
            "plan_fidelity_refused",
            format!("plan fidelity refused: {reason}"),
        ))
    } else {
        None
    }
}

/// The plan_path bound to this PR's delivery row in the ledger, or None.
/// PR numbers are per-repo and the ledger is global, so when several rows
/// match, the one whose pr_url names this checkout's remote wins; a row
/// with no pr_url answers next. A row whose pr_url names another repo never
/// answers (the fnh/fno-web PR 70 specimen: another repo's plan refused a
/// merge here). A missing or broken ledger is None: no signal, no gate.
/// Whether a pr_url names this checkout's remote. GitHub spells the pair as
/// the path segment before `/pull/`, so the match is segment-tight: a
/// prefix-sibling repo (`org/fno` vs `org/fno-web`) never contains-matches.
fn pr_url_names_this_repo(url: &str, slug: &str) -> bool {
    !slug.is_empty() && url.contains(&format!("/{slug}/pull/"))
}

/// The `owner/name` slug this checkout's origin names, empty when
/// unresolvable. One shell so the ledger leg and the graph leg of the
/// merge-gate scoping cannot disagree about which remote answered.
fn origin_slug(cwd: &Path) -> String {
    use std::process::Command;

    let origin = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    // The pr_url names `owner/repo` inside its path; the remote url names the
    // same pair in ssh, https, and .git-suffixed spellings. Compare the
    // normalized pair, never the raw url: a substring test on the raw form
    // never matches and every scoping election falls to first-match.
    repo_slug_from_origin(&origin).unwrap_or_default()
}

fn ledger_plan_path(cwd: &Path, pr: u64) -> Option<String> {
    // Read-only: the ledger is global (the Python writer appends to the
    // same file), so an undeclared test process answers None, the same
    // no-signal shape as a missing ledger, instead of reading the
    // operator's history.
    if cfg!(test) && !crate::paths::test_root_declared() {
        return None;
    }
    let rows = crate::loopcheck::ledger_file_rows(&crate::paths_cli::ledger_json(cwd));
    if rows.is_empty() {
        return None;
    }
    let slug = origin_slug(cwd);
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|row| row.get("pr_number").and_then(Value::as_u64) == Some(pr))
        .collect();
    let picked = matches
        .iter()
        .copied()
        .find(|row| {
            row.get("pr_url")
                .and_then(Value::as_str)
                .is_some_and(|url| pr_url_names_this_repo(url, &slug))
        })
        .or_else(|| {
            // An unmarked row keeps the first-match fail-open shape; a row
            // that names another repo never does.
            matches.iter().copied().find(|row| {
                row.get("pr_url")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            })
        });
    picked.and_then(|row| {
        row.get("plan_path")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

/// The PR carries a code payload (at least one non-documentation changed
/// file). A read miss answers false, which skips the gate (fail-open: the
/// fidelity guard never wedges a merge because gh hiccuped).
fn pr_payload_is_code<P: GateReads>(probes: &P, cwd: &Path, pr: u64) -> bool {
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        pr.to_string(),
        "--json".to_string(),
        "files".to_string(),
    ];
    let Ok((true, stdout)) = probes.gh_read(cwd, &args) else {
        return false;
    };
    let Ok(payload) = serde_json::from_str::<Value>(&stdout) else {
        return false;
    };
    let Some(files) = payload.get("files").and_then(Value::as_array) else {
        return false;
    };
    files.iter().any(|file| {
        file.get("path")
            .or_else(|| file.get("filename"))
            .and_then(Value::as_str)
            .is_some_and(|path| !is_documentation_path(path))
    })
}

pub(crate) fn breadcrumb(message: &str) {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "pr-merge: {message}");
}

/// The overlap gate (parallel-mode G4, LD#9), polarity kept: only arms while
/// live lanes run; `_behind_by` miss disarms; base-move and PR-file misses
/// HOLD (fail closed) because the base already moved. Its remedy pulls main
/// into a branch GitHub calls mergeable, so it rides `require_fresh_ci`
/// (default off) with `ci_base_stale`: by default only a conflict pulls main.
pub(crate) fn overlap_blocker<P: Probes>(
    probes: &P,
    cwd: &Path,
    facts_number: u64,
) -> Option<Blocker> {
    if !probes.require_fresh_ci(cwd) || probes.live_lanes(cwd) == 0 {
        return None;
    }
    let behind = behind_by(probes, cwd, facts_number);
    if behind == 0 {
        return None;
    }
    // The probes already know the base moved, so a read that fails holds:
    // an under-reported move must never read as a clear. This is the Python
    // miss contract, kept verbatim.
    let base_paths = match base_move_paths(probes, cwd, facts_number) {
        Some(paths) => paths,
        None => {
            return Some(Blocker::held(
                "base_overlap",
                "stale base: overlap probe unavailable (base moved; could not \
                 compare file sets); run fno do pr push, then retry",
            ));
        }
    };
    let pr_paths = match pr_file_paths(probes, cwd, facts_number) {
        Some(paths) => paths,
        None => {
            return Some(Blocker::held(
                "base_overlap",
                "stale base: overlap probe unavailable (the PR's own file \
                 read failed); run fno do pr push, then retry",
            ));
        }
    };
    let overlap = overlaps(&base_paths, &pr_paths);
    if overlap.is_empty() {
        return None;
    }
    let shown = overlap
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let extra = if overlap.len() > 3 {
        format!(" and {} more", overlap.len() - 3)
    } else {
        String::new()
    };
    Some(Blocker::held(
        "base_overlap",
        format!(
            "stale base: base move touches files this PR also changes ({shown}{extra}); \
             run fno do pr push, then retry"
        ),
    ))
}

/// (base, head) ref names for a PR, or None on any read miss.
pub(crate) fn pr_base_head_refs<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
) -> Option<(String, String)> {
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        pr.to_string(),
        "--json".to_string(),
        "baseRefName,headRefName".to_string(),
    ];
    let (ok, stdout) = probes.gh_read(cwd, &args).ok()?;
    if !ok {
        return None;
    }
    let refs: Value = serde_json::from_str(&stdout).ok()?;
    let base = refs.get("baseRefName").and_then(Value::as_str)?;
    let head = refs.get("headRefName").and_then(Value::as_str)?;
    if base.is_empty() || head.is_empty() {
        return None;
    }
    Some((base.to_string(), head.to_string()))
}

/// Commits the PR head is behind its base. 0 on any probe miss (never block
/// a merge because our own read failed).
fn behind_by<P: GateReads>(probes: &P, cwd: &Path, pr: u64) -> u64 {
    let Some((base, head)) = pr_base_head_refs(probes, cwd, pr) else {
        breadcrumb(
            "stale-base probe unavailable (pr refs unreadable); merging without freshness hold",
        );
        return 0;
    };
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/compare/{base}...{head}"),
        "-q".to_string(),
        ".behind_by".to_string(),
    ];
    match probes.gh_read(cwd, &args) {
        Ok((true, stdout)) => stdout.trim().parse::<u64>().unwrap_or_else(|_| {
            breadcrumb(
                "stale-base probe unavailable (gh compare failed); merging without freshness hold",
            );
            0
        }),
        _ => {
            breadcrumb(
                "stale-base probe unavailable (gh compare failed); merging without freshness hold",
            );
            0
        }
    }
}

/// Files the BASE branch gained since the PR head diverged, or None (HOLD).
/// Truncation is a miss: an under-reported move fails in the merging direction.
fn base_move_paths<P: GateReads>(probes: &P, cwd: &Path, pr: u64) -> Option<Vec<String>> {
    let Some((base, head)) = pr_base_head_refs(probes, cwd, pr) else {
        breadcrumb(
            "overlap probe unavailable (pr refs unreadable); reporting base freshness evidence",
        );
        return None;
    };
    base_move_paths_between(probes, cwd, &base, &head)
}

/// [`base_move_paths`] for a caller that already holds the base ref and head.
pub(crate) fn base_move_paths_between<P: GateReads>(
    probes: &P,
    cwd: &Path,
    base: &str,
    head: &str,
) -> Option<Vec<String>> {
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/compare/{head}...{base}"),
        "--jq".to_string(),
        "{truncated: .truncated, names: [.files[] | ((.filename // empty), (.previous_filename // empty))]}"
            .to_string(),
    ];
    let (ok, stdout) = probes.gh_read(cwd, &args).ok()?;
    if !ok {
        breadcrumb("overlap probe unavailable (gh reverse compare failed); reporting base freshness evidence");
        return None;
    }
    let payload: Value = serde_json::from_str(&stdout).ok()?;
    let names = payload.get("names").and_then(Value::as_array)?;
    let mut paths = Vec::with_capacity(names.len());
    for name in names {
        match name.as_str() {
            Some(path) if !path.is_empty() => paths.push(path.to_string()),
            _ => {
                breadcrumb("overlap probe unavailable (compare file list carries non-string entries); reporting base freshness evidence");
                return None;
            }
        }
    }
    if payload
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || paths.len() >= 300
    {
        breadcrumb(&format!(
            "overlap probe unavailable (compare truncated, {} files; caps at 300); reporting base freshness evidence",
            paths.len()
        ));
        return None;
    }
    Some(paths)
}

/// The PR's own changed file paths, the old name of a rename included, or
/// None (HOLD). An EMPTY list is a real answer: a PR with no diff cannot
/// overlap anything.
pub(crate) fn pr_file_paths<P: GateReads>(probes: &P, cwd: &Path, pr: u64) -> Option<Vec<String>> {
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/pulls/{pr}/files"),
        "--paginate".to_string(),
        "--jq".to_string(),
        ".[] | (.filename // empty), (.previous_filename // empty)".to_string(),
    ];
    let (ok, stdout) = probes.gh_read(cwd, &args).ok()?;
    if !ok {
        breadcrumb("overlap probe unavailable (gh pr files read failed); reporting base freshness evidence");
        return None;
    }
    Some(
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Sorted intersection of two changed-file lists, documentation paths dropped
/// from both sides first. Empty: no semantic conflict the merge could carry.
fn overlaps(base_paths: &[String], pr_paths: &[String]) -> Vec<String> {
    let base: std::collections::BTreeSet<&str> = base_paths
        .iter()
        .map(String::as_str)
        .filter(|p| !is_documentation_path(p))
        .collect();
    let pr: std::collections::BTreeSet<&str> = pr_paths
        .iter()
        .map(String::as_str)
        .filter(|p| !is_documentation_path(p))
        .collect();
    base.intersection(&pr).map(|p| (*p).to_string()).collect()
}

/// True when every changed line of a Rust file's patch is a `//` comment or
/// blank. Any other file type, or an empty patch (binary, oversized), reads
/// false: only a positive reading releases the hold. A line inside a
/// multi-line string literal that starts with `//` would read as a comment;
/// no paint file holds one.
fn patch_is_comment_only(file: &str, patch: &str) -> bool {
    if !file.ends_with(".rs") || patch.is_empty() {
        return false;
    }
    let mut changed = false;
    for line in patch.lines() {
        let Some(body) = line
            .strip_prefix('+')
            .or_else(|| line.strip_prefix('-'))
            .filter(|_| !line.starts_with("+++") && !line.starts_with("---"))
        else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        if !body.starts_with("//") {
            return false;
        }
        changed = true;
    }
    changed
}

/// Whether every touched paint file changed by comment lines only. A rename
/// or an unreadable patch list reads false, so the hold stays.
fn comment_only_on<P: GateReads>(probes: &P, cwd: &Path, pr: u64, touched: &[&str]) -> bool {
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/pulls/{pr}/files"),
        "--paginate".to_string(),
        "--jq".to_string(),
        ".[] | {f: .filename, s: .status, p: (.patch // \"\")} | @json".to_string(),
    ];
    let Ok((true, stdout)) = probes.gh_read(cwd, &args) else {
        return false;
    };
    let rows: Vec<Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    touched.iter().all(|path| {
        rows.iter().any(|row| {
            row["f"].as_str() == Some(path)
                && row["s"].as_str() == Some("modified")
                && patch_is_comment_only(path, row["p"].as_str().unwrap_or(""))
        })
    })
}

/// Visual evidence for this head. Missing looks warn; an explicit user
/// decline holds only its named head. Paint paths select optional evidence,
/// never erase an already recorded user decline.
pub(crate) fn visual_approval_blocker<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
) -> Option<Blocker> {
    visual_approval_verdict(probes, cwd, pr, head).0
}

/// The gate read with the carry's warning: `(hold, warning)`. The merge
/// paths take the pair so an unprovable carry warns without holding.
pub(crate) fn visual_approval_verdict<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
) -> (Option<Blocker>, Option<crate::authorized_merge::Warning>) {
    let root = vault_root(cwd);
    let declines: Vec<String> = declining_pages(&root, pr)
        .into_iter()
        .chain(law_decline_texts(probes, cwd, pr))
        .collect();
    if declines
        .iter()
        .any(|text| visual_decline_at_head(text, pr, head))
    {
        return (
            Some(Blocker::held(
                "visual_declined",
                format!(
                    "the user declined PR {pr} at {head}; this authority hold binds only that head"
                ),
            )),
            None,
        );
    }
    let paint_paths = crate::agents_config::visual_paint_paths(cwd);
    if paint_paths.is_empty() {
        return (None, None);
    }
    let Some(files) = pr_file_paths(probes, cwd, pr) else {
        // An unreadable file list is evidence, not a user decline.
        return (
            Some(Blocker::unknown(
                "visual_paint_paths_unknown",
                format!(
                    "PR {pr}: the changed-file list was unreadable, so the visual-approval gate cannot answer"
                ),
            )),
            None,
        );
    };
    let touched: Vec<&str> = files
        .iter()
        .filter(|f| path_matches_paint(f, &paint_paths))
        .map(String::as_str)
        .collect();
    if touched.is_empty() {
        return (None, None);
    }
    // Comments and code names cannot move a pixel, so a look would judge
    // nothing. The code diff alone clears the gate: no shots, no ruling, no
    // question. An unreadable patch leaves warning evidence.
    if comment_only_on(probes, cwd, pr, &touched)
        || touched.iter().all(|f| {
            paint_patch(probes, cwd, pr, f).is_some_and(|p| visual_screen::names_only_patch(&p))
        })
    {
        return (None, None);
    }
    // Ruling: a declared main-repair identifier IS the red-main fix; its lane
    // already merges through red, so the look holds nothing. Fail toward
    // holding: only a positive tag on the bound node exempts.
    let entry = pr_bound_entry(cwd, pr);
    if entry
        .as_ref()
        .is_some_and(|e| node_carries_tag(e, MAIN_REPAIR_TAG))
    {
        return (None, None);
    }
    let node_id = entry.as_ref().and_then(RowFacts::id_of);
    let exact_head_clear = answered_question_names_pr(&root, pr, head)
        || node_id
            .as_deref()
            .is_some_and(|node| role_chat_clears_pr(probes, cwd, pr, head, node))
        || law_chat_clears_pr(probes, cwd, pr, head);
    if exact_head_clear {
        return (None, None);
    }
    let mut declined_by = None;
    let mut carry_ask = None;
    if !open_look_names_head(&root, pr, head) {
        match approval_carries(probes, cwd, pr, head, node_id.as_deref(), &paint_paths) {
            Carry::Clear => return (None, None),
            Carry::Warn(reason) => {
                return (
                    None,
                    Some(crate::authorized_merge::Warning::new(
                        "visual_carry_unproven",
                        format!(
                            "PR {pr}: the look approval at an earlier head could not be proven \
                             at this head ({reason}); the carry warns and never holds"
                        ),
                    )),
                );
            }
            Carry::Declined(text) => declined_by = Some(text),
            Carry::Absent => {}
            Carry::Ask(ask) => carry_ask = Some(ask),
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shots = node_id
        .as_deref()
        .map(|n| mux_shots(&root, home.as_deref(), n))
        .unwrap_or_default();
    let mut detail = if let Some(ask) = &carry_ask {
        let hunks = ask
            .hunks
            .iter()
            .map(paint_hunk_summary)
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "PR {pr} touches the paint surface ({}); the look approval at {} does not carry: paint moved since ({hunks}); ask the user again at {head}; this is warning evidence and does not hold CI or merging",
            touched.join(", "),
            ask.approved.get(..8).unwrap_or(&ask.approved),
        )
    } else {
        format!(
            "PR {pr} touches the paint surface ({}), with no recorded look at {head}; this is warning evidence and does not hold CI or merging",
            touched.join(", ")
        )
    };
    // The re-ask page names which states the approved look's shots showed, so
    // the reader answers against what the look actually judged. A look with
    // no shot record names no states.
    if let (Some(node), Some(_)) = (node_id.as_deref(), carry_ask.as_ref()) {
        let mux = crate::escalation::vault_dir_with_home(&root, home.as_deref(), "mux");
        let named: Vec<String> = crate::frames::views_for_node(&mux, node)
            .into_iter()
            .filter(|v| !v.view.is_empty())
            .map(|v| v.key())
            .collect();
        if named.is_empty() {
            detail.push_str("; no shot record names the states the look showed");
        } else {
            detail.push_str(&format!(
                "; the approved look's shots show states: {}; states outside them are not shown by that look",
                named.join(", ")
            ));
        }
    }
    let prior: Vec<String> = declines
        .iter()
        .filter_map(|text| visual_decline_title(text))
        .filter(|title| page_names_pr(title, pr))
        .collect();
    if !prior.is_empty() {
        detail.push_str(&format!(
            "; earlier declined look evidence: {}",
            prior.join("; ")
        ));
    }
    if touched.iter().any(|f| is_public_copy(f)) {
        detail.push_str("; review public copy from the target reader's perspective");
    }
    match (node_id.as_deref(), shots.as_slice()) {
        (Some(_), []) => detail.push_str("; no screenshots are available"),
        (None, _) => detail.push_str("; no node is bound to this PR"),
        (Some(node), _) => {
            let mux = crate::escalation::vault_dir_with_home(&root, home.as_deref(), "mux");
            let doc = std::fs::read_to_string(root.join("docs/architecture/mux-design-system.md"))
                .unwrap_or_default();
            let headings = visual_screen::rule_headings(&doc);
            match {
                // The critique may live in the asking session's vault
                // project: a Ready one wins, else the own dir's verdict
                // names the remedy.
                let mut verdict = visual_screen::CritiqueRead::Missing;
                for d in shot_candidates(&mux) {
                    match visual_screen::read_critique(&d, node, head, &headings) {
                        r @ visual_screen::CritiqueRead::Ready(_) => {
                            verdict = r;
                            break;
                        }
                        visual_screen::CritiqueRead::Missing => {}
                        r => verdict = r,
                    }
                }
                verdict
            } {
                visual_screen::CritiqueRead::Missing => {
                    detail.push_str("; no design critique is available")
                }
                visual_screen::CritiqueRead::Unfilled(why) => {
                    detail.push_str(&format!("; design critique unavailable: {why}"));
                }
                visual_screen::CritiqueRead::High(high) => {
                    detail.push_str(&format!(
                        "; design critic reports {} high findings: {}",
                        high.len(),
                        high.join("; ")
                    ));
                }
                visual_screen::CritiqueRead::Ready(critique) => {
                    detail.push_str(&format!(
                        "; design critique: {}",
                        visual_screen::critique_summary(&critique)
                    ));
                }
            }
        }
    }
    if let Some(text) = declined_by {
        detail.push_str(&format!(
            "; a decline names a commit after the approved one, so the look was withdrawn: {text}"
        ));
    }
    (Some(Blocker::held("visual_approval", detail)), None)
}

fn visual_decline_title(text: &str) -> Option<String> {
    let (title, answer) = crate::attention_file::parse_page(text)
        .map(|(front, _)| {
            let title = if front.ask.is_empty() {
                front.title
            } else {
                front.ask
            };
            (title, front.answer.unwrap_or_default())
        })
        .unwrap_or_else(|| {
            let mut lines = text.lines();
            let title = lines.next().unwrap_or_default().to_string();
            let answer = lines
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .to_string();
            (title, answer)
        });
    let lower = title.trim().to_ascii_lowercase();
    let visual = lower.starts_with("visual approval pr")
        || lower.starts_with("approve the look")
        || (lower.starts_with("may pr") && lower.contains("merge as painted"));
    let merge = lower
        .strip_prefix("may pr")
        .or_else(|| lower.strip_prefix("can pr"))
        .is_some_and(|tail| {
            let action = tail.trim_start_matches(|c: char| {
                c.is_ascii_digit() || c.is_ascii_whitespace() || matches!(c, '#' | '-' | '/' | ':')
            });
            action == "merge"
                || action.starts_with("merge ")
                || action.starts_with("merge?")
                || action.starts_with("merge(")
        });
    ((visual || merge) && answer_declines(&answer, visual)).then_some(title)
}

fn answer_declines(answer: &str, numbered_hold: bool) -> bool {
    let answer = answer.trim().to_ascii_lowercase();
    let number: String = answer.chars().take_while(char::is_ascii_digit).collect();
    if !number.is_empty() && numbered_hold {
        return number == "2";
    }
    let first = answer
        .split(|c: char| !c.is_ascii_alphabetic())
        .find(|word| !word.is_empty())
        .unwrap_or_default();
    (first == "b" && numbered_hold)
        || matches!(
            first,
            "hold" | "no" | "decline" | "declined" | "reject" | "rejected"
        )
}

fn visual_decline_at_head(text: &str, pr: u64, head: &str) -> bool {
    visual_decline_title(text)
        .is_some_and(|title| page_names_pr(&title, pr) && text_names_head(&title, head))
}

/// The unified-diff patch GitHub holds for one changed file of the PR.
/// `None` when the read fails or GitHub sent no patch (binary, too large).
fn paint_patch<P: GateReads>(probes: &P, cwd: &Path, pr: u64, file: &str) -> Option<String> {
    let filter = format!(".[] | select(.filename == \"{file}\") | .patch // empty");
    let args: Vec<String> = [
        "api",
        &format!("repos/{{owner}}/{{repo}}/pulls/{pr}/files"),
        "--paginate",
        "--jq",
        &filter,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let (ok, out) = probes.gh_read(cwd, &args).ok()?;
    (ok && !out.trim().is_empty()).then_some(out)
}

/// The public front page a reader meets before any install: the README and
/// its images. Landing copy lives in the docs tree and takes the same path.
fn is_public_copy(path: &str) -> bool {
    path == "README.md" || path.starts_with("docs/images/")
}

/// How many earlier commits of the PR the carry walks, newest first.
const CARRY_WALK: usize = 30;

/// What the approval carry answers at a head.
pub(crate) enum Carry {
    /// No paint file changed since the approved commit: the look stands at this head.
    Clear,
    /// No approval record names this PR at any walked commit: first-look evidence.
    Absent,
    /// A decline names a commit after the old approved commit: the look was withdrawn.
    Declined(String),
    /// Paint moved since the approved commit: the look cannot carry silently.
    /// The payload names what moved so the re-ask page can list it.
    Ask(CarryAsk),
    /// The carry cannot prove its read (stale record, unreadable read): warn, never hold.
    Warn(String),
}

/// The re-ask payload: the approved commit the look named, and the paint
/// hunks that moved between it and the head.
pub(crate) struct CarryAsk {
    pub approved: String,
    pub hunks: Vec<PaintHunk>,
}

/// One paint file that moved, with its unified-diff hunk headers.
pub(crate) struct PaintHunk {
    pub file: String,
    pub headers: Vec<String>,
}

/// The user's look sticks to the PR until merge: a later commit moves the
/// head, and the approval holds while the paint paths the config lists are
/// untouched since the approved commit. The newest earlier commit of this PR
/// an approval text names is the reference, kept on the record for audit. A
/// decline after the old approved commit withdraws the look. Paint moved, the
/// carry re-asks, and the payload names what moved. Every evidence gap warns
/// and never holds: a stale record, an unreadable read.
fn approval_carries<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
    node: Option<&str>,
    paint_paths: &[String],
) -> Carry {
    let root = vault_root(cwd);
    // The approval texts answer first: a PR with no look record anywhere is
    // the first look, whatever the commit list
    // reads. Only an approval that exists but cannot be proven warns.
    let mut texts: Vec<String> = approving_pages(&root, pr)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    texts.extend(
        node.map(|n| role_approval_texts(probes, cwd, pr, n))
            .unwrap_or_default(),
    );
    texts.extend(law_approval_texts(probes, cwd, pr));
    if texts.is_empty() {
        return Carry::Absent;
    }
    let Some(commits) = gh_lines(
        probes,
        cwd,
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/pulls/{pr}/commits"),
            "--paginate",
            "--jq",
            ".[].sha",
        ],
    ) else {
        return Carry::Warn("the PR commit list was unreadable".to_string());
    };
    let Some(approved) = carry_reference(&commits, head, &texts) else {
        return Carry::Warn(
            "the approval names no commit in the PR's recent history, so the look record is stale"
                .to_string(),
        );
    };
    let mut declines = declining_pages(&root, pr);
    declines.extend(law_decline_texts(probes, cwd, pr));
    let newer: Vec<&str> = commits
        .iter()
        .skip_while(|sha| sha.as_str() != approved)
        .map(String::as_str)
        .collect();
    if declined_after(&declines, &newer) {
        let text = declines
            .iter()
            .find(|text| newer.iter().any(|sha| text_names_head(text, sha)))
            .cloned()
            .unwrap_or_else(|| "a decline names a commit after the approved one".to_string());
        return Carry::Declined(text);
    }
    // The paint diff is the whole carry read: the config defines what can
    // move pixels. No paint file changed between the approved commit and the
    // head, so the look stands. Paint moved, the look re-asks, and the
    // payload names what moved so the page can list it.
    let Some(hunks) = paint_delta(probes, cwd, approved, head, paint_paths) else {
        return Carry::Warn(
            "the paint diff between the approved commit and this head was unreadable".to_string(),
        );
    };
    if hunks.is_empty() {
        return Carry::Clear;
    }
    Carry::Ask(CarryAsk {
        approved: approved.to_string(),
        hunks,
    })
}

/// The newest earlier commit of the PR an approval text names: the carry's
/// reference. The texts a fleet writes spell the head short, so the match is
/// the same prefix matcher the exact-head doors use.
fn carry_reference<'a>(commits: &'a [String], head: &str, texts: &[String]) -> Option<&'a str> {
    commits
        .iter()
        .rev()
        .filter(|sha| sha.as_str() != head)
        .take(CARRY_WALK)
        .find(|sha| texts.iter().any(|text| text_names_head(text, sha)))
        .map(String::as_str)
}

/// The paint delta between two commits: per paint file, the hunk headers the
/// compare serves, from one compare read. `None` when the read fails.
fn paint_delta<P: GateReads>(
    probes: &P,
    cwd: &Path,
    base: &str,
    sha: &str,
    paint_paths: &[String],
) -> Option<Vec<PaintHunk>> {
    let lines = gh_lines(
        probes,
        cwd,
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/compare/{base}...{sha}"),
            "--jq",
            ".files[] | (\"PAINTFILE \" + .filename), ((.patch // \"\") | split(\"\\n\") | map(select(startswith(\"@@\"))) | .[])",
        ],
    )?;
    Some(parse_paint_markers(&lines, paint_paths))
}

/// Marker lines into per-file hunk records, paint-filtered. A paint file
/// whose patch body is unreadable still lists, so the page names it without
/// hunks.
fn parse_paint_markers(lines: &[String], paint_paths: &[String]) -> Vec<PaintHunk> {
    let mut out: Vec<PaintHunk> = Vec::new();
    for line in lines {
        if let Some(file) = line.strip_prefix("PAINTFILE ") {
            out.push(PaintHunk {
                file: file.to_string(),
                headers: Vec::new(),
            });
        } else if line.starts_with("@@") {
            if let Some(last) = out.last_mut() {
                last.headers.push(line.to_string());
            }
        }
    }
    out.retain(|h| path_matches_paint(&h.file, paint_paths));
    out
}

/// One line for the re-ask page: the file and its hunk headers, or a note
/// that the compare served no patch body for it.
fn paint_hunk_summary(h: &PaintHunk) -> String {
    if h.headers.is_empty() {
        format!("{} (no hunk bodies in the compare)", h.file)
    } else {
        format!("{} {}", h.file, h.headers.join(" "))
    }
}

/// Did the user decline the paint at a commit pushed after the approval?
/// `newer` holds the commits after the approved one. A decline page names
/// the head it judged.
fn declined_after(declines: &[String], newer: &[&str]) -> bool {
    declines
        .iter()
        .any(|text| newer.iter().any(|sha| text_names_head(text, sha)))
}

/// One `gh` read as its non-empty trimmed lines; `None` on a failed read.
fn gh_lines<P: GateReads>(probes: &P, cwd: &Path, args: &[&str]) -> Option<Vec<String>> {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let (ok, stdout) = probes.gh_read(cwd, &args).ok()?;
    ok.then(|| {
        stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

/// The repo root whose `.fno/config.toml` carries the project id: the vault
/// paths (`internal/<project>/...`) resolve wrong from a worktree, whose
/// checkout has no project id and falls to the git-remote slug.
fn vault_root(cwd: &Path) -> PathBuf {
    crate::paths::canonical_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// The mux captures for one node's PR: `internal/<project>/mux/<node>-*.png`
/// top level, plus files inside a `<node>-shots/` folder, as vault-relative
/// embed names. Sorted, so the embedded list is stable.
pub(crate) fn mux_shots(root: &Path, home: Option<&Path>, node: &str) -> Vec<String> {
    let dir = crate::escalation::vault_dir_with_home(root, home, "mux");
    let mut out = shots_in_dir(&dir, node);
    for s in sibling_mux_dirs(&dir) {
        out.extend(shots_in_dir(&s.join("mux"), node));
    }
    out.sort();
    out
}

/// The sibling vault projects' mux dirs for a mux dir whose project folder
/// sits under a vault `internal/` root: shots filed where the asking session
/// lived, found by node id, which is unique across the vault. Empty for a
/// mux dir outside that layout (the space fallback).
pub(crate) fn sibling_mux_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let internal = dir.parent().and_then(|p| p.parent());
    let Some(internal) = internal else {
        return Vec::new();
    };
    if internal.file_name().and_then(|n| n.to_str()) != Some("internal") {
        return Vec::new();
    }
    let own = dir.parent().unwrap_or(internal);
    let mut siblings: Vec<std::path::PathBuf> = std::fs::read_dir(internal)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && p != own)
                .collect()
        })
        .unwrap_or_default();
    siblings.sort();
    siblings
}

/// The mux dirs a critique or shot may live in: the own project's mux dir
/// first, then any sibling vault project's.
pub(crate) fn shot_candidates(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v = vec![dir.to_path_buf()];
    v.extend(sibling_mux_dirs(dir));
    v
}

/// Resolve a mux-relative shot or boxed-critique name to its file: the own
/// project's mux dir first, then any sibling vault project's. Falls back to
/// the own-dir join so a missing shot keeps its own path in error text.
pub(crate) fn resolve_shot(dir: &Path, name: &str) -> std::path::PathBuf {
    let direct = dir.join(name);
    if direct.exists() {
        return direct;
    }
    for s in sibling_mux_dirs(dir) {
        let p = s.join("mux").join(name);
        if p.exists() {
            return p;
        }
    }
    direct
}

/// The node-prefixed captures in one mux folder: top-level `<node>*.png`
/// plus files inside a `<node>-shots/` folder, as mux-relative embed
/// names, sorted.
fn shots_in_dir(dir: &Path, node: &str) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(node) && name.to_ascii_lowercase().ends_with(".png") {
            out.push(name);
        } else if name == format!("{node}-shots") && e.path().is_dir() {
            let Ok(sub) = std::fs::read_dir(e.path()) else {
                continue;
            };
            let mut sub_entries: Vec<_> = sub.flatten().collect();
            sub_entries.sort_by_key(|e| e.file_name());
            out.extend(sub_entries.iter().filter_map(|e| {
                let sub_name = e.file_name().to_string_lossy().to_string();
                sub_name
                    .to_ascii_lowercase()
                    .ends_with(".png")
                    .then(|| format!("{node}-shots/{sub_name}"))
            }));
        }
    }
    out
}

/// Does one changed file match the paint-path list? Three pattern shapes: a
/// `dir/**` subtree, a `/name` anchored at the repo root, and a bare file
/// name (any directory). `ponytail:` no glob engine; a mid-pattern `**` or
/// `*` wildcard is added when a config needs it.
pub(crate) fn path_matches_paint(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        let p = p.trim();
        if let Some(rooted) = p.strip_prefix('/') {
            return path == rooted;
        }
        if let Some(prefix) = p.strip_suffix("/**") {
            let prefix = prefix.trim_end_matches('/');
            return path.starts_with(prefix)
                && (path.len() == prefix.len() || path[prefix.len()..].starts_with('/'))
                && !is_test_path(&path[prefix.len()..]);
        }
        if !p.contains('/') {
            return path.rsplit('/').next() == Some(p);
        }
        path == p
    })
}

/// A test file inside a `dir/**` subtree moves no pixel the user sees, so a
/// test-only PR never waits on a look. Exact and bare-name patterns still
/// match as written.
fn is_test_path(rest: &str) -> bool {
    let name = rest.rsplit('/').next().unwrap_or(rest);
    rest.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

/// True when an ANSWERED question page in this project's questions directory
/// names the PR at this exact head and its answer picks the approve option:
/// the pages the lead check-in reads, parsed the same way. "Hold for
/// changes" or "none of these" is an answer too, and it must keep the hold.
/// Head-scoped like the role row: a push after the user looked holds again.
fn answered_question_names_pr(root: &Path, pr: u64, head: &str) -> bool {
    !head.is_empty()
        && approving_pages(root, pr)
            .iter()
            .any(|(_, text)| text_names_head(text, head))
}

/// The question dirs this repo's gate reads: the PR repo's own project
/// folder plus the attention arm's published one, each with its done/
/// archive when `with_done`. The daemon files every ask into ONE published
/// folder regardless of which repo's PR the question concerns, so an
/// approval can land outside the PR repo's own folder. A page from the own
/// folder is trusted as filed; a page from the published folder counts only
/// when its stamped `project` names this repo's project: PR numbers collide
/// across repos, so an unstamped or foreign-stamped page never clears this
/// gate. Each entry is `(dir, Option<required stamp>)`.
fn question_read_dirs(root: &Path, with_done: bool) -> Vec<(PathBuf, Option<String>)> {
    let own = crate::escalation::questions_dir(root);
    let project = own
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let mut out = vec![(own.clone(), None)];
    if with_done {
        out.push((own.join("done"), None));
    }
    if let Some(published) = crate::attention_arm::published_questions_dir() {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if canon(&published) != canon(&own) {
            out.push((published.clone(), project.clone()));
            if with_done {
                out.push((published.join("done"), project));
            }
        }
    }
    out
}

/// The page's stamped project clears the folder's stamp rule: `None` (the
/// own folder) accepts anything, `Some(project)` only a matching stamp.
fn stamp_allows(front_project: &str, stamp: &Option<String>) -> bool {
    stamp
        .as_ref()
        .map_or(true, |want| front_project.trim() == want)
}

/// Every ANSWERED question page that picks the approve option and names the
/// PR, as `(question id, page text)`, at any head.
fn approving_pages(root: &Path, pr: u64) -> Vec<(String, String)> {
    // Answered pages are archived into done/ after the fact; an approval
    // must not lapse because its page moved there.
    question_read_dirs(root, true)
        .iter()
        .flat_map(|(dir, stamp)| {
            crate::lead_answers::read_question_pages(dir)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(_stem, text)| {
                    let (front, _) = crate::attention_file::parse_page(&text)?;
                    let stamped_ok = stamp_allows(&front.project, stamp);
                    (front.status == "answered"
                        && stamped_ok
                        && front.answer.as_deref().is_some_and(answer_approves)
                        && page_names_pr(&text, pr))
                    .then_some((front.question_id, text))
                })
        })
        .collect()
}

/// True when an OPEN question page names the PR at this exact head: the user
/// has been asked to look at this commit and has not answered. A carried
/// approval of an earlier commit must not clear it.
fn open_look_names_head(root: &Path, pr: u64, head: &str) -> bool {
    question_read_dirs(root, false).iter().any(|(dir, stamp)| {
        let pages: Vec<String> = crate::lead_answers::read_question_pages(dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_stem, text)| {
                crate::attention_file::parse_page(text)
                    .is_some_and(|(front, _)| stamp_allows(&front.project, stamp))
            })
            .map(|(_stem, text)| text)
            .collect();
        open_page_names_head(pages.iter().map(String::as_str), pr, head)
    })
}

fn open_page_names_head<'a>(pages: impl Iterator<Item = &'a str>, pr: u64, head: &str) -> bool {
    let mut pages = pages;
    !head.is_empty()
        && pages.any(|text| {
            crate::attention_file::parse_page(text).is_some_and(|(front, _)| front.status == "open")
                && page_names_pr(text, pr)
                && text_names_head(text, head)
        })
}

/// Every ANSWERED question page that names the PR and does not pick the
/// approve option (hold for changes): the page text.
fn declining_pages(root: &Path, pr: u64) -> Vec<String> {
    question_read_dirs(root, true)
        .iter()
        .flat_map(|(dir, stamp)| {
            crate::lead_answers::read_question_pages(dir)
                .unwrap_or_default()
                .into_iter()
                .map(|(_stem, text)| text)
                .filter_map(|text| {
                    let (front, _) = crate::attention_file::parse_page(&text)?;
                    let stamped_ok = stamp_allows(&front.project, stamp);
                    (front.status == "answered"
                        && stamped_ok
                        && front.answer.as_deref().is_some_and(|a| !answer_approves(a))
                        && page_names_pr(&text, pr))
                    .then_some(text)
                })
        })
        .collect()
}

/// Does a recorded answer pick the approve option? A numbered answer
/// approves only as option 1 (the filed page's "Merge as painted"). A bare
/// answer approves when it opens with an approve word. Anything else holds.
/// The approve words an answer may open with: `Merge as painted`, the label
/// the ask templates generate, and the CLI's own ship word that a reworded
/// page label uses. Anything else reads as a decline in the carry path, so
/// the list must carry every approve label the pages actually produce.
fn answer_approves(answer: &str) -> bool {
    let a = answer.trim().to_ascii_lowercase();
    let digits: String = a.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        return digits == "1";
    }
    ["merge", "approve", "yes", "ship"]
        .iter()
        .any(|w| a.starts_with(w))
}

/// The page text names the PR: a word-start `pr` (any case), an optional
/// `#`/`-`/`/`/`:`/space separator run, then the PR number, not followed by
/// another digit. Prose like "the PR is 3058" or "PRs 3058" never matches.
pub(crate) fn page_names_pr(text: &str, pr: u64) -> bool {
    let needle = pr.to_string();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 2 <= bytes.len() {
        let is_pr =
            bytes[i].to_ascii_lowercase() == b'p' && bytes[i + 1].to_ascii_lowercase() == b'r';
        if !is_pr {
            i += 1;
            continue;
        }
        let word_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        i += 1;
        if !word_start {
            continue;
        }
        let mut j = i + 1; // first byte after "pr"
                           // One separator run, spaces and symbols in any order, so "PR #3036"
                           // and "pr - 3036" read like "PR 3036".
        while matches!(
            bytes.get(j),
            Some(b'#') | Some(b'-') | Some(b'/') | Some(b':') | Some(b' ')
        ) {
            j += 1;
        }
        let rest = &text[j.min(text.len())..];
        if rest.starts_with(&needle)
            && !rest[needle.len()..]
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// The row text the PR/head match reads: decision plus rationale, the two
/// free-text fields a recording carries.
fn row_text(row: &Value) -> String {
    format!(
        "{}\n{}",
        row.get("decision").and_then(Value::as_str).unwrap_or(""),
        row.get("rationale").and_then(Value::as_str).unwrap_or("")
    )
}

/// The rationale attests the approval came from the user's own chat words.
/// `superuser in chat` contains `user in chat`, so one casefold covers both
/// spellings the leads already record.
fn row_is_chat_attested(row: &Value) -> bool {
    rationale_lower(row).contains("user in chat")
}

/// The row's rationale, casefolded; empty when absent.
fn rationale_lower(row: &Value) -> String {
    row.get("rationale")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase()
}

/// The rationale attests the user tapped Approve on a Visual Log page. The
/// lead's reader writes this row only for a tap whose head still matches the
/// PR and whose viewer is one of the user's accounts.
fn row_is_tap_attested(row: &Value) -> bool {
    rationale_lower(row).contains(VISUAL_LOG_TAP)
}

/// The rationale phrase a Visual Log tap row carries.
const VISUAL_LOG_TAP: &str = "user tapped approve on the visual log";

/// The rationale attests the paint diff is identifier-only: the edits
/// rename or rebind code names and every rendered string stays
/// byte-identical, so no pixel moves and a look would judge nothing. The
/// shape matters where the ask door refuses a reversible question and no
/// shots exist to file a page with, which left a held paint PR no lane at
/// all. Both spellings a lead writes read; the ruling stays role-gated and
/// head-scoped like the chat attestation, so only the lead whose territory
/// covers the node can mint it and a push voids it.
fn row_attests_identifier_only(row: &Value) -> bool {
    let r = rationale_lower(row);
    r.contains("identifiers only") || r.contains("identifier-only")
}

/// The rationale cites one of these question ids as a whole token. The
/// user answered that page on the board, so the board is the channel and
/// the lead need not write a chat attestation that would be false.
fn row_cites_any(row: &Value, ids: &[String]) -> bool {
    let Some(r) = row.get("rationale").and_then(Value::as_str) else {
        return false;
    };
    let tokens: Vec<&str> = r
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .collect();
    ids.iter().any(|id| tokens.contains(&id.as_str()))
}

/// True when a live role decision row transcribes the user's approval of
/// this PR at this head: authority `role`, the PR's bound node as subject
/// (the decide door lets only the lead whose territory covers that node mint
/// it), a rationale that attests the user's chat words, cites an answered
/// question page that approves this PR, or attests the identifier-only paint
/// shape, and text naming both the PR and the
/// exact head sha the lead re-pinned. Head-scoped like the operator head grant: a
/// push invalidates the approval. An unreadable index never clears.
pub(crate) fn role_chat_clears_pr<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
    node: &str,
) -> bool {
    !head.is_empty()
        && role_approval_texts(probes, cwd, pr, node)
            .iter()
            .any(|text| text_names_head(text, head))
}

/// One `fno backlog decisions ... --json` read; None when the read fails.
fn decisions_payload<P: GateReads>(probes: &P, cwd: &Path, args: &[&str]) -> Option<Value> {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let Some((Some(0), out, _)) = probes.fno_read(cwd, &args).ok() else {
        return None;
    };
    serde_json::from_slice::<Value>(&out).ok()
}

/// Does `token` name `head`: the full sha, or a hex prefix of at least seven
/// characters, in any case. A question card abbreviates the head it asks
/// about, and the user's answer on the board inherits that spelling.
pub(crate) fn sha_names_head(token: &str, head: &str) -> bool {
    token.len() >= 7
        && token.len() <= head.len()
        && token.bytes().all(|b| b.is_ascii_hexdigit())
        && head.as_bytes()[..token.len()].eq_ignore_ascii_case(token.as_bytes())
}

/// The text names the head: the full sha, or a hex token that prefixes it.
fn text_names_head(text: &str, head: &str) -> bool {
    !head.is_empty()
        && (text.contains(head)
            || text
                .split(|c: char| !c.is_ascii_hexdigit())
                .any(|t| sha_names_head(t, head)))
}

/// A board answer recorded as a decision: the user answered a question the
/// board put to them, so the row carries the question text that names the PR
/// and head, and the answer text that picks the approve option. The page
/// stays open when its close is lost; the decision still holds the answer.
fn board_approval_text(row: &Value, pr: u64) -> Option<String> {
    let (question, answer) = board_user_answer(row, pr)?;
    answer_approves(answer).then(|| question.to_string())
}

/// A board answer naming the PR that does not pick the approve option: the
/// decline a carried approval must honor when it was answered on another
/// machine and left no page in this vault.
fn board_decline_text(row: &Value, pr: u64) -> Option<String> {
    let (question, answer) = board_user_answer(row, pr)?;
    (!answer_approves(answer)).then(|| format!("{question}\n{answer}"))
}

/// `(question, answer)` of a question-board row the user answered about this PR.
fn board_user_answer(row: &Value, pr: u64) -> Option<(&str, &str)> {
    let trace = row.get("trace")?;
    let from_board = trace.get("comms").and_then(Value::as_str) == Some("question")
        && trace.get("recipient_kind").and_then(Value::as_str) == Some("user");
    let question = row.get("question").and_then(Value::as_str)?;
    let answer = row.get("decision").and_then(Value::as_str)?;
    // The asking team may close its own question with a role, team, agent or
    // beastmode answer. Only the user's own answer (no authority stamp, or
    // an operator or quoted chat stamp) is a look.
    let by_user = match row.get("authority_source").and_then(Value::as_str) {
        None | Some("operator") => true,
        Some("chat_attested") => row
            .get("quote")
            .and_then(Value::as_str)
            .is_some_and(|q| !q.trim().is_empty()),
        Some(_) => false,
    };
    (from_board && by_user && row.get("question_id").is_some() && page_names_pr(question, pr))
        .then_some((question, answer))
}

/// Board declines of this PR in the law lane, at any head.
fn law_decline_texts<P: GateReads>(probes: &P, cwd: &Path, pr: u64) -> Vec<String> {
    let Some(payload) = decisions_payload(
        probes,
        cwd,
        &[
            "backlog",
            "decisions",
            "--lane",
            "law",
            "--state",
            "live",
            "--limit",
            "0",
            "--json",
        ],
    ) else {
        return Vec::new();
    };
    payload
        .get("decisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| board_decline_text(row, pr))
        .collect()
}

/// The text of every live role row on this node that approves this PR, at
/// any head: one decisions read, so a caller can test many heads with it.
/// A board answer on the node counts too (see `board_approval_text`).
fn role_approval_texts<P: GateReads>(probes: &P, cwd: &Path, pr: u64, node: &str) -> Vec<String> {
    // Subject-scoped and uncapped: a capped newest-first read of the whole
    // lane loses an approval once enough other rows land after it.
    let Some(payload) = decisions_payload(
        probes,
        cwd,
        &[
            "backlog",
            "decisions",
            node,
            "--lane",
            "unattributed",
            "--state",
            "all",
            "--limit",
            "0",
            "--json",
        ],
    ) else {
        return Vec::new();
    };
    let approved: Vec<String> = approving_pages(&vault_root(cwd), pr)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| !id.is_empty())
        .collect();
    let rows = payload
        .get("decisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let mut texts = Vec::new();
    for row in rows {
        if row.get("subject").and_then(Value::as_str) != Some(node) {
            continue;
        }
        if let Some(text) = board_approval_text(row, pr) {
            texts.push(text);
            continue;
        }
        let live = matches!(
            row.get("lifecycle").and_then(Value::as_str),
            Some("live") | Some("unscoped")
        );
        if row.get("authority_source").and_then(Value::as_str) == Some("role")
            && live
            && (row_is_chat_attested(row)
                || row_is_tap_attested(row)
                || row_cites_any(row, &approved)
                || row_attests_identifier_only(row))
        {
            let text = row_text(row);
            if page_names_pr(&text, pr) {
                texts.push(text);
            }
        }
    }
    texts
}

/// The text of every live law row that approves this PR, at any head: the
/// chat-attested row the worker the user spoke to records (`fno inbox law
/// set`), or the operator law the question board records for the user's own
/// board answer. The lead who owns the gate cannot re-record an approval it
/// never witnessed. A chat row must carry the user's words (a `user in
/// chat` rationale or a stored `quote`), name the PR, and open with an
/// approve word. A board row reads through `board_approval_text`, which
/// identifies it by trace (comms question, recipient user), authority and
/// question id, not by `attested_by`; its question text names the PR and
/// its answer must approve. The head test stays with the caller.
fn law_approval_texts<P: GateReads>(probes: &P, cwd: &Path, pr: u64) -> Vec<String> {
    let Some(payload) = decisions_payload(
        probes,
        cwd,
        &[
            "backlog",
            "decisions",
            "--lane",
            "law",
            "--state",
            "live",
            "--limit",
            "0",
            "--json",
        ],
    ) else {
        return Vec::new();
    };
    let mut texts = Vec::new();
    for row in payload
        .get("decisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // The board's own law row: the question text names the PR and the
        // head it asked about, and the answer picks the approve option.
        if let Some(text) = board_approval_text(row, pr) {
            texts.push(text);
            continue;
        }
        let chat_cleared = row.get("authority_source").and_then(Value::as_str)
            == Some("chat_attested")
            && row.get("lifecycle").and_then(Value::as_str) == Some("live")
            && (row_is_chat_attested(row)
                || row
                    .get("quote")
                    .and_then(Value::as_str)
                    .is_some_and(|q| !q.trim().is_empty()))
            && row
                .get("decision")
                .and_then(Value::as_str)
                .is_some_and(answer_approves);
        if chat_cleared {
            let text = row_text(row);
            if page_names_pr(&text, pr) {
                texts.push(text);
            }
        }
    }
    texts
}

/// True when a live law row that approves this PR (chat-attested, or the
/// question board's own operator-law row for the user's board answer) names
/// the head sha, full or as a prefix of seven or more hex characters. Head-scoped like the
/// role row: a push voids it.
pub(crate) fn law_chat_clears_pr<P: GateReads>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
) -> bool {
    !head.is_empty()
        && law_approval_texts(probes, cwd, pr)
            .iter()
            .any(|text| text_names_head(text, head))
}

/// The `owner/name` pair a remote url names, normalized across the ssh
/// (`git@host:owner/repo.git`), https (`https://host/owner/repo.git`), and
/// trailing-slash spellings, so the ledger scoping test reads the same
/// repository whatever form the checkout's origin takes.
pub(crate) fn repo_slug_from_origin(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut parts: Vec<&str> = trimmed
        .split(['/', ':'])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let name = parts.pop()?;
    let owner = parts.pop()?;
    Some(format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A vault with two project folders and a repo in one of them, plus the
    /// arm home whose items.json publishes the OTHER project's questions
    /// folder: the fixture for approvals filed where the daemon wrote them.
    fn cross_vault_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "mg-pub-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let vault = tmp.join("vault");
        let repo = vault.join("internal").join("fno-web").join("repo");
        std::fs::create_dir_all(repo.join(".fno")).unwrap();
        std::fs::create_dir_all(tmp.join("fno")).unwrap();
        std::fs::create_dir_all(tmp.join("empty-home")).unwrap();
        std::fs::write(
            repo.join(".fno").join("config.toml"),
            format!(
                "[project]\nid = \"fno-web\"\n\n[obsidian]\nenabled = true\nvault = \"{}\"\n",
                vault.display()
            ),
        )
        .unwrap();
        (tmp, repo)
    }

    /// Publish `dir` as the arm's questions folder via items.json in the
    /// attention dir the arm home resolves.
    fn write_published_items_json(dir: &std::path::Path) {
        let attention = crate::attention_arm::attention_dir().unwrap();
        std::fs::create_dir_all(&attention).unwrap();
        std::fs::write(
            attention.join("items.json"),
            json!({
                "as_of": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                "questions_dir": dir.display().to_string(),
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn readme_and_doc_images_are_public_copy_and_other_docs_are_not() {
        assert!(is_public_copy("README.md"));
        assert!(is_public_copy("docs/images/target-run.gif"));
        assert!(!is_public_copy("docs/architecture/pr-status-verdict.md"));
        assert!(!is_public_copy("crates/fno/README.md"));
        assert!(path_matches_paint(
            "docs/images/target-run.gif",
            &["docs/images/**".to_string()]
        ));
        let root_readme = ["/README.md".to_string()];
        assert!(path_matches_paint("README.md", &root_readme));
        assert!(!path_matches_paint(
            "docs/test-audit/README.md",
            &root_readme
        ));
    }

    #[test]
    fn path_matches_paint_skips_test_files_under_a_subtree() {
        let client = ["crates/fno/src/client/**".to_string()];
        assert!(path_matches_paint(
            "crates/fno/src/client/sideline.rs",
            &client
        ));
        for test_path in [
            "crates/fno/src/client/tests/overlay_pad_tests.rs",
            "crates/fno/src/client/tests/snapshots/a.snap",
            "crates/fno/src/client/sideline_tests.rs",
            "crates/fno/src/client/tests.rs",
        ] {
            assert!(!path_matches_paint(test_path, &client), "{test_path}");
        }
        // A name the user listed on purpose still counts, test word or not.
        assert!(path_matches_paint("tests.rs", &["tests.rs".to_string()]));
    }

    #[test]
    fn a_paint_diff_of_comments_only_skips_the_look() {
        let comments = "@@ -1,2 +1,2 @@\n ctx\n-// a busy box\n+// a busy machine\n+\n";
        assert!(patch_is_comment_only("a.rs", comments));
        let code = "@@ -1 +1 @@\n-let a = \"box\"; // c\n+let a = \"machine\"; // c\n";
        assert!(!patch_is_comment_only("a.rs", code));
        let mixed = "@@ -1 +1 @@\n-// old\n+// new\n+draw();\n";
        assert!(!patch_is_comment_only("a.rs", mixed));
        // Only Rust files read as comments; an html `//` is script code.
        assert!(!patch_is_comment_only("page.html", comments));
        // An empty patch (binary, oversized) never releases the hold.
        assert!(!patch_is_comment_only("a.rs", ""));
    }

    #[test]
    fn mux_shots_scans_node_prefix_and_shots_folder() {
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tmp, repo) = cross_vault_fixture("shots");
        // The shots land in THIS project's mux folder...
        let mux = crate::escalation::vault_dir_with_home(&repo, None, "mux");
        std::fs::create_dir_all(mux.join("x-aaaa-shots")).unwrap();
        std::fs::write(mux.join("x-aaaa-bell-dark.png"), "x").unwrap();
        std::fs::write(mux.join("fno-webmux-not-x.png"), "x").unwrap();
        std::fs::write(mux.join("x-aaaa-shots/one-after.png"), "x").unwrap();
        std::fs::write(mux.join("x-aaaa-shots/notes.txt"), "png-noted.txt").unwrap();
        // ...and one shot filed where the asking session lived: a sibling
        // vault project's mux folder. The node id is unique across the vault.
        let sibling = tmp.join("vault").join("internal").join("fno").join("mux");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("x-aaaa-remote-shot.png"), "x").unwrap();
        let shots = mux_shots(&repo, None, "x-aaaa");
        assert_eq!(
            shots,
            vec![
                "x-aaaa-bell-dark.png".to_string(),
                "x-aaaa-remote-shot.png".to_string(),
                "x-aaaa-shots/one-after.png".to_string(),
            ]
        );
        drop(lock);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn an_approval_filed_in_the_published_folder_clears_when_the_stamp_matches() {
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tmp, repo) = cross_vault_fixture("stamp");
        let saved_home = std::env::var("HOME").ok();
        let saved_arm = std::env::var("FNO_AGENTS_HOME").ok();
        std::env::set_var("HOME", tmp.join("empty-home"));
        std::env::set_var("FNO_AGENTS_HOME", tmp.join("fno"));
        let published = tmp
            .join("vault")
            .join("internal")
            .join("fno")
            .join("questions");
        std::fs::create_dir_all(published.join("done")).unwrap();
        let page = |project: &str| {
            format!(
                "---\nquestion_id: q-c0bdce8f\nstatus: answered\nanswer: \"1\"\nproject: {project}\nasked_at: 2026-11-06T10:00:00Z\n---\n# PR 81 paint approval\n\nhead a5daa9c4f00dcafe\n\nMerge as painted.\n"
            )
        };
        std::fs::write(published.join("q-c0bdce8f.md"), page("fno-web")).unwrap();
        write_published_items_json(&published);
        let head = "a5daa9c4f00dcafe";
        assert!(
            answered_question_names_pr(&repo, 81, head),
            "matching stamp approves across projects"
        );
        // A page stamped with the asker's own repo names THAT repo's PR 81;
        // the numbers collide across repos, so it must not clear this gate.
        std::fs::write(published.join("q-c0bdce8f.md"), page("fno")).unwrap();
        assert!(
            !answered_question_names_pr(&repo, 81, head),
            "a foreign-stamp page must not approve"
        );
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_arm {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
        drop(lock);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_shot_filed_in_a_sibling_project_resolves_to_its_file() {
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tmp, repo) = cross_vault_fixture("resolve");
        let mux = crate::escalation::vault_dir_with_home(&repo, None, "mux");
        std::fs::create_dir_all(&mux).unwrap();
        let sibling = tmp.join("vault").join("internal").join("fno").join("mux");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("x-aaaa-dark.png"), "x").unwrap();
        let got = resolve_shot(&mux, "x-aaaa-dark.png");
        assert_eq!(got, sibling.join("x-aaaa-dark.png"));
        std::fs::write(mux.join("x-aaaa-dark.png"), "own").unwrap();
        assert_eq!(
            resolve_shot(&mux, "x-aaaa-dark.png"),
            mux.join("x-aaaa-dark.png")
        );
        assert_eq!(
            resolve_shot(&mux, "x-never-shot.png"),
            mux.join("x-never-shot.png")
        );
        drop(lock);
        std::fs::remove_dir_all(&tmp).ok();
    }
    #[test]
    fn an_unreconciled_manifest_of_a_contract_node_holds() {
        // AC6's gate half: mocks would ship; the merge holds by code.
        let root = std::env::temp_dir().join(format!("x53c5-stub-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".fno")).unwrap();
        let node = "x-test";
        std::fs::write(
            root.join(".fno").join(format!("stub-manifest-{node}.json")),
            r#"{"reconciled": false, "stubs": [{"stub_id": "a"}]}"#,
        )
        .unwrap();
        let blocker = stub_manifest_blocker(&root, node).expect("held");
        assert_eq!(blocker.code, "stub_manifest_unreconciled");
        assert!(blocker.detail.contains("1 stub(s)"), "{}", blocker.detail);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_reconciled_manifest_clears_and_a_missing_one_never_holds() {
        let root = std::env::temp_dir().join(format!("x53c5-stub2-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".fno")).unwrap();
        let node = "x-test";
        std::fs::write(
            root.join(".fno").join(format!("stub-manifest-{node}.json")),
            r#"{"reconciled": true, "stubs": []}"#,
        )
        .unwrap();
        assert!(stub_manifest_blocker(&root, node).is_none());
        // No manifest carried: nothing to hold against.
        assert!(stub_manifest_blocker(&root, "x-never-wrote").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn contract_node_reads_the_persisted_pr_fields() {
        // The store persists `pr_number` + `additional_prs`; a dependent
        // recorded only in the additional list must still be found.
        let entries = vec![json!({
            "id": "x-3333",
            "dep": "contract",
            "pr_number": 7,
            "additional_prs": [
                {"number": 42},
                {"url": "https://github.com/o/r/pull/99"},
                105,
            ],
        })];
        for pr in [7u64, 42, 99, 105] {
            assert_eq!(
                contract_node_for_pr(&entries, pr, "o/r").as_deref(),
                Some("x-3333"),
                "pr {pr} must match"
            );
        }
        assert_eq!(contract_node_for_pr(&entries, 8, "o/r"), None);
    }

    #[test]
    fn a_page_names_the_pr_only_at_a_word_start_with_the_number() {
        assert!(page_names_pr("May PR 3036 merge?", 3036));
        assert!(page_names_pr("pr-3036 ships the splash", 3036));
        assert!(page_names_pr("(PR #3036)", 3036));
        assert!(!page_names_pr("PRs 3036 land", 3036));
        assert!(!page_names_pr("the PR is 3036", 3036));
        assert!(!page_names_pr("PR 30365", 3036));
        assert!(!page_names_pr("plain 3036", 3036));
    }

    #[test]
    fn an_approval_names_a_head_by_any_unique_hex_prefix_of_seven_or_more() {
        let head = "52caec1527aabbccddeeff00112233445566778";
        assert!(text_names_head("Approve PR 4444 at 52caec1527", head));
        assert!(text_names_head("at 52CAEC1 please", head));
        assert!(text_names_head(&format!("full {head}"), head));
        assert!(!text_names_head("at 52caec", head), "six is too short");
        assert!(!text_names_head("at 52caec9", head), "a diverging prefix");
        assert!(!text_names_head("", ""));
    }

    #[test]
    fn a_decline_after_the_approved_commit_voids_the_carry() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let look = "May PR 9 merge as painted? (node at 01234567)\nHold for changes";
        assert!(visual_decline_at_head(look, 9, head));
        assert!(!visual_decline_at_head(
            look,
            9,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!visual_decline_at_head(look, 8, head));
        for (question, expected) in [
            ("visual approval PR 9 at 01234567", true),
            ("Approve the look of PR 9 at 01234567?", true),
            ("May PR 9 merge at 01234567?", true),
            ("Can PR 9 merge at 01234567?", true),
            ("May PR 9 change its merge-base at 01234567?", false),
            (
                "How does PR 9 configure visual approval at 01234567?",
                false,
            ),
            ("How does PR 9 configure routing at 01234567?", false),
        ] {
            let text = format!("{question}\nHold for changes");
            assert_eq!(visual_decline_at_head(&text, 9, head), expected, "{text}");
        }
        let question = look.lines().next().unwrap();
        let page = |answer| {
            format!(
                "---\nquestion_id: q-1\nstatus: answered\nask: {question}\nanswer: {answer}\n---\n"
            )
        };
        for (answer, expected) in [
            ("Hold for changes", true),
            ("b", true),
            ("2. Hold for changes", true),
            ("No, change the border", true),
            ("not sure", false),
            ("Resolved by the team", false),
        ] {
            assert_eq!(
                visual_decline_at_head(&page(answer), 9, head),
                expected,
                "{answer}"
            );
        }
        assert!(!visual_decline_at_head(question, 9, head));
        let declines = vec!["May PR 9 merge? answer 2 at c3".to_string()];
        assert!(declined_after(&declines, &["c3", "c4"]));
        let spelled = vec!["May PR 9 merge? answer 2 at 0123456789".to_string()];
        assert!(declined_after(
            &spelled,
            &["0123456789abcdef0123456789abcdef01234567"]
        ));
        assert!(!declined_after(&declines, &["c4"]));
        assert!(!declined_after(&[], &["c3"]));
        // A decline answered on another machine arrives as a board row, not a page.
        let board = |answer: &str| {
            json!({
                "trace": {"comms": "question", "recipient_kind": "user"},
                "question_id": "q-1",
                "question": "May PR 9 merge at 0123456789?",
                "decision": answer,
            })
        };
        let text = board_decline_text(&board("Hold for changes"), 9).unwrap();
        assert!(declined_after(
            &[text],
            &["0123456789abcdef0123456789abcdef01234567"]
        ));
        assert!(board_decline_text(&board("Approve"), 9).is_none());
        assert!(board_decline_text(&board("Hold for changes"), 8).is_none());
        // A reworded option label reads as an approval, so it never voids
        // the carry, and the gate reads the board answer it came from.
        assert!(answer_approves("Ship as painted"));
        assert!(board_decline_text(&board("Ship as painted"), 9).is_none());
    }

    #[test]
    fn an_open_look_page_for_the_head_stops_the_carry() {
        let page = |status: &str, text: &str| {
            format!("---\nquestion_id: q-1\nstatus: {status}\n---\n{text}\n")
        };
        let head = "c6e25abe8bfe407ca8a10288046b559cffc2e8c2";
        let open = page("open", "visual approval PR 4487 at c6e25abe");
        let answered = page("answered", "visual approval PR 4487 at c6e25abe");
        let other_head = page("open", "visual approval PR 4487 at 8e974218");
        let other_pr = page("open", "visual approval PR 4488 at c6e25abe");
        let names =
            |pages: &[&String]| open_page_names_head(pages.iter().map(|p| p.as_str()), 4487, head);
        assert!(names(&[&open]));
        assert!(!names(&[&answered]));
        assert!(!names(&[&other_head, &other_pr]));
        assert!(!names(&[]));
    }

    #[test]
    fn the_carry_reference_is_the_newest_earlier_commit_an_approval_names() {
        // The records a fleet writes spell the head short: a decision or a
        // question page names a prefix, never the 40-character sha. The carry
        // reference is the newest earlier commit any approval text names;
        // merges and test-only commits between it and the head no longer
        // matter, the frames compare decides the carry.
        let approved = "fb3c72b0c9aabbccddeeff0011223344556677aa";
        let main_merge = "1394dcb7cfaabbccddeeff0011223344556677bb";
        let head = "8ba2229a40aabbccddeeff0011223344556677dd";
        let commits: Vec<String> = [approved, main_merge, head]
            .into_iter()
            .map(String::from)
            .collect();
        let texts = vec![
            "Approved: PR 4487 at fb3c72c (user in chat: looks right)".to_string(),
            // A page that names the head full still carries.
            "visual approval PR 4487 at 1394dcb7cfaabbccddeeff0011223344556677bb".to_string(),
        ];
        assert_eq!(
            carry_reference(&commits, head, &texts),
            Some(main_merge),
            "newest named earlier commit wins"
        );
        // A short-spelled decision names the approved commit even when the
        // commits list spells it full.
        let short_only =
            vec!["Approved: PR 4487 at fb3c72b0c9 (user in chat: looks right)".to_string()];
        assert_eq!(
            carry_reference(&commits, head, &short_only),
            Some(approved),
            "the short sha names the full commit"
        );
        // No earlier commit is named: nothing carries.
        assert_eq!(
            carry_reference(&commits, head, &["at 52caec1".to_string()]),
            None
        );
    }

    #[test]
    fn an_identifier_only_ruling_reads_as_attested_and_other_rationales_do_not() {
        let attested = |r: &str| row_attests_identifier_only(&json!({ "rationale": r }));
        assert!(attested(
            "Visual approval is granted by the lead. The paint-file edits rename local \
             identifiers only (the action name became the command name). Every rendered \
             string stays byte-identical, e.g. tab_prune.rs still prints 'would retire' \
             and 'pruned'. No pixel changes."
        ));
        assert!(attested(
            "identifier-only paint diff: fn run renamed to fn cmd; footer still prints \
             'would retire'"
        ));
        // A look lane and a board citation are their own attestations, not
        // this one; a bare approval with no identifier claim never reads as
        // identifier-only.
        assert!(!attested("user in chat: approved the splash"));
        assert!(!attested("user answered q-1 on the board"));
        assert!(!attested("Approved without a look"));
        assert!(!attested(""));
        assert!(!row_attests_identifier_only(&json!({})));
    }

    /// The specimen behind the scoping rule: the ledger is global and fnh
    /// PR 70 exists, so a fno-web checkout asking about its own PR 70 saw
    /// fnh's plan answer and the fidelity gate refused the wrong PR. A row
    /// whose pr_url names another repo never answers; a slug-matching row or
    /// a row with no pr_url still does.
    #[test]
    fn a_foreign_repos_pr_url_never_answers_a_shared_pr_number() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Saved-env guard: other tests read these pins, so restore, not remove.
        // Claiming hermetic manufactures the fence's claim, so every root the
        // fence judges must be pinned under std::env::temp_dir() here: the CI
        // cargo job exports FNO_AGENTS_HOME=$RUNNER_TEMP/fno-agents for the
        // whole job, and an unpinned claim made the fence refuse that ambient
        // value (the cli-ci red this test shipped).
        struct Pins {
            hermetic: Option<std::ffi::OsString>,
            state: Option<std::ffi::OsString>,
            agents_home: Option<std::ffi::OsString>,
            spaces: Option<std::ffi::OsString>,
            fno_home: Option<std::ffi::OsString>,
        }
        impl Drop for Pins {
            fn drop(&mut self) {
                match &self.hermetic {
                    Some(v) => std::env::set_var("FNO_TEST_HERMETIC", v),
                    None => std::env::remove_var("FNO_TEST_HERMETIC"),
                }
                match &self.state {
                    Some(v) => std::env::set_var("FNO_STATE_DIR", v),
                    None => std::env::remove_var("FNO_STATE_DIR"),
                }
                match &self.agents_home {
                    Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
                    None => std::env::remove_var("FNO_AGENTS_HOME"),
                }
                match &self.spaces {
                    Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                    None => std::env::remove_var("FNO_SPACES_DIR"),
                }
                match &self.fno_home {
                    Some(v) => std::env::set_var("FNO_HOME", v),
                    None => std::env::remove_var("FNO_HOME"),
                }
            }
        }
        let _pins = Pins {
            hermetic: std::env::var_os("FNO_TEST_HERMETIC"),
            state: std::env::var_os("FNO_STATE_DIR"),
            agents_home: std::env::var_os("FNO_AGENTS_HOME"),
            spaces: std::env::var_os("FNO_SPACES_DIR"),
            fno_home: std::env::var_os("FNO_HOME"),
        };
        std::env::set_var("FNO_TEST_HERMETIC", "1");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_STATE_DIR", state.path());
        std::env::set_var("FNO_AGENTS_HOME", state.path().join("agents-home"));
        std::env::set_var("FNO_SPACES_DIR", state.path().join("spaces"));
        std::env::set_var("FNO_HOME", state.path().join("fno-home"));

        // The checkout: fno-web origin, so the slug is fnodotdev/fno-web.
        let repo = tempfile::tempdir().unwrap();
        for args in [
            ["init", "-q"].as_slice(),
            [
                "config",
                "remote.origin.url",
                "git@github.com:fnodotdev/fno-web.git",
            ]
            .as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?} failed");
        }

        let ledger_path = crate::paths_cli::ledger_json(repo.path());
        std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
        let write_ledger = |entries: serde_json::Value| {
            std::fs::write(
                &ledger_path,
                serde_json::to_string(&json!({ "entries": entries })).unwrap(),
            )
            .unwrap();
        };
        let fnh_row = json!({
            "pr_number": 70,
            "pr_url": "https://github.com/fnodotdev/fnh/pull/70",
            "plan_path": "/fnh/plan.md",
        });

        // The specimen: only the other repo's row exists. It names another
        // repo, so it never answers and the gate stays silent.
        write_ledger(json!([fnh_row]));
        assert_eq!(ledger_plan_path(repo.path(), 70), None);

        // An unresolvable origin cannot prove any pr_url is ours, so a row
        // that carries one stays silent there too.
        let bare = tempfile::tempdir().unwrap();
        write_ledger(json!([fnh_row]));
        assert_eq!(ledger_plan_path(bare.path(), 70), None);

        // A row with no pr_url may answer: unmarked provenance keeps the old
        // first-match fail-open shape.
        write_ledger(json!([
            fnh_row,
            json!({"pr_number": 70, "plan_path": "/web/plan.md"})
        ]));
        assert_eq!(
            ledger_plan_path(repo.path(), 70).as_deref(),
            Some("/web/plan.md")
        );

        // The slug match outranks the rest, whatever order the rows hold.
        write_ledger(json!([
            fnh_row,
            json!({
                "pr_number": 70,
                "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
                "plan_path": "/web/chosen.md",
            }),
        ]));
        assert_eq!(
            ledger_plan_path(repo.path(), 70).as_deref(),
            Some("/web/chosen.md")
        );

        // A prefix-sibling repo's url contains this slug as a substring but
        // names a different path segment; it never reads as a match.
        write_ledger(json!([json!({
            "pr_number": 70,
            "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
            "plan_path": "/web/plan.md",
        })]));
        let sibling = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(sibling.path())
            .status()
            .expect("git runs");
        assert!(status.success(), "git init failed");
        let status = std::process::Command::new("git")
            .args([
                "config",
                "remote.origin.url",
                "git@github.com:fnodotdev/fno.git",
            ])
            .current_dir(sibling.path())
            .status()
            .expect("git runs");
        assert!(status.success(), "git config failed");
        let sibling_ledger = crate::paths_cli::ledger_json(sibling.path());
        std::fs::create_dir_all(sibling_ledger.parent().unwrap()).unwrap();
        std::fs::write(
            &sibling_ledger,
            serde_json::to_string(&json!({ "entries": [json!({
                "pr_number": 70,
                "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
                "plan_path": "/web/plan.md",
            })] }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(ledger_plan_path(sibling.path(), 70), None);
    }
    #[test]
    fn the_carry_maps_absent_declined_clear_ask_and_warn_over_the_paint_diff() {
        // The verdict mapping itself, over facts the fakes serve: no look
        // record anywhere is Absent; a decline after the approved commit is
        // Declined; a compare naming no paint file Clear; paint hunks Ask
        // with the payload naming what moved; an unreadable compare Warn.
        struct RouteFake(Vec<(String, String)>);
        impl GateReads for RouteFake {
            fn gh_read(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String), String> {
                let joined = args.join(" ");
                for (needle, out) in &self.0 {
                    if joined.contains(needle.as_str()) {
                        return Ok((true, out.clone()));
                    }
                }
                Err("no route".to_string())
            }
            fn fno_read(
                &self,
                _cwd: &Path,
                _args: &[String],
            ) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
                Err("no fno route".to_string())
            }
        }
        let tmp = std::env::temp_dir().join(format!("xc-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".fno")).unwrap();
        let spaces = std::env::temp_dir().join(format!("xc-map-spaces-{}", std::process::id()));
        std::fs::create_dir_all(&spaces).unwrap();
        let _env_lock = crate::pr_status::cache_env_lock();
        crate::paths::pin_test_claims_root(&spaces);
        // A guard, not a manual restore: a mid-test assert must not leak the
        // pinned env into the tests that follow in this process.
        struct RestoreEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                    None => std::env::remove_var("FNO_SPACES_DIR"),
                }
                match self.1.take() {
                    Some(v) => std::env::set_var("FNO_HOME", v),
                    None => std::env::remove_var("FNO_HOME"),
                }
            }
        }
        let _env = RestoreEnv(
            std::env::var_os("FNO_SPACES_DIR"),
            std::env::var_os("FNO_HOME"),
        );
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        std::env::set_var("FNO_HOME", &tmp);
        std::fs::write(
            tmp.join(".fno/config.toml"),
            "merge.visual_paint_paths = [\"crates/fno/src/client/**\"]\n",
        )
        .unwrap();
        let qdir = crate::escalation::questions_dir(&tmp);
        std::fs::create_dir_all(&qdir).unwrap();
        let head = "a".repeat(40);
        let pushed = "b".repeat(40);
        let node = "x-carrymap";
        let paint = vec!["crates/fno/src/client/**".to_string()];
        let page = |answer: &str, body: &str| {
            format!(
                "---\nquestion_id: q-map\nstatus: answered\ntitle: May PR 9 merge?\n\
                 answer: {answer}\n---\n{body}\n"
            )
        };
        let approve = page("1. Merge as painted", &format!("PR 9 at head {head}"));
        let hold = page(
            "2. Hold for changes",
            &format!("PR 9 at {}", pushed.get(..10).unwrap_or(&pushed)),
        );
        // The compare read between the approved commit and the pushed head;
        // each case serves its own body, and Warn serves no compare route.
        let routes = |compare: &str| -> Vec<(String, String)> {
            vec![
                ("/commits".to_string(), format!("{head}\n{pushed}\n")),
                (format!("compare/{head}...{pushed}"), compare.to_string()),
            ]
        };
        // Absent: no page anywhere, whatever the compare answers.
        let fake = RouteFake(routes(""));
        assert!(matches!(
            approval_carries(&fake, &tmp, 9, &pushed, Some(node), &paint),
            Carry::Absent
        ));
        // Declined: an approve at head, then a hold naming the pushed commit.
        std::fs::write(qdir.join("q-map-approve.md"), &approve).unwrap();
        std::fs::write(qdir.join("q-map-hold.md"), &hold).unwrap();
        assert!(matches!(
            approval_carries(&fake, &tmp, 9, &pushed, Some(node), &paint),
            Carry::Declined(_)
        ));
        std::fs::remove_file(qdir.join("q-map-hold.md")).unwrap();
        // Clear: files moved since the approved commit, none on the paint
        // surface, so the look stands.
        std::fs::write(qdir.join("q-map-approve.md"), &approve).unwrap();
        let fake = RouteFake(routes("PAINTFILE README.md\n@@ -1,2 +1,3 @@\n"));
        assert!(matches!(
            approval_carries(&fake, &tmp, 9, &pushed, Some(node), &paint),
            Carry::Clear
        ));
        // Ask: paint moved, and the payload names the approved commit and
        // the hunk that moved, so the re-ask page can list it.
        let fake = RouteFake(routes(
            "PAINTFILE crates/fno/src/client/theme.rs\n@@ -10,7 +10,9 @@ fn theme()\n",
        ));
        let Carry::Ask(ask) = approval_carries(&fake, &tmp, 9, &pushed, Some(node), &paint) else {
            panic!("paint moved since the approval, so the carry asks");
        };
        assert_eq!(ask.approved, head);
        let [hunk] = ask.hunks.as_slice() else {
            panic!("one paint file moved, so the payload names one hunk");
        };
        assert_eq!(hunk.file, "crates/fno/src/client/theme.rs");
        assert_eq!(hunk.headers, vec!["@@ -10,7 +10,9 @@ fn theme()"]);
        // Warn: the compare read fails, and the carry warns instead of
        // holding.
        let fake = RouteFake(vec![(
            "/commits".to_string(),
            format!("{head}\n{pushed}\n"),
        )]);
        assert!(matches!(
            approval_carries(&fake, &tmp, 9, &pushed, Some(node), &paint),
            Carry::Warn(_)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&spaces);
    }

    #[test]
    fn paint_markers_parse_into_paint_filtered_hunks_and_summaries() {
        // Compare marker lines into per-file records: a paint file keeps its
        // hunk headers, a paint file with no patch body lists bare, a
        // non-paint file drops together with its headers, and a stray header
        // before any file is ignored. The summary is the re-ask page's line.
        let paint = vec!["crates/fno/src/client/**".to_string()];
        let hunks = parse_paint_markers(
            &[
                "@@ -1,2 +1,3 @@ orphan".to_string(),
                "PAINTFILE crates/fno/src/client/theme.rs".to_string(),
                "@@ -10,7 +10,9 @@ fn theme()".to_string(),
                "@@ -40,6 +42,7 @@".to_string(),
                "PAINTFILE README.md".to_string(),
                "@@ -1,2 +1,3 @@".to_string(),
                "PAINTFILE crates/fno/src/client/help.rs".to_string(),
            ],
            &paint,
        );
        let [theme, bare] = hunks.as_slice() else {
            panic!("two paint files survive the filter, got {}", hunks.len());
        };
        assert_eq!(theme.file, "crates/fno/src/client/theme.rs");
        assert_eq!(
            theme.headers,
            vec!["@@ -10,7 +10,9 @@ fn theme()", "@@ -40,6 +42,7 @@"]
        );
        assert_eq!(bare.file, "crates/fno/src/client/help.rs");
        assert!(bare.headers.is_empty());
        assert_eq!(
            paint_hunk_summary(theme),
            "crates/fno/src/client/theme.rs @@ -10,7 +10,9 @@ fn theme() @@ -40,6 +42,7 @@"
        );
        assert_eq!(
            paint_hunk_summary(bare),
            "crates/fno/src/client/help.rs (no hunk bodies in the compare)"
        );
    }
}
#[cfg(test)]
mod scoped_read_tests {
    use super::*;
    use crate::merge_gates::GateReads;
    use std::time::Duration;

    #[test]
    fn resolved_reads_pin_actual_child_repo_and_host() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo-b");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["remote", "add", "origin", "https://github.com/owner/b.git"],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .status()
                .unwrap()
                .success());
        }
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "state_dir = {:?}\n[merge]\nvisual_paint_paths = [\"paint.rs\"]\n",
                dir.path()
            ),
        )
        .unwrap();
        let log = dir.path().join("targets");
        let script = format!(
            r#"#!/bin/sh
printf '%s|%s|%s\n' "$GH_REPO" "$GH_HOST" "$*" >> '{}'
case "$1" in
  api) printf '%s\n' other.rs ;;
  pr) printf '%s\n' '{{"baseRefName":"main","headRefName":"feature"}}' ;;
  *) printf '%s\n' '{{}}' ;;
esac
"#,
            log.display()
        );
        crate::write_exec_stub(dir.path(), "gh", &script);
        let fno = crate::write_exec_stub(dir.path(), "fno", &script);
        let _fno = crate::claims::EnvVarGuard::set("FNO_BIN", fno.to_str().unwrap());
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path = crate::claims::EnvVarGuard::set("PATH", &path);
        let _cfg = crate::claims::EnvVarGuard::set("FNO_CONFIG", config.to_str().unwrap());
        let _repo = crate::claims::EnvVarGuard::set("GH_REPO", "owner/a");
        let _host = crate::claims::EnvVarGuard::set("GH_HOST", "ambient.invalid");

        assert!(std::process::Command::new("git")
            .args([
                "config",
                "url./local-mirror/.insteadOf",
                "https://github.com/"
            ])
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success());
        let facts = PrFacts {
            number: 42,
            head_sha: "a".repeat(40),
            head_ref: "feature".into(),
            base_ref: "main".into(),
            url: "https://github.com/owner/b/pull/42".into(),
            body: Some(String::new()),
            state: "OPEN".into(),
            armed: false,
        };
        let reads = crate::merge_gates::ScopedReads::for_checkout(&repo);
        assert!(crate::merge_gates::visual_approval_blocker(
            &reads,
            &repo,
            facts.number,
            &facts.head_sha
        )
        .is_none());
        assert!(crate::docs_gate::docs_gate_blocker(&reads, &repo, &facts).is_none());
        assert_eq!(
            crate::merge_gates::pr_base_head_refs(&ScopedReads::for_checkout(&repo), &repo, 42),
            Some(("main".into(), "feature".into()))
        );
        ScopedReads::for_checkout(&repo)
            .program(
                fno.as_os_str(),
                &["pr", "coverage-check", "42"],
                &repo,
                Duration::from_secs(5),
            )
            .unwrap();
        ScopedReads::for_checkout(&repo)
            .fno_read(
                &repo,
                &[
                    "backlog".into(),
                    "decisions".into(),
                    "subject".into(),
                    "--json".into(),
                ],
            )
            .unwrap();
        let targets = std::fs::read_to_string(log).unwrap();
        eprintln!("Actual scoped child targets:\n{targets}");
        for required in ["|pr coverage-check 42", "|backlog decisions subject --json"] {
            assert!(
                targets.lines().any(|target| target.contains(required)),
                "missing scoped read {required}: {targets}"
            );
        }
        assert!(
            !targets.contains("{owner}"),
            "unscoped API endpoint: {targets}"
        );
        assert!(
            targets.contains("api repos/owner/b/pulls/42/files"),
            "{targets}"
        );
        assert!(
            targets.contains("--repo owner/b"),
            "unscoped pr view: {targets}"
        );
        for target in targets.lines() {
            assert!(
                target.starts_with("owner/b|github.com|"),
                "wrong read target: {target}"
            );
        }
    }
}
