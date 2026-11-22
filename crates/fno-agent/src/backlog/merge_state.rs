//! The merge-state read behind `backlog reconcile` and the close path:
//! one PR's open/closed/merged state with merged-file evidence, served
//! first from the permanent gh-cache row (merged facts never change).
//! Ported from graph/_reconcile.py (`PrMergeState`, `query_pr_merge_state`)
//! on the merge_evidence reader plumbing and the gh_cache row store.

use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use super::merge_evidence::PrReadError;
use super::pr_link;
use crate::gh_cache;
use crate::pr_push::run_labeled;

/// The Python reader's timeout (`GH_QUERY_TIMEOUT_S`): covers network hangs
/// and stuck auth prompts.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const FILES_PAGE_SIZE: usize = 100;
/// GitHub exposes at most 3,000 files on this endpoint and a full final
/// page has no positive tail marker, so fail closed rather than call an
/// incomplete list complete.
const FILES_MAX_PAGES: usize = 30;

/// One PR's GitHub-confirmed state (the PrMergeState twin).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrMergeState {
    pub number: i64,
    pub state: String,
    pub url: Option<String>,
    pub merged_at: Option<String>,
    pub merge_sha: Option<String>,
    pub changed_files: Vec<String>,
    pub files_truncated: bool,
    /// The PR body; its closure line names the nodes this merge closes.
    /// None on a cache hit: a body stays editable after merge, so the
    /// permanent row never holds it ([`read_body`] reads it fresh).
    pub body: Option<String>,
}

/// The production read: cache first, REST on a miss, write-back on a fresh
/// MERGED read. Tests go through [`fresh_read`] and [`paginate_files`] with
/// injected readers, so no test traffic reaches gh. A recorded repo reads
/// under the rename aliases, so a PR recorded before a repo move (the
/// fno-web org move ships a built-in row because the old name 404s with no
/// redirect) answers its new location, in the cache row and the gh call
/// alike.
pub(crate) fn query_pr_merge_state(
    pr: i64,
    repo: Option<&str>,
    cwd: Option<&str>,
    include_files: bool,
) -> Result<PrMergeState, PrReadError> {
    let cwd_path = Path::new(cwd.unwrap_or("."));
    let repo = repo.map(|r| pr_link::canonical_repo(cwd_path, r));
    if let Some(row) = cached_row(pr, repo.as_deref(), cwd_path, include_files) {
        return Ok(row);
    }
    let fresh = fresh_read(
        pr,
        repo.as_deref(),
        cwd_path,
        include_files,
        read_info,
        read_files,
    )?;
    if fresh.state == "MERGED" {
        write_cache_row(pr, repo.as_deref(), cwd_path, &fresh, include_files);
    }
    Ok(fresh)
}

/// The gh-cache hit: `{"info": {...}, "files": [...]?}` with a string
/// state, and the files list present exactly when this read asked for it.
fn cached_row(
    pr: i64,
    repo: Option<&str>,
    cwd: &Path,
    include_files: bool,
) -> Option<PrMergeState> {
    let slug = repo?;
    let root = gh_cache::rows_root(cwd)?;
    let now = gh_cache::now_secs();
    let row = gh_cache::read_row(&root, "merged", slug, Some(pr as u64), None, now);
    let row = row.get("row")?.as_object()?;
    let info = row.get("info")?.as_object()?;
    let state = info.get("state")?.as_str()?.to_string();
    if include_files && !row.get("files").map(Value::is_array).unwrap_or(false) {
        return None;
    }
    Some(PrMergeState {
        number: info.get("pr").and_then(Value::as_i64).unwrap_or(pr),
        state,
        url: str_field(info.get("url")),
        merged_at: str_field(info.get("merged_at")),
        merge_sha: str_field(info.get("merge_sha")),
        changed_files: row
            .get("files")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        files_truncated: false,
        body: None,
    })
}

fn write_cache_row(
    pr: i64,
    repo: Option<&str>,
    cwd: &Path,
    state: &PrMergeState,
    include_files: bool,
) {
    let Some(slug) = repo else { return };
    let Some(root) = gh_cache::rows_root(cwd) else {
        return;
    };
    let mut row = json!({
        "info": {
            "pr": state.number,
            "state": state.state,
            "url": state.url,
            "merged_at": state.merged_at,
            "merge_sha": state.merge_sha,
        }
    });
    if include_files {
        row["files"] = json!(state.changed_files);
    }
    gh_cache::write_row(&root, "merged", slug, Some(pr as u64), &row);
}

