//! The bank task runner: disposable worktree -> optional worker -> mechanical
//! grade (the deleted Python leg's runner.py, ported). One run = one task
//! executed once; --repeat K is K runs of the same task, each in its own
//! fresh disposable worktree. A bank task never executes in the user's
//! working copy. The worker step routes through `fno agent spawn --once`
//! so provider rotation and the spawn cap stay in play.

use crate::harness::Harness;
use crate::pid::Pid;
use serde_json::json;

/// The worker spawn's answer.
pub struct SpawnResult {
    pub ok: bool,
    pub reason: String,
    pub worker_name: String,
    pub timed_out: bool,
    pub lane_note: String,
}

/// One attempt's outcome and its history-row evidence.
pub struct RunResult {
    pub task_id: String,
    pub tier: String,
    pub passed: bool,
    pub reason: String,
    pub duration_s: f64,
    pub repeat_index: u32,
    pub variant: String,
    pub attempt_index: u32,
    pub attempt_id: serde_json::Value,
    pub status: String,
}

/// Variant must match baseline|v[1-9][0-9]*.
pub fn variant_ok(variant: &str) -> bool {
    if variant == "baseline" {
        return true;
    }
    let Some(digits) = variant.strip_prefix('v') else {
        return false;
    };
    !digits.is_empty()
        && digits.starts_with(|c: char| c.is_ascii_digit() && c != '0')
        && digits.chars().all(|c| c.is_ascii_digit())
}

