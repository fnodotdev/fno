//! Fills the golden drafts and ships them in one batch PR, never one PR a day.
//!
//! `evals_drafts` writes a draft per typed correction with every answer field
//! empty. This pass hands the oldest open drafts to a bounded one-shot model
//! call. The daemon, not the model, decides what ships: a reply is kept only
//! when its YAML loads as a golden bank task and its own grade passes each
//! good fixture case and fails each bad one. A kept golden stages into
//! `<dir>/pending` and waits there; the batch PR on `evals/golden-drafts`
//! opens when `BATCH_SIZE` goldens are staged or the oldest has waited
//! `BATCH_MAX_AGE_DAYS` days, and while that PR is open every new batch
//! pushes onto it. A person still reviews and merges it.
//! `.drafted` in the drafts folder is the pass's memory: one line per handled
//! hash, so a `no_case` or unproved draft is never retried.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::evals::bank;
use crate::reader_route::ReaderRoute;

const DEFAULT_MAX_PER_DAY: usize = 5;
const SPAWN_TIMEOUT_S: u64 = 300;
const MODEL: &str = "sonnet";

/// The batch PR opens at this many staged goldens.
const BATCH_SIZE: usize = 10;
/// Or when the oldest staged golden has waited this many days.
const BATCH_MAX_AGE_DAYS: u64 = 14;
/// The rolling branch every batch PR uses.
const BATCH_BRANCH: &str = "evals/golden-drafts";

type Spawn<'a> =
    &'a dyn Fn(&str, &str, &Path, u64, &ReaderRoute) -> Result<(i32, String, String), String>;
type Open<'a> = &'a dyn Fn(&str, &str, &str, &[(PathBuf, Vec<u8>)]) -> Result<String, String>;

#[derive(Deserialize)]
struct Reply {
    verdict: String,
    #[serde(default)]
    bank: String,
    #[serde(default)]
    fixture: String,
    #[serde(default)]
    reason: String,
}

fn prompt(draft: &str) -> String {
    format!(
        "You turn one user correction into a golden eval task for this repository. Read docs/evals.md section Goldens first.\n\
Rules: the rule must be findable in the repo; the grade must be mechanical (file-exists or exit commands on files the worker writes); the prompt asks for recall of the exact action, never a choice among listed options; no `gh`.\n\
Draft:\n{draft}\n\
Reply with one JSON object and nothing else.\n\
A usable case: {{\"verdict\":\"case\",\"bank\":\"<full bank task YAML: id golden-<slug>, tier capability, prompt, grade, timeout_minutes, tags [golden], golden block with source, quote, expected, rule, failure_type, cost, outcome>\",\"fixture\":\"<metrics fixture YAML: cases list with at least 3 label good and 3 label bad, each {{id, label, files}}>\"}}\n\
No mechanical case: {{\"verdict\":\"no_case\",\"reason\":\"<one sentence>\"}}\n\
Do not invent a rule you cannot cite from a repo file."
    )
}