/// The REST read with no cache leg. The two reader seams keep tests
/// hermetic (the Python `info_reader`/`files_reader` contract); the
/// production seams are [`read_info`] and [`read_files`].
pub(crate) fn fresh_read(
    pr: i64,
    repo: Option<&str>,
    cwd: &Path,
    include_files: bool,
    info_reader: impl Fn(i64, Option<&str>, &Path) -> Result<Value, PrReadError>,
    files_reader: impl Fn(i64, Option<&str>, &Path) -> Result<Vec<String>, PrReadError>,
) -> Result<PrMergeState, PrReadError> {
    let info = info_reader(pr, repo, cwd)?;
    let map = info.as_object().ok_or_else(|| {
        PrReadError::new(
            "REST PR info reader returned a value that is not an object",
            "malformed",
        )
    })?;
    if !map.contains_key("pr") || !map.contains_key("state") {
        return Err(PrReadError::new(
            "REST PR info reader omitted required PR number or state",
            "malformed",
        ));
    }
    let state = match map.get("state") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(PrReadError::new(
                format!("REST PR info reader returned malformed state {other}"),
                "malformed",
            ))
        }
        None => unreachable!("state key checked above"),
    };
    if !matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED") {
        return Err(PrReadError::new(
            format!("REST PR info reader returned malformed state {state:?}"),
            "malformed",
        ));
    }
    let changed_files = if state == "MERGED" && include_files {
        files_reader(pr, repo, cwd)?
    } else {
        Vec::new()
    };
    let number = match map.get("pr") {
        None => pr,
        Some(v) => v.as_i64().ok_or_else(|| {
            PrReadError::new(
                "REST PR info reader returned a malformed PR number",
                "malformed",
            )
        })?,
    };
    Ok(PrMergeState {
        number,
        state,
        url: str_field(map.get("url")),
        merged_at: str_field(map.get("merged_at")),
        merge_sha: str_field(map.get("merge_sha")),
        changed_files,
        files_truncated: false,
        body: Some(
            map.get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
    })
}