/// Resolve whether the headless grading-worker spawn is armed:
/// config.autonomy.enabled gates first (the wave-3 master switch), then
/// config.evals.enabled (default true; an unreadable config reads OFF).
pub fn evals_enabled() -> bool {
    if !crate::backlog::advance_settings::autonomy_master_enabled(None) {
        return false;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match crate::paths_cli::cfg_str(&cwd, &["evals", "enabled"]) {
        Some(v) => v == "true",
        None => true,
    }
}

/// One evals run per machine: the run slot is an exclusive std file lock on
/// `<history dir>/evals-gate/run.lock`, held for the process lifetime. A
/// second run queues here instead of stacking its fixture git steps on the
/// first's, which drove the gate machine to load 82-92 on 16 cores when the
/// head and main runs overlapped. The kernel frees the slot when the holder
/// dies, so no TTL applies.
pub struct RunSlot {
    file: std::fs::File,
    #[allow(dead_code)]
    path: std::path::PathBuf,
}

pub(crate) fn run_slot_path() -> std::path::PathBuf {
    let history = super::history_path();
    history
        .parent()
        .map(|p| p.join("evals-gate"))
        .unwrap_or_else(|| history.clone())
        .join("run.lock")
}

fn slot_holder_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

/// Take the machine's evals run slot, queueing behind the holder if one
/// lives. Held until the returned slot drops, at `run_command` scope.
pub(crate) fn acquire_run_slot() -> RunSlot {
    let path = run_slot_path();
    if let Some(slot) = RunSlot::try_acquire(&path) {
        return slot;
    }
    RunSlot::acquire_blocking(&path)
}

fn stamp_run_slot(file: &mut std::fs::File) {
    use std::io::Write as _;
    let _ = file.set_len(0);
    let _ = std::io::Write::write_all(&mut *file, format!("{}\n", std::process::id()).as_bytes());
    let _ = std::io::Write::flush(file);
}

impl RunSlot {
    /// Take the slot if it is free; None when another run holds it.
    fn try_acquire(path: &std::path::Path) -> Option<RunSlot> {
        std::fs::create_dir_all(path.parent()?).ok()?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(path)
            .ok()?;
        // try_lock: a held slot reads as "someone else is running", never as
        // a hang; the blocking path below is the queue.
        if file.try_lock().is_err() {
            return None;
        }
        stamp_run_slot(&mut file);
        Some(RunSlot {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Block until the holder exits, naming it first so a queued run reads
    /// in the log as queued, never as a hang. `File::lock` retries EINTR
    /// internally, so a signal delivered to the process cannot wedge it.
    fn acquire_blocking(path: &std::path::Path) -> RunSlot {
        let holder = slot_holder_pid(path)
            .map(|p| p.to_string())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("machine evals slot held by pid {holder}; queuing");
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let _ = std::fs::create_dir_all(dir);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(path)
            .expect("run.lock open");
        if let Err(e) = file.lock() {
            panic!("run.lock lock failed: {e}");
        }
        stamp_run_slot(&mut file);
        eprintln!("machine evals slot acquired");
        RunSlot {
            file,
            path: path.to_path_buf(),
        }
    }
}

/// The run's checkout ref resolved to a sha; a missing ref grades as a
/// failure, never a crash.
fn git_rev(repo_root: &std::path::Path, ref_name: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", ref_name])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// A live owner is a concurrent run; its tree is in use, not orphaned.
/// Tree names end in -<pid>-<ms>.
fn owner_alive(worktree: &str) -> bool {
    let name = std::path::Path::new(worktree)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut parts: Vec<&str> = name.rsplit('-').collect();
    parts.reverse();
    // [<tag words...>, pid, ms]
    if parts.len() < 3 {
        return false;
    }
    match parts
        .iter()
        .rev()
        .nth(1)
        .and_then(|s| s.parse::<i64>().ok())
    {
        Some(pid) => Pid::try_from(pid).map(|p| p.exists()).unwrap_or(false),
        None => false,
    }
}

/// The disposable eval worktree base, honoring the worktree-base override.
fn evals_worktree_base() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    crate::paths_cli::worktrees_base(&cwd).join("evals")
}

/// One fresh disposable worktree: clean detached checkout at *ref* under
/// the evals base, with an untracked in-flight marker so a concurrent
/// merged-cleanup sweep reads it as a reap candidate and holds.
fn make_disposable_worktree(
    repo_root: &std::path::Path,
    ref_name: &str,
    tag: &str,
) -> Result<std::path::PathBuf, String> {
    let _git_step = git_step_lock();
    let base = evals_worktree_base();
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let pid = std::process::id();
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("{tag}-{pid}-{millis}");
    let path = base.join(name);
    let out = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&path)
        .arg(ref_name)
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    std::fs::write(path.join(".fno-evals-tree"), "in-flight eval worktree\n")
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// The fixture tree a golden prompt names, carried onto an older checkout. Only
/// this tree moves: `evals/bank` holds each golden's expected answer, and a
/// worker told to read the repo would read it.
const OVERLAY_PATH: &str = "evals/fixtures/golden";

/// Commit `overlay_ref`'s fixtures on top of the checkout, so the worker sees a
/// clean tree. Returns the new head, the base a grader diffs against.
fn apply_overlay(dir: &std::path::Path, overlay_ref: &str) -> Result<String, String> {
    git_in(dir, &["checkout", overlay_ref, "--", OVERLAY_PATH])?;
    commit_all(dir, "evals: overlay golden fixtures")
}

/// Serialises this process's fixture git steps across `--workers` threads:
/// the sealed-tree `git add -A -f` and commits are CPU-heavy, and running
/// them N threads wide stacks load the machine run slot already confines
/// to one process. Poison-tolerant: a panicking worker must not wedge the
/// others' prep forever.
static GIT_STEP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn git_step_lock() -> std::sync::MutexGuard<'static, ()> {
    GIT_STEP_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let _git_step = git_step_lock();
    // A run started from a git hook inherits these, and they would turn this
    // `git add` and commit on the real repo.
    let out = std::process::Command::new("git")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Each path holds a golden's answer. Workers told to read the repo did read
/// them, so a golden's tree leaves them out. The loaded task keeps its grader
/// in memory.
const ANSWER_KEY_PATHS: [&str; 2] = ["evals/bank", "evals/fixtures/metrics"];

/// A skill's golden tasks sit beside it in `skills/<name>/evals/*.yaml`, so they
/// are answer keys too. Case directories deeper in `evals/` stay.
fn remove_skill_task_files(tree: &std::path::Path) {
    let Ok(skills) = std::fs::read_dir(tree.join("skills")) else {
        return;
    };
    for skill in skills.flatten() {
        let Ok(files) = std::fs::read_dir(skill.path().join("evals")) else {
            continue;
        };
        for f in files.flatten().map(|f| f.path()) {
            if f.is_file() && f.extension().is_some_and(|e| e == "yaml") {
                let _ = std::fs::remove_file(f);
            }
        }
    }
}

/// Where sealed trees live: the temp dir, so no `$HOME` file sits on the
/// worker's ancestor path. None of it may sit inside `$HOME`, which the fence
/// denies as a whole.
pub(crate) fn sealed_base(
    temp: &std::path::Path,
    home: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let base = temp.join("fno-evals");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let base = base.canonicalize().map_err(|e| e.to_string())?;
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    if base.starts_with(&home) {
        return Err(format!(
            "sealed tree base {} sits inside the home dir {}",
            base.display(),
            home.display()
        ));
    }
    Ok(base)
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// Write `ref_name`'s files (or only `paths` of them) into `dest`.
pub(super) fn extract_ref(
    repo_root: &std::path::Path,
    ref_name: &str,
    paths: &[&str],
    dest: &std::path::Path,
) -> Result<(), String> {
    let _git_step = git_step_lock();
    use std::process::{Command, Stdio};
    let mut archive = Command::new("git")
        .args(["archive", "--format=tar", ref_name])
        .args(paths)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = archive.stdout.take().ok_or("git archive gave no stdout")?;
    let tar = Command::new("tar")
        .args(["-xf", "-", "-C"])
        .arg(dest)
        .stdin(stdout)
        .output()
        .map_err(|e| e.to_string())?;
    let archived = archive.wait_with_output().map_err(|e| e.to_string())?;
    if !archived.status.success() {
        return Err(String::from_utf8_lossy(&archived.stderr).trim().to_string());
    }
    if !tar.status.success() {
        return Err(String::from_utf8_lossy(&tar.stderr).trim().to_string());
    }
    Ok(())
}

/// The paths a golden worker reads as instructions. A work-product task keeps
/// its fixture and takes only these from `--ref`, so a skill or rule change
/// varies the tree while the code to fix stays fixed.
const RULES_PATHS: [&str; 8] = [
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    ".claude",
    "skills",
    "agents",
    "commands",
    "hooks",
];

/// Lay `rules_ref`'s rules paths over the tree. A path the ref lacks is skipped.
fn apply_rules(
    repo_root: &std::path::Path,
    rules_ref: &str,
    dir: &std::path::Path,
) -> Result<(), String> {
    let listed = git_in(
        repo_root,
        &[
            ["ls-tree", "--name-only", rules_ref, "--"].as_slice(),
            RULES_PATHS.as_slice(),
        ]
        .concat(),
    )?;
    let present: Vec<&str> = listed.lines().collect();
    for path in &present {
        let _ = std::fs::remove_dir_all(dir.join(path));
        let _ = std::fs::remove_file(dir.join(path));
    }
    if present.is_empty() {
        return Ok(());
    }
    extract_ref(repo_root, rules_ref, &present, dir)
}

/// A golden's tree: `ref_name`'s files as one commit in a new repo. A worktree
/// names the real repo in its `.git` file, and any parent commit holds the
/// rules and the answer keys one `git show` away, so this tree has neither.
/// Returns the path and its head, the base a grader diffs against.
pub(crate) fn make_sealed_tree(
    repo_root: &std::path::Path,
    ref_name: &str,
    overlay_ref: Option<&str>,
    rules_ref: Option<&str>,
    tag: &str,
    base: &std::path::Path,
) -> Result<(std::path::PathBuf, String), String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = base.join(format!("{tag}-{}-{millis}", std::process::id()));
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let built = fill_sealed_tree(repo_root, ref_name, overlay_ref, rules_ref, &path);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&path);
    }
    built.map(|head| (path, head))
}

fn fill_sealed_tree(
    repo_root: &std::path::Path,
    ref_name: &str,
    overlay_ref: Option<&str>,
    rules_ref: Option<&str>,
    dir: &std::path::Path,
) -> Result<String, String> {
    extract_ref(repo_root, ref_name, &[], dir)?;
    if let Some(rules) = rules_ref {
        apply_rules(repo_root, rules, dir)?;
    }
    if let Some(overlay) = overlay_ref {
        let _ = std::fs::remove_dir_all(dir.join(OVERLAY_PATH));
        extract_ref(repo_root, overlay, &[OVERLAY_PATH], dir)?;
    }
    for key in ANSWER_KEY_PATHS {
        let _ = std::fs::remove_dir_all(dir.join(key));
    }
    remove_skill_task_files(dir);
    // An empty template: a user's template can carry hooks that name real paths.
    git_in(dir, &["init", "-q", "--template="])?;
    // The fence denies the user's git config, so the worker's git reads these.
    for (key, value) in [
        ("user.name", "fno-evals"),
        ("user.email", "evals@fno.invalid"),
        ("core.hooksPath", "/dev/null"),
        ("core.excludesFile", "/dev/null"),
        ("core.attributesFile", "/dev/null"),
        ("commit.gpgsign", "false"),
    ] {
        git_in(dir, &["config", key, value])?;
    }
    git_in(dir, &["add", "-A", "-f"])?;
    commit_all(dir, "evals: golden tree")
}

fn commit_all(dir: &std::path::Path, message: &str) -> Result<String, String> {
    git_in(
        dir,
        &[
            "-c",
            "user.name=fno-evals",
            "-c",
            "user.email=evals@fno.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            message,
        ],
    )?;
    git_in(dir, &["rev-parse", "HEAD"])
}

fn remove_worktree(repo_root: &std::path::Path, path: &std::path::Path) {
    let _git_step = git_step_lock();
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .current_dir(repo_root)
        .output();
    let _ = std::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_root)
        .output();
}

/// Every trial worktree is a fresh cwd, so claude keeps one projects dir
/// per trial and they pile up in its session list; the history row is the
/// record. Best-effort: the cleanup never masks the run's verdict.
fn forget_trial_transcript(workdir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(trial_transcript_dir(workdir));
}

fn trial_transcript_dir(workdir: &std::path::Path) -> std::path::PathBuf {
    crate::claude_drive::claude_projects_dir().join(crate::claude_ask::claude_cwd_slug(workdir))
}

/// A failed trial keeps its transcript, the only record of why it failed.
const KEPT_TRANSCRIPT_DAYS: u64 = 14;

/// Drop kept trial transcripts past their age. The evals base slug prefixes a
/// trial's dir, so a real session's transcripts never match.
fn prune_kept_transcripts(projects: &std::path::Path, base_slug: &str, max_age_days: u64) {
    let Ok(entries) = std::fs::read_dir(projects) else {
        return;
    };
    let max_age = std::time::Duration::from_secs(max_age_days * 86_400);
    let prefix = format!("{base_slug}-");
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old && entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Prune leftover eval worktrees whose owning run is gone. Best-effort; a
/// sweep failure never blocks a fresh run.
pub fn sweep_orphans(repo_root: &std::path::Path) -> usize {
    prune_kept_transcripts(
        &crate::claude_drive::claude_projects_dir(),
        &crate::claude_ask::claude_cwd_slug(&evals_worktree_base()),
        KEPT_TRANSCRIPT_DAYS,
    );
    let swept = sealed_base(&std::env::temp_dir(), &home_dir())
        .map(|base| {
            prune_kept_transcripts(
                &crate::claude_drive::claude_projects_dir(),
                &crate::claude_ask::claude_cwd_slug(&base),
                KEPT_TRANSCRIPT_DAYS,
            );
            sweep_sealed_trees(&base)
        })
        .unwrap_or(0);
    swept + sweep_worktrees(repo_root)
}

/// A sealed tree is not a registered worktree, so `git worktree list` never
/// names it. Its name carries the owner, the same as a worktree's.
fn sweep_sealed_trees(base: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if !owner_alive(&entry.path().display().to_string())
            && std::fs::remove_dir_all(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

fn sweep_worktrees(repo_root: &std::path::Path) -> usize {
    let Ok(out) = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
    else {
        return 0;
    };
    let evals_base = evals_worktree_base();
    let mut removed = 0usize;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(wt) = line.strip_prefix("worktree ") else {
            continue;
        };
        if !wt.contains(&evals_base.display().to_string()) {
            continue;
        }
        if owner_alive(wt) {
            continue;
        }
        remove_worktree(repo_root, std::path::Path::new(wt));
        removed += 1;
    }
    removed
}

/// A claude worker's flags that make the tree under test its only source of
/// rules. Without them it loads the installed plugin, the user's settings and
/// their MCP servers, so a cohort measures today's machine and not the tree.
/// The tree's own plugin stays off too: its hooks read the live fleet, and the
/// worker then acts on that state. Only claude has these flags, so any other
/// harness runs with today's plugin context and no read fence.
fn tree_only_flags(
    harness: Option<&str>,
    tree: &std::path::Path,
    home: &std::path::Path,
) -> Vec<String> {
    if harness.map(Harness::parse) != Some(Harness::Claude) {
        return Vec::new();
    }
    vec![
        "--setting-sources".into(),
        "project".into(),
        "--strict-mcp-config".into(),
        format!("--settings={}", read_fence(tree, home)),
    ]
}

/// The file tools stop at the block and the deny rules, and Bash stops at the
/// OS sandbox. The block alone leaves `cat` and `git` free, and a canary read
/// `~/.claude` and `~/.fno` past it, so the deny rules take all of home. The
/// sealed base is denied because a sibling trial's tree holds its
/// `decision.md`.
fn read_fence(tree: &std::path::Path, home: &std::path::Path) -> serde_json::Value {
    let base = tree.parent().unwrap_or(tree);
    json!({
        "permissions": {
            "blockReadsOutsideWorkingDirectories": true,
            "deny": ["WebFetch", "WebSearch", "Read(~/**)", "Edit(~/**)"],
        },
        "sandbox": {
            "enabled": true,
            "allowUnsandboxedCommands": false,
            "failIfUnavailable": true,
            "network": {"strictAllowlist": true, "allowedDomains": []},
            "filesystem": {
                "denyRead": [home, base],
                "allowRead": [tree],
            },
        },
    })
}

/// The env of a fenced worker. Auto memory reads the user's files. The seal
/// drops the spawn's state grants. Git and ripgrep would fail on the user's
/// config files, which the fence denies, so git reads only the tree's own and
/// ripgrep reads none.
const SEALED_WORKER_ENV: [(&str, &str); 5] = [
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    (crate::spawn_seam::EVALS_SEALED_ENV, "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("RIPGREP_CONFIG_PATH", ""),
];

/// The permission mode an unattended footnote evals lane runs in. The other
/// lane doors run headless workers bypassed (the claude one-shot and opencode
/// pass their skip flag, zcode defaults its yolo mode), so the footnote lane
/// matches them through its launch spec. A footnote spawn without the flag
/// keeps the harness default; the mode is never a default.
const FOOTNOTE_LANE_PERMISSION_MODE: &str = "bypassPermissions";

/// The spawn argv: the substrate's own --timeout is the first guard; a
/// lane is a complete coordinate (its harness wins over the provider
/// default, and its route/account ride along). `--` fences the prompt, or
/// with `harness_flags` (or a footnote lane model or permission mode) fences
/// those and the prompt sits before it.
fn spawn_argv(
    name: &str,
    workdir: &str,
    timeout_s: i64,
    harness: Option<&str>,
    lane: Option<&LaneRow>,
    prompt: &str,
    harness_flags: &[String],
) -> Vec<String> {
    let mut argv: Vec<String> = ["agents", "spawn"].iter().map(|s| s.to_string()).collect();
    argv.push("--name".into());
    argv.push(name.into());
    argv.push("--once".into());
    argv.push("--cwd".into());
    argv.push(workdir.into());
    argv.push("--timeout".into());
    argv.push(timeout_s.to_string());
    if let Some(h) = harness {
        argv.push("--harness".into());
        argv.push(h.into());
    }
    if let Some(l) = lane {
        if !l.route.is_empty() {
            argv.push("--route".into());
            argv.push(l.route.clone());
        }
        if !l.account.is_empty() {
            argv.push("--account".into());
            argv.push(l.account.clone());
        }
    }
    // A lane model crosses the fence: the footnote harness reads its model
    // only from fenced args, and an unrouted claude lane whose model stays
    // behind falls back to the CLI default, which put opus on a target lane.
    // A routed lane already carries its model through --route. Each harness
    // spells the flag its own way: claude refuses `-m`.
    let lane_model = lane
        .filter(|l| !l.model.is_empty() && (l.harness == "footnote" || l.route.is_empty()))
        .map(|l| {
            let flag = if l.harness == "footnote" {
                "-m"
            } else {
                "--model"
            };
            (flag, l.model.clone())
        });
    // The unattended footnote lane runs bypassed like every other lane door;
    // the mode rides the fence so the launch spec carries it.
    let lane_mode = lane
        .filter(|l| l.harness == "footnote")
        .map(|_| FOOTNOTE_LANE_PERMISSION_MODE);
    if harness_flags.is_empty() && lane_model.is_none() && lane_mode.is_none() {
        argv.push("--".into());
        argv.push(prompt.into());
    } else {
        // argv-fence: exempt - the fence here holds the harness flags, so the
        // prompt sits before it. run_task sends no flags with a dash-led prompt.
        argv.push(prompt.into());
        argv.push("--".into());
        if let Some((flag, m)) = lane_model {
            argv.push(flag.into());
            argv.push(m);
        }
        if let Some(m) = lane_mode {
            argv.push("--permission-mode".into());
            argv.push(m.to_string());
        }
        argv.extend_from_slice(harness_flags);
    }
    argv
}

/// True when a spawn exit means the worker ran out its budget. 124 is the
/// claude one-shot's timeout. Exit 12 is opencode's timeout and also the code
/// for a registry failure before launch, so 12 counts only with its message.
fn spawn_exit_timed_out(code: i32, message: &str) -> bool {
    code == 124 || (code == 12 && message.contains("timed out"))
}

/// The content the worker left, as a git tree id. The temporary index keeps
/// the real one untouched, and starting from a copy of it spares rehashing
/// the whole checkout. None when git cannot say, which compare reads as
/// unknown.
fn tree_hash(workdir: &std::path::Path) -> Option<String> {
    let _git_step = git_step_lock();
    let git = |args: &[&str], index: Option<&std::path::Path>| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(workdir);
        if let Some(i) = index {
            cmd.env("GIT_INDEX_FILE", i);
        }
        cmd.output().ok().filter(|o| o.status.success())
    };
    let real = git(&["rev-parse", "--git-path", "index"], None)?;
    let real = workdir.join(String::from_utf8_lossy(&real.stdout).trim());
    let scratch = tempfile::tempdir().ok()?;
    let index = scratch.path().join("index");
    std::fs::copy(real, &index).ok()?;
    git(&["add", "-A"], Some(&index))?;
    let tree = git(&["write-tree"], Some(&index))?;
    Some(String::from_utf8_lossy(&tree.stdout).trim().to_string())
}

/// The worker spawn: `fno agent spawn --once` in the workdir. A non-zero
/// exit, missing binary, or timeout is a graded failure, never a sweep
/// crash. The deadline is the backstop the Python leg kept: a wedged
/// spawner fails the leg instead of hanging the run. The worker name is the
/// typed non-node identity `ev-th-evals-<pid>-<secs>-<seq>`. The pipes drain on
/// their own threads, because a worker that fills one would otherwise block
/// until the deadline and read as a stall.
fn default_spawn(
    prompt: &str,
    workdir: &std::path::Path,
    timeout_s: i64,
    provider: Option<&str>,
    lane: Option<&LaneRow>,
    harness_flags: &[String],
) -> SpawnResult {
    // The per-process sequence keeps same-second parallel spawns and an
    // in-window retry off one reserved name (a retry reuses pid and second).
    static SPAWN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tag = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        SPAWN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let name = format!("ev-th-evals-{tag}");
    let harness = lane
        .map(|l| l.harness.as_str())
        .filter(|h| !h.is_empty())
        .or(provider);
    let argv = spawn_argv(
        &name,
        &workdir.display().to_string(),
        timeout_s,
        harness,
        lane,
        prompt,
        harness_flags,
    );
    // Name a vanished workdir here, before the child blames its own binary:
    // the inner spawn folds a missing cwd into the same not-found as a
    // missing CLI.
    if !workdir.is_dir() {
        return SpawnResult {
            ok: false,
            reason: format!(
                "eval workdir is missing before the spawn: {}",
                workdir.display()
            ),
            worker_name: name,
            timed_out: false,
            lane_note: String::new(),
        };
    }
    let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
    if !harness_flags.is_empty() {
        cmd.envs(SEALED_WORKER_ENV);
    }
    // The pin rides every claude eval worker, sealed or not: the harness's
    // own state writes (transcripts, history) land under the fno eval dir
    // instead of the user's ~/.claude. Other harnesses keep their own stores,
    // so they take no pin. Staging copies the auth and onboarding state that
    // makes the pinned root runnable headless, and mirrors the context
    // entries (plugin, user memory) a worker reads.
    let is_claude = harness.is_some_and(|h| Harness::parse(h) == Harness::Claude);
    if is_claude {
        if let Some(dir) = crate::spawn_seam::stage_evals_claude_config() {
            cmd.env("CLAUDE_CONFIG_DIR", &dir);
        }
    }
    cmd.args(&argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(timeout_s.max(1) as u64 + 30);
    let mut child = match cmd.spawn() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return SpawnResult {
                ok: false,
                reason: "spawn failed: `fno` binary not found".into(),
                worker_name: name,
                timed_out: false,
                lane_note: String::new(),
            };
        }
        Err(e) => {
            return SpawnResult {
                ok: false,
                reason: format!("spawn error: {e}"),
                worker_name: name,
                timed_out: false,
                lane_note: String::new(),
            };
        }
        Ok(child) => child,
    };
    fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut text);
            }
            text
        })
    }
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());
    loop {
        match child.try_wait() {
            Err(e) => {
                return SpawnResult {
                    ok: false,
                    reason: format!("spawn error: {e}"),
                    worker_name: name,
                    timed_out: false,
                    lane_note: String::new(),
                };
            }
            Ok(Some(status)) => {
                let out_text = out_reader.join().unwrap_or_default();
                let err_text = err_reader.join().unwrap_or_default();
                let code = status.code().unwrap_or(-1);
                if code == 0 {
                    return SpawnResult {
                        ok: true,
                        reason: String::new(),
                        worker_name: name,
                        timed_out: false,
                        lane_note: String::new(),
                    };
                }
                let tail = if err_text.trim().is_empty() {
                    out_text
                } else {
                    err_text
                };
                let last = failure_line(&tail);
                return SpawnResult {
                    ok: false,
                    reason: format!("spawn exit {code}: {last}"),
                    worker_name: name,
                    timed_out: spawn_exit_timed_out(code, &last),
                    lane_note: String::new(),
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return SpawnResult {
                        ok: false,
                        reason: format!("spawn timed out after {timeout_s}s"),
                        worker_name: name,
                        timed_out: true,
                        lane_note: String::new(),
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// Words that mark a line as the cause of a failed launch.
const CAUSE_WORDS: [&str; 9] = [
    "error",
    "429",
    "limit",
    "exhaust",
    "denied",
    "refused",
    "not found",
    "unavailable",
    "failed",
];

/// The line that names why a spawn exited non-zero. Claude prints
/// `[claude-code:...]` diagnostics on runs that succeed too (an SDK side
/// query on a routed model id), and a trailing hint can follow the real
/// error. Take the last non-diagnostic line that reads as a cause, else the
/// last non-diagnostic line, else the last line.
fn failure_line(text: &str) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    let plain: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim_start().starts_with("[claude-code:"))
        .collect();
    let reads_as_cause = |l: &&str| {
        let low = l.to_lowercase();
        CAUSE_WORDS.iter().any(|w| low.contains(w))
    };
    plain
        .iter()
        .rev()
        .find(|l| reads_as_cause(l))
        .or(plain.last())
        .or(lines.last())
        .map(|l| l.to_string())
        .unwrap_or_default()
}

/// A resolved lane coordinate from the native inventory (route_gather):
/// the same fold `agents.profiles.*.lanes` joins against, never a second
/// model/effort enum.
#[derive(Default)]
pub struct LaneRow {
    pub name: String,
    pub harness: String,
    pub route: String,
    pub account: String,
    pub model: String,
    pub effort: String,
}

/// Resolve a named lane through the native inventory; the refusal names the
/// declared lanes (the Python repr shape, single quotes).
pub fn resolve_lane(name: &str) -> Result<LaneRow, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let lane = lane_from_inventory(&crate::route_gather::inventory(&cwd), name)?;
    if !lane.account.is_empty() {
        if let Ok(accounts) = crate::accounts::records::load(None, &cwd) {
            let ids: Vec<&str> = accounts
                .records
                .iter()
                .filter(|r| r.harness == lane.harness)
                .map(|r| r.id.as_str())
                .collect();
            lane_account_check(&lane, &ids)?;
        }
    }
    Ok(lane)
}

/// A lane's `account` must name a registered record for its own harness.
/// A route such as `zai/...` is a vendor, not an account: naming it as one
/// made every spawn on the lane exit 2 after the gate had already queued its
/// trials. The refusal fires at lane resolution, before any attempt spends.
fn lane_account_check(lane: &LaneRow, harness_ids: &[&str]) -> Result<(), String> {
    if harness_ids.contains(&lane.account.as_str()) {
        return Ok(());
    }
    Err(format!(
        "lane '{}' names account '{}', which is not a registered {} account ({}); \
         drop `account` from its routing.models row or register it",
        lane.name,
        lane.account,
        lane.harness,
        harness_ids.join(", ")
    ))
}

pub(crate) fn lane_from_inventory(inv: &serde_json::Value, name: &str) -> Result<LaneRow, String> {
    let rows = inv
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let field = |row: &serde_json::Value, key: &str| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let Some(row) = rows.iter().find(|r| field(r, "name") == name) else {
        let mut names: Vec<String> = rows.iter().map(|r| field(r, "name")).collect();
        names.sort();
        let joined: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
        let declared = joined.join(", ");
        return Err(format!(
            "unknown lane '{name}'; declared lanes: [{declared}]"
        ));
    };
    Ok(LaneRow {
        name: name.to_string(),
        harness: field(row, "harness"),
        route: field(row, "route"),
        account: field(row, "account"),
        model: field(row, "model"),
        effort: field(row, "effort"),
    })
}

/// What ran, read from the harness's own transcript store through the
/// observe door. A transcript nobody can read leaves `lane_status`
/// `unverified` and `usage` null, never zero.
fn lane_evidence(
    lane: &LaneRow,
    harness: &str,
    attempted: bool,
    spawned: bool,
    workdir: &std::path::Path,
    started_epoch: f64,
) -> serde_json::Value {
    let model = if lane.model.is_empty() {
        &lane.route
    } else {
        &lane.model
    };
    let payload = json!({
        "lane": {"name": lane.name, "harness": harness, "model": model, "effort": lane.effort},
        "attempted": attempted,
        "spawned": spawned,
        "workdir": workdir.to_string_lossy(),
        "started_epoch": started_epoch,
    });
    crate::eval_attempt::observe(&payload)
}

/// The harness a run spawns on: the lane row's, else the CLI arg. The history
/// row records this same value, so a lane-driven run is not stamped null.
fn seat_harness<'a>(
    lane: Option<&'a LaneRow>,
    worker_provider: Option<&'a str>,
) -> Option<&'a str> {
    lane.map(|l| l.harness.as_str())
        .filter(|h| !h.is_empty())
        .or(worker_provider)
}