/// The first JSON object in a model reply.
fn parse_reply(out: &str) -> Result<Reply, String> {
    let (a, b) = (out.find('{'), out.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else {
        return Err("no JSON object in the reply".into());
    };
    serde_json::from_str(&out[a..=b]).map_err(|e| format!("reply is not the JSON asked for: {e}"))
}

/// Stage the reply beside a scratch bank and prove it. Returns the task id.
fn validate(repo: &Path, reply: &Reply) -> Result<String, String> {
    let stage = tempfile::tempdir().map_err(|e| e.to_string())?;
    let bank_dir = stage.path().join("evals").join("bank");
    let metrics = stage.path().join("evals").join("fixtures").join("metrics");
    for dir in [&bank_dir, &metrics] {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let probe = bank_dir.join("draft.yaml");
    std::fs::write(&probe, &reply.bank).map_err(|e| e.to_string())?;
    let task = bank::load_task(&probe).map_err(|e| format!("bank YAML refused: {}", e.0))?;
    let Some(golden) = task.golden.as_ref() else {
        return Err("no golden block".into());
    };
    let slug_ok = task
        .id
        .strip_prefix("golden-")
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if !slug_ok || task.tier != "capability" {
        return Err(format!(
            "id {:?} or tier is not a capability golden",
            task.id
        ));
    }
    if !task.tags.iter().any(|t| t == "golden") || task.tags.iter().any(|t| t == "gate") {
        return Err("tags must hold golden and never gate".into());
    }
    if repo
        .join("evals")
        .join("bank")
        .join(format!("{}.yaml", task.id))
        .exists()
    {
        return Err(format!("{} already exists in the bank", task.id));
    }
    let catalog = crate::evals::failure_types::Catalog::load(&repo.join("evals").join("bank"))?;
    if !catalog.types.iter().any(|t| t.id == golden.failure_type) {
        return Err(format!(
            "failure_type {:?} is not in the catalog",
            golden.failure_type
        ));
    }
    std::fs::write(bank_dir.join(format!("{}.yaml", task.id)), &reply.bank)
        .map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())?;
    std::fs::write(metrics.join(format!("{}.yaml", task.id)), &reply.fixture)
        .map_err(|e| e.to_string())?;
    let failures = crate::evals::failure_types::golden_failures(&task, &bank_dir);
    if failures.is_empty() {
        Ok(task.id)
    } else {
        Err(format!("grade unproved: {}", failures.join("; ")))
    }
}

fn open_drafts(dir: &Path) -> Vec<(String, PathBuf)> {
    let handled = std::fs::read_to_string(dir.join(".drafted")).unwrap_or_default();
    let done: Vec<&str> = handled
        .lines()
        .filter_map(|l| l.split('\t').next())
        .collect();
    let mut found: Vec<(std::time::SystemTime, String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .filter_map(|p| {
            let hash = p.file_stem()?.to_string_lossy().into_owned();
            let at = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            (!done.contains(&hash.as_str())).then_some((at, hash, p))
        })
        .collect();
    // Oldest first: the 30-day expiry takes the oldest drafts.
    found.sort();
    found.into_iter().map(|(_, h, p)| (h, p)).collect()
}

fn record(dir: &Path, hash: &str, note: &str, draft: &Path) {
    use std::io::Write as _;
    let line = format!("{hash}\t{}\n", note.replace(['\n', '\t'], " "));
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(".drafted"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
    let _ = std::fs::remove_file(draft);
}

pub(crate) fn max_per_day(repo: &Path) -> usize {
    match crate::agents_config::config_value_deep(repo, &["evals", "drafter", "max_per_day"]) {
        Some(toml::Value::Integer(n)) if n >= 0 => n as usize,
        _ => DEFAULT_MAX_PER_DAY,
    }
}

/// Stage a kept golden for the next batch: bank bytes under
/// `<dir>/pending/bank/<id>.yaml`, fixture bytes under
/// `<dir>/pending/fixtures/<id>.yaml`. Their mtimes are the kept-at
/// timestamps, and `create_new` refuses a duplicate staged id so a second
/// keep can never silently overwrite the first.
fn stage(dir: &Path, id: &str, bank: &str, fixture: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let bank_dir = dir.join("pending").join("bank");
    let fix_dir = dir.join("pending").join("fixtures");
    std::fs::create_dir_all(&bank_dir)?;
    std::fs::create_dir_all(&fix_dir)?;
    let bank_path = bank_dir.join(format!("{id}.yaml"));
    let fix_path = fix_dir.join(format!("{id}.yaml"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&bank_path)?;
    f.write_all(bank.as_bytes())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&fix_path)?;
    f.write_all(fixture.as_bytes())?;
    Ok(())
}

/// The staged batch: (file name, kept-at) pairs, oldest keep first.
fn pending_files(dir: &Path) -> Vec<(String, std::time::SystemTime)> {
    let bank = dir.join("pending").join("bank");
    let mut found: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(&bank)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let at = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((at, name))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(at, name)| (name, at)).collect()
}

/// True when the oldest staged golden has waited BATCH_MAX_AGE_DAYS.
fn pending_stale(pending: &[(String, std::time::SystemTime)], now: std::time::SystemTime) -> bool {
    let Some((_, oldest)) = pending.first() else {
        return false;
    };
    let bound = std::time::Duration::from_secs(BATCH_MAX_AGE_DAYS * 24 * 60 * 60);
    now.duration_since(*oldest)
        .map(|d| d >= bound)
        .unwrap_or(false)
}

/// One daily pass. Kept goldens stage into `<dir>/pending` and ship in
/// batches through `open` (bank_pr::ship): the batch PR opens at BATCH_SIZE
/// staged goldens or when the oldest has waited BATCH_MAX_AGE_DAYS days,
/// and an open batch PR takes every new batch as a push. Returns the counts
/// for the log.
pub(crate) fn fill(
    dir: &Path,
    repo: &Path,
    now: std::time::SystemTime,
    max: usize,
    spawn: Spawn,
    open: Open,
) -> Result<Value, String> {
    let route = crate::reader_route::resolve(repo, "golden-drafter", None, MODEL);
    let (mut kept, mut dropped) = (Vec::new(), 0u64);
    for (hash, path) in open_drafts(dir).into_iter().take(max) {
        let draft = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let reply = spawn(
            &format!("golden-drafter-{hash}"),
            &prompt(&draft),
            repo,
            SPAWN_TIMEOUT_S,
            &route,
        )
        .and_then(|(code, out, err)| {
            if code == 0 {
                parse_reply(&out)
            } else {
                Err(format!("spawn exit {code}: {}", err.trim()))
            }
        });
        let reply = match reply {
            Ok(r) => r,
            // A failed call is not a verdict on the case: leave it for tomorrow.
            Err(e) => {
                eprintln!("golden drafter: {hash}: {e}");
                continue;
            }
        };
        if reply.verdict != "case" {
            record(dir, &hash, &format!("no_case: {}", reply.reason), &path);
            dropped += 1;
            continue;
        }
        match validate(repo, &reply) {
            Ok(id) => match stage(dir, &id, &reply.bank, &reply.fixture) {
                Ok(()) => {
                    record(dir, &hash, &format!("kept {id}"), &path);
                    kept.push(id);
                }
                // The id is already staged from another draft and can never
                // ship twice: refuse this one instead of retrying it daily.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    record(
                        dir,
                        &hash,
                        &format!("refused: {id} is already staged"),
                        &path,
                    );
                    dropped += 1;
                }
                Err(e) => return Err(format!("stage {id}: {e}")),
            },
            Err(why) => {
                let note = if why.starts_with("grade unproved") {
                    format!("grade_unproved: {why}")
                } else {
                    format!("refused: {why}")
                };
                record(dir, &hash, &note, &path);
                dropped += 1;
            }
        }
    }
    let pending = pending_files(dir);
    if pending.is_empty() {
        return Ok(json!({"kept": kept.len(), "dropped": dropped}));
    }
    let mut edits: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for (name, _) in &pending {
        let bank = dir.join("pending").join("bank").join(name);
        let fix = dir.join("pending").join("fixtures").join(name);
        let bytes = std::fs::read(&bank).map_err(|e| format!("pending {}: {e}", bank.display()))?;
        let fix_bytes =
            std::fs::read(&fix).map_err(|e| format!("pending {}: {e}", fix.display()))?;
        edits.push((PathBuf::from(format!("evals/bank/{name}")), bytes));
        edits.push((
            PathBuf::from(format!("evals/fixtures/metrics/{name}")),
            fix_bytes,
        ));
    }
    let title = "Golden eval tasks drafted from corrections";
    let list: String = pending
        .iter()
        .map(|(name, _)| format!("- `{}`\n", name.trim_end_matches(".yaml")))
        .collect();
    let body = format!("The daemon drafted these from typed user corrections. Each loaded as a golden, passed its 3 good fixture cases and failed its 3 bad ones. Read the prompt, the rule and the grade before you merge.\n\n{list}");
    // Below threshold with nothing stale: this pass only stages, and an open
    // batch PR takes the batch only at the next one.
    if pending.len() < BATCH_SIZE && !pending_stale(&pending, now) {
        return Ok(json!({
            "kept": kept.len(),
            "dropped": dropped,
            "waiting": pending.len(),
        }));
    }
    let url = open(BATCH_BRANCH, title, &body, &edits)?;
    // Shipped: clear the stage. A refusal above leaves it intact for the
    // next pass.
    for (name, _) in &pending {
        let _ = std::fs::remove_file(dir.join("pending").join("bank").join(name));
        let _ = std::fs::remove_file(dir.join("pending").join("fixtures").join(name));
    }
    Ok(json!({"kept": kept.len(), "dropped": dropped, "pr": url}))
}