/// The PR body, read fresh: the closure evidence a cached merge row omits.
pub(crate) fn read_body(
    pr: i64,
    repo: Option<&str>,
    cwd: Option<&str>,
) -> Result<String, PrReadError> {
    let info = read_info(pr, repo, Path::new(cwd.unwrap_or(".")))?;
    Ok(info
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

/// The repo slug a read answers against: the explicit repo under the
/// rename aliases, else the checkout's own. A read with neither refuses
/// rather than risk asking the wrong repository.
fn repo_slug_or_err(repo: Option<&str>, cwd: &Path) -> Result<String, PrReadError> {
    match repo {
        Some(r) => Ok(pr_link::canonical_repo(cwd, r)),
        None => pr_link::resolve_current_repo_slug(cwd.to_str())
            .ok_or_else(|| PrReadError::new("could not resolve owner/repo from the checkout", "")),
    }
}

/// The production info seam: resolve the slug, one `gh api` pull read,
/// shape it into the info object the cache row and PrMergeState share.
fn read_info(pr: i64, repo: Option<&str>, cwd: &Path) -> Result<Value, PrReadError> {
    let slug = repo_slug_or_err(repo, cwd)?;
    let path = format!("repos/{slug}/pulls/{pr}");
    let data = gh_api_json(&path, cwd)?;
    let state = if data.get("merged").and_then(Value::as_bool).unwrap_or(false) {
        "MERGED".to_string()
    } else {
        let raw = data.get("state").and_then(Value::as_str).ok_or_else(|| {
            PrReadError::new(
                "REST PR info reader omitted required PR number or state",
                "malformed",
            )
        })?;
        match raw.to_ascii_uppercase().as_str() {
            "OPEN" => "OPEN".to_string(),
            "CLOSED" => "CLOSED".to_string(),
            // REST never says MERGED as a state string; a merged PR is
            // state "closed" with merged true (the flag wins above).
            other => {
                return Err(PrReadError::new(
                    format!("REST PR info reader returned malformed state {other:?}"),
                    "malformed",
                ))
            }
        }
    };
    Ok(json!({
        "pr": pr,
        "state": state,
        "url": data.get("html_url"),
        "merged_at": data.get("merged_at"),
        "merge_sha": data.get("merge_commit_sha"),
        "body": data.get("body").and_then(Value::as_str).unwrap_or_default(),
    }))
}

/// The production files seam: every changed-file path from the paginated
/// REST files endpoint.
fn read_files(pr: i64, repo: Option<&str>, cwd: &Path) -> Result<Vec<String>, PrReadError> {
    let slug = repo_slug_or_err(repo, cwd)?;
    paginate_files(|page| {
        let path = format!("repos/{slug}/pulls/{pr}/files?per_page={FILES_PAGE_SIZE}&page={page}");
        gh_api_json(&path, cwd)
    })
}

/// One PR commit as the commits endpoint serves it: the message is the
/// naming evidence.
#[derive(serde::Deserialize)]
struct CommitRow {
    #[serde(default)]
    commit: CommitInner,
}

#[derive(serde::Deserialize, Default)]
struct CommitInner {
    #[serde(default)]
    message: String,
}

/// The production commits seam: every commit message from the paginated
/// REST commits endpoint. The commit half of the cascade's naming
/// evidence: a contained child named in a commit message shipped with its
/// change even when the body omits it.
pub(crate) fn read_commit_messages(
    pr: i64,
    repo: Option<&str>,
    cwd: Option<&str>,
) -> Result<Vec<String>, PrReadError> {
    let cwd_path = Path::new(cwd.unwrap_or("."));
    let slug = repo_slug_or_err(repo, cwd_path)?;
    paginate_messages(|page| {
        let path =
            format!("repos/{slug}/pulls/{pr}/commits?per_page={FILES_PAGE_SIZE}&page={page}");
        gh_api_json(&path, cwd_path)
    })
}

/// One `gh api` read returning the parsed JSON body. A concrete path rides
/// the conditional etag door (`eligible` names exactly that shape), so the
/// watch lanes' repeat reads of an unchanged PR answer 304 for no core
/// point; a template path keeps the plain spawn with the escape-sequences
/// flag, whose substitution the door's fingerprint cannot carry.
fn gh_api_json(path: &str, cwd: &Path) -> Result<Value, PrReadError> {
    gh_api_json_via(cwd, path, |argv| {
        run_labeled("backlog-evidence", "gh", argv, cwd, QUERY_TIMEOUT)
            .map(|(ok, out, err)| (ok, out.into_bytes(), err))
    })
}

/// The door-routing body behind [`gh_api_json`], with the gh spawn
/// injected: a concrete path rides the conditional etag door, a template
/// path keeps the plain spawn. The test injects an argv recorder instead of
/// a real gh.
fn gh_api_json_via(
    cwd: &Path,
    path: &str,
    spawn: impl Fn(&[&str]) -> Result<(bool, Vec<u8>, String), String>,
) -> Result<Value, PrReadError> {
    let plain = ["api".to_string(), path.to_string()];
    if crate::gh_etag::eligible(&plain).is_some() {
        let (ok, out, err) = crate::gh_etag::conditional_api_get(cwd, path, spawn)
            .map_err(|e| PrReadError::new(e, "availability"))?;
        if !ok {
            let message = if err.trim().is_empty() { out } else { err };
            return Err(PrReadError::new(message, ""));
        }
        return serde_json::from_str(out.trim()).map_err(|e| {
            PrReadError::new(
                format!("gh api returned output that is not JSON: {e}"),
                "malformed",
            )
        });
    }
    let (ok, out, err) = (spawn)(&["api", "--allow-escape-sequences", path])
        .map_err(|e| PrReadError::new(e, "availability"))?;
    let out = String::from_utf8_lossy(&out).into_owned();
    if !ok {
        let message = if err.trim().is_empty() { out } else { err };
        return Err(PrReadError::new(message, ""));
    }
    serde_json::from_str(out.trim()).map_err(|e| {
        PrReadError::new(
            format!("gh api returned output that is not JSON: {e}"),
            "malformed",
        )
    })
}

/// The pagination loop over the files endpoint: stop on the first short
/// page, fail closed at the 3,000-file cap, refuse a malformed row.
fn paginate_files(
    mut page_fetch: impl FnMut(usize) -> Result<Value, PrReadError>,
) -> Result<Vec<String>, PrReadError> {
    let mut paths: Vec<String> = Vec::new();
    for page in 1..=FILES_MAX_PAGES {
        let payload = page_fetch(page)?;
        let rows = payload.as_array().ok_or_else(|| {
            PrReadError::new(
                format!("gh api pull files page {page} returned a value that is not an array"),
                "malformed",
            )
        })?;
        for (index, row) in rows.iter().enumerate() {
            let name = row
                .get("filename")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    PrReadError::new(
                        format!("gh api pull files page {page} carried malformed row {index}"),
                        "malformed",
                    )
                })?;
            paths.push(name.to_string());
        }
        if rows.len() < FILES_PAGE_SIZE {
            return Ok(paths);
        }
    }
    Err(PrReadError::new(
        "gh api pull files reached GitHub's 3,000-file cap without a short page",
        "",
    ))
}