/// The run loop: per (repeat, attempt): fresh disposable worktree ->
/// optional worker -> mechanical grade -> history row -> worktree removed.
/// Every attempt persists with a unique identity and structured
/// observations; the native verdict labels it graded/infrastructure/
/// unavailable/ungraded. A repeat whose attempt 0 is not graded gets one
/// attempt 1; a config refusal (`gate_blocked`) is not retried.
pub fn run_task(
    task: &super::bank::TaskSpec,
    repo_root: &std::path::Path,
    repeat: u32,
    history_path: &std::path::Path,
    worker_provider: Option<&str>,
    variant: &str,
    variant_ref: &str,
    lane: Option<&LaneRow>,
    experiment_id: Option<&str>,
    overlay_ref: Option<&str>,
) -> Vec<RunResult> {
    // A work-product task checks out its fix's parent whatever `--ref` says;
    // `--ref` then names only the rules tree the worker reads.
    let work_product = task.is_work_product();
    let checkout_ref = if work_product {
        task.repo_fixture.clone()
    } else {
        variant_ref.to_string()
    };
    let rules_ref = (work_product && variant_ref != task.repo_fixture).then_some(variant_ref);
    let bank_rev = git_rev(repo_root, variant_ref);
    let fixture_rev = work_product
        .then(|| git_rev(repo_root, &checkout_ref))
        .flatten();
    let overlay_rev = overlay_ref.and_then(|r| git_rev(repo_root, r));
    let timeout_s = std::cmp::max(1, task.timeout_minutes.saturating_mul(60));
    let mut results: Vec<RunResult> = Vec::new();
    let run_id = uuid_v4();
    let spawn_harness = seat_harness(lane, worker_provider);
    // One probe per run: every attempt row of this run carries the same
    // harness version, so a pinned-version read never guesses.
    let harness_version =
        crate::ledger_register::probe_harness_version(spawn_harness.unwrap_or_default());
    // A run that dies before a verdict is an infra failure, not a grade: it
    // reruns once, and a second death leaves the live runs to decide.
    let mut queue: std::collections::VecDeque<(u32, u32)> = (0..repeat).map(|i| (i, 0)).collect();
    while let Some((i, attempt)) = queue.pop_front() {
        let started = std::time::Instant::now();
        let started_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut reason = String::new();
        let mut gate_blocked = false;
        let mut spawned = false;
        let mut worker_timed_out = false;
        let mut outcome: Option<super::grading::GradeOutcome> = None;
        let mut workdir: Option<std::path::PathBuf> = None;
        let mut worker_name: Option<String> = None;
        let mut tree: Option<String> = None;
        let mut worker_sealed = false;
        let mut kept_transcript: Option<std::path::PathBuf> = None;
        let mut lane_fields: Option<serde_json::Value> = None;
        let mut grade_base = checkout_ref.clone();
        let golden = task.golden.is_some();
        if golden {
            let overlay = overlay_rev.as_deref().or(overlay_ref);
            match sealed_base(&std::env::temp_dir(), &home_dir()).and_then(|base| {
                make_sealed_tree(
                    repo_root,
                    &checkout_ref,
                    overlay,
                    rules_ref,
                    &task.id,
                    &base,
                )
            }) {
                Err(e) => {
                    reason =
                        format!("fixture checkout failed ({checkout_ref}); fixture drift? {e}");
                }
                Ok((dir, head)) => {
                    grade_base = head;
                    workdir = Some(dir);
                }
            }
        } else {
            match make_disposable_worktree(repo_root, &checkout_ref, &task.id) {
                Err(e) => {
                    reason =
                        format!("fixture checkout failed ({checkout_ref}); fixture drift? {e}");
                }
                Ok(dir) => match overlay_rev
                    .as_deref()
                    .or(overlay_ref)
                    .map(|r| apply_overlay(&dir, r))
                {
                    Some(Err(e)) => {
                        reason = format!("fixture overlay failed on {checkout_ref}: {e}");
                        remove_worktree(repo_root, &dir);
                    }
                    Some(Ok(head)) => {
                        grade_base = head;
                        workdir = Some(dir);
                    }
                    None => workdir = Some(dir),
                },
            }
        }
        if let Some(dir) = &workdir {
            if task.prompt.is_some() && !evals_enabled() {
                // The gate only bites the REAL default spawn; a caller with
                // its own worker is an explicit invocation, not autonomous.
                reason = "config.evals.enabled is false".into();
                gate_blocked = true;
            } else if let Some(prompt) = &task.prompt {
                // Other bank tasks invoke plugin skills, so only a golden drops them.
                // A leading dash needs the fence, so that prompt also keeps them.
                let flags = if golden && !prompt.starts_with('-') {
                    tree_only_flags(
                        spawn_harness,
                        dir,
                        &home_dir().canonicalize().unwrap_or_default(),
                    )
                } else {
                    Vec::new()
                };
                worker_sealed = !flags.is_empty();
                let res = default_spawn(prompt, dir, timeout_s, worker_provider, lane, &flags);
                if !res.ok {
                    reason = res.reason.clone();
                    worker_timed_out = res.timed_out;
                }
                if res.ok || res.timed_out {
                    spawned = true;
                    worker_name = Some(res.worker_name.clone());
                }
            }
            if !gate_blocked && (spawned || task.prompt.is_none()) {
                tree = tree_hash(dir);
            }
            if reason.is_empty() {
                let o = super::grading::grade(task, dir, Some(&grade_base));
                if !o.passed {
                    reason = o.reason();
                }
                outcome = Some(o);
            }
            // Before the cleanup: a pass forgets the transcript this reads.
            lane_fields = lane.map(|l| {
                lane_evidence(
                    l,
                    spawn_harness.unwrap_or_default(),
                    task.prompt.is_some(),
                    spawned,
                    dir,
                    started_wall,
                )
            });
            if golden {
                let _ = std::fs::remove_dir_all(dir);
            } else {
                remove_worktree(repo_root, dir);
            }
            if outcome.as_ref().is_some_and(|o| o.passed) {
                forget_trial_transcript(dir);
            } else {
                kept_transcript = Some(trial_transcript_dir(dir));
            }
        }
        let duration = started.elapsed().as_secs_f64().round_ties_even();
        let passed = outcome.as_ref().map(|o| o.passed).unwrap_or(false);
        let mut obs = serde_json::Map::new();
        obs.insert("fixture_prepared".into(), json!(workdir.is_some()));
        obs.insert("worker_required".into(), json!(task.prompt.is_some()));
        if task.prompt.is_some() {
            obs.insert("worker_started".into(), json!(spawned));
        }
        if task.prompt.is_some() && worker_timed_out {
            obs.insert("worker_timed_out".into(), json!(true));
        }
        if task.prompt.is_some() {
            obs.insert("worker_sealed".into(), json!(worker_sealed));
        }
        if let Some(dir) = kept_transcript.as_ref().filter(|d| d.is_dir()) {
            obs.insert("kept_transcript".into(), json!(dir.display().to_string()));
        }
        if gate_blocked {
            obs.insert("gate_blocked".into(), json!(true));
        }
        obs.insert("grader_ran".into(), json!(outcome.is_some()));
        if let Some(o) = &outcome {
            obs.insert("grader_passed".into(), json!(o.passed));
        }
        if let Some(name) = &worker_name {
            obs.insert("worker".into(), json!(name));
        }
        let attempt_id = uuid_v4();
        let row_payload = json!({"obs": obs.clone(), "bank_rev": bank_rev});
        let verdict = crate::eval_attempt::classify(&row_payload, bank_rev.as_deref());
        let status = verdict.status.to_string();
        if attempt == 0 && !gate_blocked && status != "graded" {
            queue.push_front((i, 1));
        }
        let mut row = json!({
            "ts": now_iso(),
            "task_id": task.id,
            "tier": task.tier,
            "pass": passed,
            "reason": if passed { "" } else { reason.as_str() },
            "duration_s": duration,
            "repeat_index": i,
            "attempt_index": attempt,
            "attempt_id": attempt_id,
            "run_id": run_id,
            "obs": obs,
            "bank_rev": bank_rev,
            "tree_hash": tree,
            "worker_provider": spawn_harness,
            "variant": variant,
            "experiment_id": experiment_id,
        });
        if let Some(rev) = &overlay_rev {
            row["overlay_rev"] = json!(rev);
        }
        if let Some(rev) = &fixture_rev {
            row["fixture_rev"] = json!(rev);
        }
        if let Some(golden) = &task.golden {
            row["failure_type"] = json!(golden.failure_type);
        }
        row["seat"] = json!(if task.tags.iter().any(|t| t == "lead") {
            "lead"
        } else {
            "worker"
        });
        if !status.is_empty() {
            row["status"] = json!(status);
        }
        if let Some(l) = lane {
            let fields = lane_fields.unwrap_or_else(|| {
                let none = std::path::Path::new("");
                lane_evidence(
                    l,
                    spawn_harness.unwrap_or_default(),
                    task.prompt.is_some(),
                    false,
                    none,
                    started_wall,
                )
            });
            if let serde_json::Value::Object(fields) = fields {
                for (k, v) in fields {
                    row[k] = v;
                }
            }
            row["requested_account"] = json!(l.account);
        }
        if let Some(fields) = row.as_object_mut() {
            fields.insert(
                "harness_version".to_string(),
                match &harness_version {
                    Some(v) => json!(v),
                    None => serde_json::Value::Null,
                },
            );
        }
        let _ = super::history::append_attempt(history_path, &row);
        results.push(RunResult {
            task_id: task.id.clone(),
            tier: task.tier.clone(),
            passed,
            reason: if passed {
                String::new()
            } else {
                reason.clone()
            },
            duration_s: duration,
            repeat_index: i,
            variant: variant.to_string(),
            attempt_index: attempt,
            attempt_id: attempt_id.into(),
            status,
        });
    }
    results
}