/// The bank checkout the PR is cut from, or None when no registered project
/// carries one.
pub(crate) fn bank_repo() -> Option<PathBuf> {
    let has_bank = |p: &PathBuf| p.join("evals").join("bank").is_dir();
    crate::provider::plugin_root()
        .filter(|r| r.join(".git").exists())
        .into_iter()
        .chain(crate::backlog::settings::project_roots())
        .find(has_bank)
}

/// The golden drafter's one-shot: routed through the reader route (profile
/// `golden-drafter`) with the harness's tool-bearing form in the fence.
fn spawn_reader(
    name: &str,
    prompt: &str,
    cwd: &Path,
    timeout: u64,
    route: &ReaderRoute,
) -> Result<(i32, String, String), String> {
    let argv = crate::reader_route::reader_tools_argv(name, prompt, cwd, timeout, route, &[])
        .map_err(|e| e.to_string())?;
    crate::blueprint_judge::headless_spawn(&argv, timeout)
}

/// The daemon's daily entry, called after the drafts pass.
pub(crate) fn daily(dir: &Path) {
    let Some(repo) = bank_repo() else { return };
    let open = |branch: &str, title: &str, body: &str, edits: &[(PathBuf, Vec<u8>)]| {
        crate::bank_pr::ship(&repo, branch, title, body, edits).map_err(|e| e.to_string())
    };
    match fill(
        dir,
        &repo,
        std::time::SystemTime::now(),
        max_per_day(&repo),
        &spawn_reader,
        &open,
    ) {
        Ok(v) => eprintln!("golden drafter: {v}"),
        Err(e) => eprintln!("golden drafter: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const BANK: &str = "id: golden-demo\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: reply.md\n  - kind: exit\n    command: \"grep -q yes reply.md\"\ntimeout_minutes: 5\ntags: [golden]\ngolden:\n  source: {kind: correction, ref: \"c:1\"}\n  quote: q\n  expected: e\n  rule: r\n  failure_type: evasive-answer\n  cost: trust\n  outcome: failed\n";

    fn fixture(bad_passes: bool) -> String {
        let bad = if bad_passes { "yes" } else { "nope" };
        format!("cases:\n  - {{id: g1, label: good, files: {{\"reply.md\": \"yes\\n\"}}}}\n  - {{id: g2, label: good, files: {{\"reply.md\": \"yes 2\\n\"}}}}\n  - {{id: g3, label: good, files: {{\"reply.md\": \"yes 3\\n\"}}}}\n  - {{id: b1, label: bad, files: {{\"reply.md\": \"{bad}\\n\"}}}}\n  - {{id: b2, label: bad, files: {{\"reply.md\": \"no\\n\"}}}}\n  - {{id: b3, label: bad, files: {{}}}}\n")
    }

    fn repo_with_catalog() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let bank_dir = repo.path().join("evals").join("bank");
        std::fs::create_dir_all(&bank_dir).unwrap();
        std::fs::write(
            bank_dir.join("failure-types.yaml"),
            "cost_order: [trust]\ntypes:\n  - {id: evasive-answer, cost: trust, description: d}\n",
        )
        .unwrap();
        repo
    }

    /// Stage `id` into the drafts dir's pending batch without a model call.
    fn stage_fake(dir: &Path, id: &str) {
        let bank = dir.join("pending").join("bank");
        let fix = dir.join("pending").join("fixtures");
        std::fs::create_dir_all(&bank).unwrap();
        std::fs::create_dir_all(&fix).unwrap();
        std::fs::write(bank.join(format!("{id}.yaml")), "bank-yaml").unwrap();
        std::fs::write(fix.join(format!("{id}.yaml")), "fixture-yaml").unwrap();
    }

    #[test]
    fn kept_waits_below_threshold_and_stages() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        for h in ["aaa", "bbb", "ccc"] {
            std::fs::write(dir.path().join(format!("{h}.yaml")), "id: x\n").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let calls = RefCell::new(0);
        let spawn = |name: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| {
            *calls.borrow_mut() += 1;
            let reply = match name {
                "golden-drafter-aaa" => {
                    json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)})
                }
                "golden-drafter-bbb" => json!({"verdict": "no_case", "reason": "no repo rule"}),
                _ => {
                    json!({"verdict": "case", "bank": BANK.replace("golden-demo", "golden-demo2"), "fixture": fixture(true)})
                }
            };
            Ok((0, format!("sure: {reply}"), String::new()))
        };
        let opens = RefCell::new(0);
        let open = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| {
            *opens.borrow_mut() += 1;
            Ok("https://example/pr/1".to_string())
        };
        let out = fill(
            dir.path(),
            repo.path(),
            std::time::SystemTime::now(),
            5,
            &spawn,
            &open,
        )
        .unwrap();
        assert_eq!(out["kept"], 1, "{out}");
        assert_eq!(out["dropped"], 2, "{out}");
        assert!(out["pr"].is_null(), "{out}");
        assert_eq!(*opens.borrow(), 0, "below threshold opens nothing");
        assert_eq!(*calls.borrow(), 3);
        let handled = std::fs::read_to_string(dir.path().join(".drafted")).unwrap();
        assert_eq!(handled.lines().count(), 3, "{handled}");
        assert!(handled.contains("aaa\tkept golden-demo"), "{handled}");
        assert!(handled.contains("bbb\tno_case"), "{handled}");
        assert!(handled.contains("ccc\tgrade_unproved"), "{handled}");
        assert!(open_drafts(dir.path()).is_empty());
        assert!(dir
            .path()
            .join("pending")
            .join("bank")
            .join("golden-demo.yaml")
            .exists());
    }

    #[test]
    fn batch_opens_at_threshold() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=9 {
            stage_fake(dir.path(), &format!("golden-f{i}"));
        }
        std::fs::write(dir.path().join("aaa.yaml"), "id: x\n").unwrap();
        let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| {
            let reply = json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)});
            Ok((0, format!("sure: {reply}"), String::new()))
        };
        let edits_seen: RefCell<Vec<(PathBuf, Vec<u8>)>> = RefCell::new(Vec::new());
        let open = |_: &str, _: &str, _: &str, edits: &[(PathBuf, Vec<u8>)]| {
            *edits_seen.borrow_mut() = edits.to_vec();
            Ok("https://example/pr/2".to_string())
        };
        let out = fill(
            dir.path(),
            repo.path(),
            std::time::SystemTime::now(),
            5,
            &spawn,
            &open,
        )
        .unwrap();
        assert_eq!(out["kept"], 1, "{out}");
        assert_eq!(out["pr"], "https://example/pr/2", "{out}");
        let edits = edits_seen.into_inner();
        assert_eq!(edits.len(), 20);
        let paths: Vec<String> = edits.iter().map(|(p, _)| p.display().to_string()).collect();
        let ids: Vec<String> = (1..=9)
            .map(|i| format!("golden-f{i}"))
            .chain(["golden-demo".to_string()])
            .collect();
        for id in &ids {
            assert!(
                paths.contains(&format!("evals/bank/{id}.yaml")),
                "{id}: {paths:?}"
            );
            assert!(
                paths.contains(&format!("evals/fixtures/metrics/{id}.yaml")),
                "{id}: {paths:?}"
            );
        }
        let demo = edits
            .iter()
            .find(|(p, _)| p.display().to_string() == "evals/bank/golden-demo.yaml")
            .unwrap();
        assert_eq!(demo.1, BANK.as_bytes());
        assert!(pending_files(dir.path()).is_empty());
        assert_eq!(
            std::fs::read_dir(dir.path().join("pending").join("fixtures"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn waits_below_threshold_with_staged_goldens() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        stage_fake(dir.path(), "golden-f1");
        stage_fake(dir.path(), "golden-f2");
        let spawn =
            |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| panic!("no drafts queued");
        let opens = RefCell::new(0);
        let open = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| {
            *opens.borrow_mut() += 1;
            Ok("https://example/pr/3".to_string())
        };
        let out = fill(
            dir.path(),
            repo.path(),
            std::time::SystemTime::now(),
            0,
            &spawn,
            &open,
        )
        .unwrap();
        assert_eq!(out["waiting"], 2, "{out}");
        assert_eq!(out["kept"], 0, "{out}");
        assert!(out["pr"].is_null(), "{out}");
        assert_eq!(*opens.borrow(), 0);
        assert_eq!(pending_files(dir.path()).len(), 2);
    }

    #[test]
    fn batch_opens_when_oldest_is_14_days_old() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        for id in ["golden-f1", "golden-f2", "golden-f3"] {
            stage_fake(dir.path(), id);
        }
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(15 * 24 * 60 * 60);
        let staged = dir
            .path()
            .join("pending")
            .join("bank")
            .join("golden-f1.yaml");
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&staged)
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let spawn =
            |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| panic!("no drafts queued");
        let edits_seen: RefCell<Vec<(PathBuf, Vec<u8>)>> = RefCell::new(Vec::new());
        let open = |_: &str, _: &str, _: &str, edits: &[(PathBuf, Vec<u8>)]| {
            *edits_seen.borrow_mut() = edits.to_vec();
            Ok("https://example/pr/4".to_string())
        };
        let out = fill(
            dir.path(),
            repo.path(),
            std::time::SystemTime::now(),
            0,
            &spawn,
            &open,
        )
        .unwrap();
        assert_eq!(out["pr"], "https://example/pr/4", "{out}");
        let edits = edits_seen.into_inner();
        assert_eq!(edits.len(), 6);
        let paths: Vec<String> = edits.iter().map(|(p, _)| p.display().to_string()).collect();
        for id in ["golden-f1", "golden-f2", "golden-f3"] {
            assert!(
                paths.contains(&format!("evals/bank/{id}.yaml")),
                "{id}: {paths:?}"
            );
            assert!(
                paths.contains(&format!("evals/fixtures/metrics/{id}.yaml")),
                "{id}: {paths:?}"
            );
        }
        assert!(pending_files(dir.path()).is_empty());
        assert_eq!(
            std::fs::read_dir(dir.path().join("pending").join("fixtures"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn failed_ship_leaves_pending_for_retry() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=10 {
            stage_fake(dir.path(), &format!("golden-f{i}"));
        }
        std::fs::write(dir.path().join("aaa.yaml"), "id: x\n").unwrap();
        let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &ReaderRoute| {
            let reply = json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)});
            Ok((0, format!("sure: {reply}"), String::new()))
        };
        let fail = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| Err("gh down".to_string());
        let out = fill(
            dir.path(),
            repo.path(),
            std::time::SystemTime::now(),
            5,
            &spawn,
            &fail,
        );
        assert!(out.is_err(), "{out:?}");
        assert_eq!(pending_files(dir.path()).len(), 11);
        assert_eq!(
            std::fs::read_dir(dir.path().join("pending").join("fixtures"))
                .unwrap()
                .count(),
            11
        );
        let handled = std::fs::read_to_string(dir.path().join(".drafted")).unwrap();
        assert!(handled.contains("aaa\tkept golden-demo"), "{handled}");
        let open = |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| {
            Ok("https://example/pr/5".to_string())
        };
        let out = fill(
            dir.path(),
            repo.path(),
            std::time::SystemTime::now(),
            0,
            &spawn,
            &open,
        )
        .unwrap();
        assert_eq!(out["pr"], "https://example/pr/5", "{out}");
        assert!(pending_files(dir.path()).is_empty());
    }
}