/// The pagination loop over the commits endpoint, the files loop's twin:
/// stop on the first short page, fail closed at the same page cap, parse
/// each row once at the boundary into the typed row. A commit with an
/// empty message is skipped, not a refusal.
fn paginate_messages(
    mut page_fetch: impl FnMut(usize) -> Result<Value, PrReadError>,
) -> Result<Vec<String>, PrReadError> {
    let mut messages: Vec<String> = Vec::new();
    for page in 1..=FILES_MAX_PAGES {
        let payload = page_fetch(page)?;
        let rows = payload.as_array().ok_or_else(|| {
            PrReadError::new(
                format!("gh api pull commits page {page} returned a value that is not an array"),
                "malformed",
            )
        })?;
        for row in rows {
            let parsed: CommitRow = serde_json::from_value(row.clone()).map_err(|e| {
                PrReadError::new(
                    format!("gh api pull commits page {page} carried a malformed row: {e}"),
                    "malformed",
                )
            })?;
            if !parsed.commit.message.is_empty() {
                messages.push(parsed.commit.message);
            }
        }
        if rows.len() < FILES_PAGE_SIZE {
            return Ok(messages);
        }
    }
    Err(PrReadError::new(
        "gh api pull commits reached GitHub's page cap without a short page",
        "",
    ))
}