/// A uuid4-shaped hex id from getrandom (16 bytes, version/variant bits).
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(32);
    for byte in b {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The row timestamp: Python isoformat's Z form, microseconds.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_run_slot_refuses_until_the_first_releases() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("run.lock");
        let first = RunSlot::try_acquire(&path).expect("first acquire on a free slot");
        assert!(
            RunSlot::try_acquire(&path).is_none(),
            "a held slot must refuse a second taker"
        );
        assert_eq!(
            slot_holder_pid(&path),
            Some(std::process::id()),
            "the slot names its holder pid for the queue line"
        );
        drop(first);
        let second = RunSlot::try_acquire(&path).expect("release frees the slot");
        assert_eq!(slot_holder_pid(&path), Some(std::process::id()));
        drop(second);
    }

    #[test]
    fn lane_account_check_refuses_a_foreign_harness_account() {
        let lane = LaneRow {
            name: "hf-opencode".into(),
            harness: "opencode".into(),
            account: "zai".into(),
            ..LaneRow::default()
        };
        let err = lane_account_check(&lane, &[]).unwrap_err();
        assert!(
            err.contains("registered opencode account"),
            "refusal names the harness: {err}"
        );
        // A registered id passes for the same harness.
        assert!(lane_account_check(&lane, &["zai"]).is_ok());
    }

    #[test]
    fn a_timeout_exit_reads_started_and_scores_a_failed_grade() {
        assert!(spawn_exit_timed_out(124, "claude -p timed out after 60.0s"));
        assert!(spawn_exit_timed_out(12, "opencode run timed out after 60s"));
        assert!(!spawn_exit_timed_out(12, "registry read failed: gone"));
        assert!(!spawn_exit_timed_out(1, "timed out"));

        let stall = json!({"obs": {
            "fixture_prepared": true, "worker_required": true, "worker_started": true,
            "worker_timed_out": true, "grader_ran": false,
        }});
        let verdict = crate::eval_attempt::classify(&stall, None);
        assert_eq!((verdict.status, verdict.graded), ("graded", Some(false)));
        assert!(!verdict.retryable);

        let refused = json!({"obs": {
            "fixture_prepared": true, "worker_required": true, "worker_started": false,
            "worker_timed_out": true, "grader_ran": false,
        }});
        assert_eq!(
            crate::eval_attempt::classify(&refused, None).status,
            "unavailable",
            "an old row with no started worker keeps its verdict"
        );
    }

    #[test]
    fn a_sealed_tree_keeps_no_link_to_the_repo_and_no_answer_key() {
        let d = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let repo = d.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(repo.join(OVERLAY_PATH)).unwrap();
        std::fs::create_dir_all(repo.join("evals/bank")).unwrap();
        std::fs::write(repo.join(OVERLAY_PATH).join("f.py"), "old").unwrap();
        std::fs::write(repo.join("evals/bank/g.yaml"), "expected: x").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "rules").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "old"]);
        let old = git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.join(OVERLAY_PATH).join("f.py"), "new").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "new"]);
        let new = git(&["rev-parse", "HEAD"]);

        let (tree, head) = make_sealed_tree(
            &repo,
            &old,
            Some(new.as_str()),
            None,
            "golden-x",
            base.path(),
        )
        .unwrap();

        let in_tree = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&tree)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert!(tree.join(".git").is_dir());
        assert_eq!(in_tree(&["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(in_tree(&["rev-parse", "HEAD"]), head);
        assert_eq!(in_tree(&["remote"]), "");
        assert_eq!(in_tree(&["status", "--porcelain"]), "");
        assert!(!tree.join(".git/objects/info/alternates").exists());
        assert!(!tree.join("evals/bank").exists());
        assert_eq!(
            std::fs::read_to_string(tree.join(OVERLAY_PATH).join("f.py")).unwrap(),
            "new",
            "the overlay ref wins"
        );
        let config = std::fs::read_to_string(tree.join(".git/config")).unwrap();
        assert!(!config.contains(repo.to_str().unwrap()), "{config}");
    }

    #[test]
    fn a_sealed_base_inside_home_refuses() {
        let home = tempfile::tempdir().unwrap();
        let err = sealed_base(&home.path().join("tmp"), home.path()).unwrap_err();
        assert!(err.contains("inside the home dir"), "{err}");
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(sealed_base(elsewhere.path(), home.path()).is_ok());
    }

    #[test]
    fn a_lane_model_crosses_the_fence_as_minus_m() {
        let lane = LaneRow {
            name: "fn".into(),
            harness: "footnote".into(),
            model: "glm-5.3-flash".into(),
            ..Default::default()
        };
        let argv = spawn_argv(
            "n",
            "/tmp/wt",
            60,
            Some("footnote"),
            Some(&lane),
            "run it",
            &[],
        );
        let fence = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            argv[fence - 1],
            "run it",
            "the prompt sits before the fence"
        );
        assert_eq!(&argv[fence + 1..fence + 3], &["-m", "glm-5.3-flash"]);
        // The unattended footnote lane runs bypassed like every other lane
        // door: the mode rides the fence so the launch spec carries it.
        assert_eq!(
            &argv[fence + 3..fence + 5],
            &["--permission-mode", "bypassPermissions"]
        );
        // The model rides for claude too: a claude lane whose model stays
        // behind falls back to the CLI's own default, which put opus on a
        // target lane.
        let claude_lane = LaneRow {
            name: "c".into(),
            harness: "claude".into(),
            model: "claude-sonnet-5".into(),
            ..Default::default()
        };
        let argv = spawn_argv(
            "n",
            "/tmp/wt",
            60,
            Some("claude"),
            Some(&claude_lane),
            "p",
            &[],
        );
        let fence = argv.iter().position(|a| a == "--").unwrap();
        // claude spells it --model and exits 1 on `-m`.
        assert_eq!(&argv[fence + 1..fence + 3], &["--model", "claude-sonnet-5"]);
        assert!(!argv.contains(&"-m".to_string()));
        // A routed claude lane already carries its model: nothing crosses.
        let routed = LaneRow {
            name: "z".into(),
            harness: "claude".into(),
            model: "glm-5.3-flash[1m]".into(),
            route: "zai/glm-5.3-flash[1m]".into(),
            ..Default::default()
        };
        let argv = spawn_argv("n", "/tmp/wt", 60, Some("claude"), Some(&routed), "p", &[]);
        assert!(!argv.contains(&"--model".to_string()));
        assert!(!argv.contains(&"-m".to_string()));
        // The mode is footnote-only too: another harness's lane never carries
        // it (each door owns its own unattended posture).
        assert!(!argv.contains(&"--permission-mode".to_string()));
        // A footnote lane with no model still gets the fence for the mode.
        let bare = LaneRow {
            name: "fn2".into(),
            harness: "footnote".into(),
            ..Default::default()
        };
        let argv = spawn_argv("n", "/tmp/wt", 60, Some("footnote"), Some(&bare), "p", &[]);
        let fence = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &argv[fence + 1..fence + 3],
            &["--permission-mode", "bypassPermissions"]
        );
        assert!(!argv.contains(&"-m".to_string()));
    }
}