pub(crate) fn str_field(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// One PR's closure context: body + state from a single `gh pr view`
/// (the query-once contract: the trailer the gate reads and the state it
/// is read against must come from one fetch).
#[derive(Debug, Clone)]
pub(crate) struct ClosureContext {
    pub body: String,
    pub url: Option<String>,
    pub state: String,
    pub changed_files: Vec<String>,
}

/// `gh pr view` once for body + merge state + files. A typed error on any
/// gh failure; blank exit-0 output refuses (never read a bare exit 0 as
/// permission).
pub(crate) fn fetch_pr_closure_context(
    pr_number: i64,
    repo: Option<&str>,
) -> Result<ClosureContext, PrReadError> {
    use std::process::Command;
    let Some(gh) = gh_executable_for_merge_state() else {
        return Err(PrReadError::new("gh CLI not found on PATH", "availability"));
    };
    let repo = repo.map(|r| pr_link::canonical_repo(Path::new("."), r));
    let mut cmd = Command::new(gh);
    cmd.args(["pr", "view", &pr_number.to_string(), "--json"]);
    cmd.arg("number,body,url,state,mergedAt,files");
    if let Some(repo) = repo {
        cmd.args(["--repo", repo.as_str()]);
    }
    let out = crate::gh_budget::output(cmd)
        .map_err(|e| PrReadError::new(format!("gh subprocess failed to launch: {e}"), ""))?;
    if !out.status.success() {
        return Err(PrReadError::new(
            format!(
                "gh pr view #{pr_number} failed (rc={}): {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            "",
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return Err(PrReadError::new(
            format!("gh pr view #{pr_number} returned no output (exit 0)"),
            "",
        ));
    }
    let row: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| PrReadError::new(format!("gh stdout was not JSON: {e}"), "malformed"))?;
    let changed_files: Vec<String> = row
        .get("files")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|item| match item {
                    Value::String(path) => Some(path.clone()),
                    Value::Object(map) => {
                        map.get("path").and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                })
                .filter(|path| !path.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(ClosureContext {
        body: row
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: str_field(row.get("url")),
        state: row
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string(),
        changed_files,
    })
}

/// gh on PATH for this module's shellouts.
fn gh_executable_for_merge_state() -> Option<std::path::PathBuf> {
    crate::loop_dispatch::which_binary("gh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_aliased_recorded_repo_reads_the_new_location() {
        // The done_means probe: a PR url recorded under the dead
        // fnodotdev/fno-web name resolves through repo_aliases before the
        // gh call, so the close path reads the live repo instead of 404ing.
        let dir = tempfile::tempdir().unwrap();
        let slug = repo_slug_or_err(Some("fnodotdev/fno-web"), dir.path()).unwrap();
        assert_eq!(slug, "footnote-sh/fno-web");
        // An unknown slug passes through lowercased; an aliased slug is
        // idempotent, so an already-canonical read is unchanged.
        assert_eq!(
            repo_slug_or_err(Some("Some/Repo"), dir.path()).unwrap(),
            "some/repo"
        );
        assert_eq!(
            repo_slug_or_err(Some("footnote-sh/fno-web"), dir.path()).unwrap(),
            "footnote-sh/fno-web"
        );
    }

    #[test]
    fn fresh_read_refuses_a_state_outside_the_three_values() {
        let err = fresh_read(
            7,
            Some("o/r"),
            Path::new("."),
            false,
            |_, _, _| Ok(json!({"pr": 7, "state": "WIP"})),
            |_, _, _| unreachable!("files never read for a refused state"),
        )
        .expect_err("malformed state");
        assert_eq!(err.kind, "malformed");
        let msg = err.message.clone();
        assert!(msg.contains("malformed state"), "{msg}");
    }

    #[test]
    fn files_pagination_stops_on_the_short_page_and_fails_the_cap() {
        let full_page: Value = (0..FILES_PAGE_SIZE)
            .map(|i| json!({"filename": format!("f{i}.rs")}))
            .collect();
        let short: Value = (0..3)
            .map(|i| json!({"filename": format!("tail{i}.rs")}))
            .collect();
        let pages = [full_page.clone(), short];
        let mut calls = 0usize;
        let paths = paginate_files(|page| {
            calls += 1;
            Ok(pages.get(page - 1).cloned().unwrap_or_default())
        })
        .expect("paginated");
        assert_eq!(calls, 2, "stops on the first short page");
        assert_eq!(paths.len(), FILES_PAGE_SIZE + 3);
        let cap = paginate_files(|_| Ok(full_page.clone())).expect_err("cap fails closed");
        assert_eq!(cap.kind, "evidence_incomplete", "{}", cap.kind);
        let cap_msg = cap.message.clone();
        assert!(cap_msg.contains("3,000-file cap"), "{cap_msg}");
        // The commits loop shares the pagination contract: same short-page
        // stop, same cap, messages parsed once at the boundary.
        let commit_page: Value = (0..FILES_PAGE_SIZE)
            .map(|i| json!({"sha": format!("s{i}"), "commit": {"message": format!("m{i}")}}))
            .collect();
        let commit_tail: Value = (0..2)
            .map(|i| json!({"commit": {"message": format!("tail{i}")}}))
            .collect();
        let pages = [commit_page, commit_tail];
        let messages =
            paginate_messages(|page| Ok(pages.get(page - 1).cloned().unwrap_or_default()))
                .expect("paginated");
        assert_eq!(messages.len(), FILES_PAGE_SIZE + 2);
        assert_eq!(
            messages.get(FILES_PAGE_SIZE).map(String::as_str),
            Some("tail0")
        );
        let blank = json!({"commit": {"message": ""}});
        let messages = paginate_messages(|_| {
            Ok(json!([
                blank.clone(),
                json!({"commit": {"message": "kept"}})
            ]))
        })
        .expect("paginated");
        assert_eq!(messages, vec!["kept".to_string()], "blank message skipped");
    }
}
