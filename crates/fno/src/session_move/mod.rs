//! `fno agents session move <name> --to <host>`: carry one live claude session
//! to another machine over ssh and retire the source, so the work continues
//! under the same name and nothing runs twice.
//!
//! The move is a pure plan followed by an exec. [`build_plan`] reads no disk
//! and runs no command, so tests pin the step order and every refusal. The
//! step families (`code`, `carry`, `cutover`) each own a slice of the order;
//! `all` loops the whole move over a host's workers. A refusal exits 2 with one
//! line naming the cause, before any step touches either machine.

mod all;
mod carry;
mod code;
mod cutover;

use crate::transcript_transfer;
use serde::Deserialize;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

const USAGE: &str = "fno agents session move <name> --to <host> [--all] [--dry-run]";
const MANUAL_RECIPE: &str = "docs/architecture/session-move.md#manual-recipe";

/// The ordered steps of one move. The order is the contract: the source
/// stops before any byte of its transcript is copied, and the source row
/// leaves only after verify passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Preflight,
    PushCommits,
    StopSource,
    WorktreeSync,
    TranscriptSync,
    CanonDocSync,
    TargetRow,
    ClaimsHandover,
    Resume,
    Verify,
    Retire,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::Preflight => "preflight",
            Step::PushCommits => "push commits",
            Step::StopSource => "stop source",
            Step::WorktreeSync => "worktree sync",
            Step::TranscriptSync => "transcript sync",
            Step::CanonDocSync => "canon doc sync",
            Step::TargetRow => "target row",
            Step::ClaimsHandover => "claims handover",
            Step::Resume => "resume",
            Step::Verify => "verify",
            Step::Retire => "retire",
        }
    }
}

/// What the command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub name: Option<String>,
    pub to: Option<String>,
    pub all: bool,
    pub dry_run: bool,
}

/// A command line that does not parse; the text names the cause.
#[derive(Debug, PartialEq, Eq)]
pub enum ArgError {
    Help,
    Usage(String),
}

/// The parsed command line: `<name> --to <host> [--all] [--dry-run]`.
pub fn parse_args(rest: &[String]) -> Result<Options, ArgError> {
    let mut opts = Options::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--help" | "-h" => return Err(ArgError::Help),
            "--all" => opts.all = true,
            "--dry-run" => opts.dry_run = true,
            "--to" => {
                let value = inline.or_else(|| it.next().cloned());
                match value {
                    Some(v) if !v.is_empty() => opts.to = Some(v),
                    _ => return Err(ArgError::Usage("--to needs a host".into())),
                }
            }
            other if other.starts_with('-') => {
                return Err(ArgError::Usage(format!("unknown flag {other}")));
            }
            _ if opts.name.is_none() => opts.name = Some(arg.clone()),
            _ => return Err(ArgError::Usage(format!("unexpected argument {arg}"))),
        }
    }
    if opts.to.is_none() {
        return Err(ArgError::Usage("--to <host> is required".into()));
    }
    match (&opts.name, opts.all) {
        (None, false) => Err(ArgError::Usage("name a session, or pass --all".into())),
        (Some(_), true) => Err(ArgError::Usage("--all takes no session name".into())),
        _ => Ok(opts),
    }
}

/// One registry row, the fields a move reads.
#[derive(Clone, Debug, Deserialize)]
pub struct Row {
    pub name: String,
    #[serde(default)]
    pub harness: String,
    pub harness_session_id: Option<String>,
    pub cwd: Option<String>,
    pub node: Option<String>,
    pub role_scope: Option<String>,
    pub liveness: Option<String>,
}

#[derive(Deserialize)]
struct ListOut {
    #[serde(default)]
    agents: Vec<Row>,
}

/// Where a transcript lives and how the target rebuilds the same location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub path: PathBuf,
    pub store: String,
    pub rel: PathBuf,
}

/// What the source machine knows about a row beyond the row itself.
#[derive(Clone, Debug)]
pub struct Facts {
    pub worktree: PathBuf,
    pub branch: String,
    pub head: String,
    pub dirty: bool,
    pub transcript: Option<Transcript>,
    pub canon_doc: Option<PathBuf>,
    /// The repo's absolute common dir, the push target on the far side.
    pub common_dir: PathBuf,
    /// The `<session-id>/` directory beside the transcript, when it exists.
    pub sidecar: Option<PathBuf>,
    /// The `<session-id>.fno.json` origin record beside it, when it exists.
    pub origin_record: Option<PathBuf>,
}

/// One move, fully described and ordered.
#[derive(Clone, Debug)]
pub struct MovePlan {
    pub name: String,
    pub session_id: String,
    pub harness: String,
    pub cwd: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
    pub head: String,
    pub dirty: bool,
    pub transcript: Transcript,
    pub canon_doc: Option<PathBuf>,
    pub common_dir: PathBuf,
    pub sidecar: Option<PathBuf>,
    pub origin_record: Option<PathBuf>,
    pub claims: Vec<String>,
    pub host: String,
    pub from_host: String,
    pub steps: Vec<Step>,
}

/// A move the command refuses before it touches either machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    SameHost(String),
    BadHost(String),
    NoRow(String),
    NoSessionId(String),
    OtherHarness {
        name: String,
        harness: String,
    },
    NoTranscript {
        name: String,
        detail: String,
    },
    TargetUnreachable {
        host: String,
        detail: String,
    },
    TargetDaemon {
        host: String,
        detail: String,
    },
    UnsafePath(PathBuf),
    TargetHoldsOther {
        host: String,
        path: PathBuf,
        branch: String,
    },
    TargetNewer {
        host: String,
        path: PathBuf,
        source: u64,
        target: u64,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::SameHost(h) => write!(f, "--to {h} is this host; name another machine"),
            Refusal::BadHost(h) => write!(f, "--to {h:?} is not a host name"),
            Refusal::NoRow(n) => write!(f, "no session named {n} in this host's registry"),
            Refusal::NoSessionId(n) => write!(f, "{n} has no harness session id to move"),
            Refusal::OtherHarness { name, harness } => write!(
                f,
                "{name} runs {harness}; codex and opencode moves are not built yet, the manual recipe: {MANUAL_RECIPE}"
            ),
            Refusal::NoTranscript { name, detail } => {
                write!(f, "no transcript to move for {name}: {detail}")
            }
            Refusal::TargetUnreachable { host, detail } => {
                write!(f, "cannot reach {host} over ssh with fno installed: {detail}")
            }
            Refusal::TargetDaemon { host, detail } => write!(
                f,
                "the claude daemon on {host} is not gui-born ({detail}); start it from a login session there, then retry"
            ),
            Refusal::UnsafePath(p) => write!(
                f,
                "{} has a character the move cannot pass to ssh; rename it, then retry",
                p.display()
            ),
            Refusal::TargetHoldsOther { host, path, branch } => write!(
                f,
                "{host}:{} already holds branch {branch:?}, not this session's; remove it there, then retry",
                path.display()
            ),
            Refusal::TargetNewer { host, path, source, target } => write!(
                f,
                "{host}:{} is newer than the source copy (target mtime {target}, source mtime {source}); the move stops before it overwrites newer work",
                path.display()
            ),
        }
    }
}

/// A shell or parse failure, kept as text for the line the command prints.
#[derive(Debug, PartialEq, Eq)]
pub struct Fault(pub String);

impl From<&str> for Fault {
    fn from(text: &str) -> Self {
        Fault(text.to_string())
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A step that failed after the plan was accepted.
#[derive(Debug, PartialEq, Eq)]
pub enum MoveError {
    Failed { step: Step, detail: String },
}

impl MoveError {
    pub fn failed(step: Step, detail: impl Into<String>) -> Self {
        MoveError::Failed {
            step,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoveError::Failed { step, detail } => {
                write!(f, "step \"{}\" failed: {detail}", step.label())
            }
        }
    }
}

/// The host as typed, folded to a bare lowercase name for comparison.
fn normalize_host(host: &str) -> String {
    let lower = host.trim().to_ascii_lowercase();
    lower
        .strip_suffix(".local")
        .unwrap_or(&lower)
        .split('.')
        .next()
        .unwrap_or("")
        .to_string()
}

fn host_is_valid(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':'))
}

/// Refuse a target that is not a host name, or is this host.
pub fn check_host(to: &str, local_host: &str) -> Result<(), Refusal> {
    if !host_is_valid(to) {
        return Err(Refusal::BadHost(to.to_string()));
    }
    let bare = to.rsplit('@').next().unwrap_or(to);
    let target = normalize_host(bare);
    if target == "localhost" || target == normalize_host(local_host) {
        return Err(Refusal::SameHost(to.to_string()));
    }
    Ok(())
}

/// Find the row and refuse a harness this command does not move.
pub fn check_row<'a>(name: &str, rows: &'a [Row]) -> Result<&'a Row, Refusal> {
    let row = rows
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| Refusal::NoRow(name.to_string()))?;
    if row.harness != "claude" {
        return Err(Refusal::OtherHarness {
            name: name.to_string(),
            harness: row.harness.clone(),
        });
    }
    Ok(row)
}

/// The ordered steps. Worktree sync runs only for a dirty tree; every other
/// step is always present (the canon doc and claims steps no-op on a row
/// with nothing to carry).
fn steps_for(dirty: bool) -> Vec<Step> {
    let mut steps = vec![Step::Preflight, Step::PushCommits, Step::StopSource];
    if dirty {
        steps.push(Step::WorktreeSync);
    }
    steps.extend([
        Step::TranscriptSync,
        Step::CanonDocSync,
        Step::TargetRow,
        Step::ClaimsHandover,
        Step::Resume,
        Step::Verify,
        Step::Retire,
    ]);
    steps
}

/// Build the plan for one row. No I/O: the caller read the row and the facts.
pub fn build_plan(
    row: &Row,
    facts: Facts,
    host: &str,
    from_host: &str,
) -> Result<MovePlan, Refusal> {
    let session_id = match row.harness_session_id.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err(Refusal::NoSessionId(row.name.clone())),
    };
    let transcript = facts.transcript.ok_or_else(|| Refusal::NoTranscript {
        name: row.name.clone(),
        detail: "none found under the claude projects dir".into(),
    })?;
    let cwd = PathBuf::from(row.cwd.clone().unwrap_or_default());
    Ok(MovePlan {
        name: row.name.clone(),
        session_id,
        harness: row.harness.clone(),
        cwd,
        worktree: facts.worktree,
        branch: facts.branch,
        head: facts.head,
        dirty: facts.dirty,
        transcript,
        canon_doc: facts.canon_doc,
        common_dir: facts.common_dir,
        sidecar: facts.sidecar,
        origin_record: facts.origin_record,
        from_host: from_host.to_string(),
        claims: row.node.iter().map(|n| format!("node:{n}")).collect(),
        host: host.to_string(),
        steps: steps_for(facts.dirty),
    })
}

/// The dry-run text: what would run, in order, and nothing run.
pub fn render_dry_run(plan: &MovePlan) -> String {
    let mut out = format!(
        "dry run: move {} ({}) -> {}; nothing is moved\n",
        plan.name, plan.session_id, plan.host
    );
    for (i, step) in plan.steps.iter().enumerate() {
        out.push_str(&format!("  {}. {}\n", i + 1, step.label()));
    }
    out
}

/// What one shell command returned.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Every command the move runs goes through this seam, so tests count calls
/// and a dry run can prove it ran no ssh.
pub trait Shell {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, Fault>;
}

pub struct RealShell;

impl Shell for RealShell {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, Fault> {
        let mut command = std::process::Command::new(program);
        command.args(args).stdin(std::process::Stdio::null());
        let out = crate::process_admission::std_output(&mut command)
            .map_err(|e| Fault(format!("{program}: {e}")))?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

/// The ssh options for every hop: no prompt, five seconds to connect, and the
/// host after `--` so a name can never read as an option. The twin lives in
/// `fno-agents` (`mail_remote::ssh_argv`); crates/fno has no dependency on it.
pub fn ssh_argv(host: &str, remote: &str) -> Vec<String> {
    let mut argv = strings(&["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "--"]);
    argv.push(host.to_string());
    argv.push(remote.to_string());
    argv
}

/// One command the move runs: a program and its argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// `ssh <host> <remote shell string>`.
pub fn ssh_cmd(host: &str, remote: &str) -> Cmd {
    Cmd {
        program: "ssh",
        args: ssh_argv(host, remote),
    }
}

/// A remote command under a login shell, so `fno` and `git` resolve on the
/// target the way they do in a terminal there.
pub fn login(remote: &str) -> String {
    format!("sh -lc {}", sh_quote(remote))
}

/// Single-quote one word for a remote shell.
pub fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// `rsync -a` of `src` to the same path on the host, over the move's ssh
/// options. A trailing slash on `src` copies a directory's contents.
pub fn rsync_cmd(host: &str, src: &str, dst: &str, extra: &[&str]) -> Cmd {
    let mut args = strings(&["-a"]);
    args.extend(strings(extra));
    args.extend(strings(&["-e", "ssh -o BatchMode=yes -o ConnectTimeout=5"]));
    args.push(src.to_string());
    args.push(format!("{host}:{dst}"));
    Cmd {
        program: "rsync",
        args,
    }
}

/// A path the move can pass to ssh and rsync without quoting.
pub fn path_is_plain(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with('/')
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-' | '+' | '@' | ',')
        })
}

/// Run one command; a nonzero exit or a spawn failure fails the step.
pub fn run_cmd(shell: &dyn Shell, step: Step, cmd: &Cmd) -> Result<Output, MoveError> {
    let out = shell
        .run(cmd.program, &cmd.args)
        .map_err(|e| MoveError::failed(step, e.0))?;
    if out.code == 0 {
        return Ok(out);
    }
    let why = out.stderr.lines().rev().find(|l| !l.trim().is_empty());
    Err(MoveError::failed(
        step,
        format!(
            "{} exited {}: {}",
            cmd.program,
            out.code,
            why.unwrap_or("no stderr")
        ),
    ))
}

pub fn run_cmds(shell: &dyn Shell, step: Step, cmds: &[Cmd]) -> Result<(), MoveError> {
    cmds.iter()
        .try_for_each(|c| run_cmd(shell, step, c).map(drop))
}

/// Seconds since the epoch of a local file's last write.
pub fn local_mtime(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
}

/// The same on the target, over ssh. Absent or unreadable reads as None.
pub fn remote_mtime(shell: &dyn Shell, host: &str, path: &Path) -> Option<u64> {
    let p = path.to_string_lossy();
    let remote = format!("stat -f %m {p} 2>/dev/null || stat -c %Y {p}");
    let out = shell.run("ssh", &ssh_argv(host, &login(&remote))).ok()?;
    if out.code != 0 {
        return None;
    }
    out.stdout.trim().parse().ok()
}

const FNO_PROBE: &str = "sh -lc 'exec fno --version'";
/// The first claude daemon's environment on the target, one line.
const DAEMON_PROBE: &str =
    "sh -lc 'ps -E -o command= -p \"$(pgrep -f \"claude daemon run\" | head -1)\"'";

/// A daemon born in an ssh session or a launchd job loses DNS on resume
/// (the daemon-start defect this move reads as a fact, not a fix). It is
/// gui-born when its environment names no ssh connection and a gui
/// `XPC_SERVICE_NAME`; no daemon line at all refuses, since a resume over
/// ssh would start one in the wrong place.
pub fn daemon_is_gui_born(env_line: &str) -> Result<(), Fault> {
    let line = env_line.trim();
    if line.is_empty() {
        return Err("no claude daemon is running there".into());
    }
    if line.contains("SSH_CONNECTION=") {
        return Err("born inside an ssh session".into());
    }
    let service = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("XPC_SERVICE_NAME="));
    match service {
        Some(s) if s.starts_with("application.") => Ok(()),
        Some(s) => Err(Fault(format!("born under XPC_SERVICE_NAME={s}"))),
        None => Err("its launch environment names no gui service".into()),
    }
}

/// Reach the target with fno installed, and check its daemon is gui-born.
pub fn preflight_target(shell: &dyn Shell, host: &str) -> Result<(), Refusal> {
    let unreachable = |detail: String| Refusal::TargetUnreachable {
        host: host.to_string(),
        detail,
    };
    let probe = shell
        .run("ssh", &ssh_argv(host, FNO_PROBE))
        .map_err(|f| unreachable(f.0))?;
    if probe.code != 0 {
        let last = probe.stderr.lines().rev().find(|l| !l.trim().is_empty());
        return Err(unreachable(
            last.unwrap_or("ssh exited nonzero").to_string(),
        ));
    }
    let daemon = shell
        .run("ssh", &ssh_argv(host, DAEMON_PROBE))
        .map_err(|f| unreachable(f.0))?;
    daemon_is_gui_born(&daemon.stdout).map_err(|detail| Refusal::TargetDaemon {
        host: host.to_string(),
        detail: detail.0,
    })
}

/// Parse `fno agents session list --json` output into rows.
pub fn parse_rows(stdout: &str) -> Result<Vec<Row>, Fault> {
    serde_json::from_str::<ListOut>(stdout)
        .map(|l| l.agents)
        .map_err(|e| {
            Fault(format!(
                "fno agents session list printed unreadable json: {e}"
            ))
        })
}

const LIST_ARGS: [&str; 5] = ["agents", "session", "list", "--all", "--json"];

/// The rows of this host's registry, from `fno agents session list`.
pub fn read_rows(shell: &dyn Shell) -> Result<Vec<Row>, Fault> {
    let out = shell.run("fno", &strings(&LIST_ARGS))?;
    if out.code != 0 {
        return Err(Fault(format!(
            "fno agents session list exited {}",
            out.code
        )));
    }
    parse_rows(&out.stdout)
}

/// The same list read on the target.
pub fn read_rows_on(shell: &dyn Shell, host: &str) -> Result<Vec<Row>, Fault> {
    let remote = login(&format!("fno {}", LIST_ARGS.join(" ")));
    let out = shell.run("ssh", &ssh_argv(host, &remote))?;
    if out.code != 0 {
        return Err(Fault(format!(
            "fno agents session list on {host} exited {}",
            out.code
        )));
    }
    parse_rows(&out.stdout)
}

fn git_line(shell: &dyn Shell, dir: &str, args: &[&str]) -> Option<String> {
    let mut argv = strings(&["-C", dir]);
    argv.extend(strings(args));
    let out = shell.run("git", &argv).ok()?;
    (out.code == 0).then(|| out.stdout.trim().to_string())
}

/// Finds one session's transcript; a seam so tests need no claude store.
pub type Locate<'a> = &'a dyn Fn(&str) -> Option<Transcript>;

fn real_locate(sid: &str) -> Option<Transcript> {
    transcript_transfer::locate(sid)
        .ok()
        .map(|(path, store, rel)| Transcript {
            path,
            store: store.to_string(),
            rel,
        })
}

/// Read the source-side facts for one row.
fn gather_facts(shell: &dyn Shell, row: &Row, locate: Locate<'_>) -> Facts {
    let cwd = row.cwd.clone().unwrap_or_default();
    let worktree = git_line(shell, &cwd, &["rev-parse", "--show-toplevel"]).unwrap_or(cwd.clone());
    let branch =
        git_line(shell, &worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let dirty =
        git_line(shell, &worktree, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let transcript = row.harness_session_id.as_deref().and_then(locate);
    let canon_doc = row.role_scope.as_deref().and_then(|scope| {
        let out = shell
            .run(
                "fno",
                &strings(&["config", "paths", "handoff", "--scope", scope]),
            )
            .ok()?;
        let path = out.stdout.trim();
        (out.code == 0 && !path.is_empty()).then(|| PathBuf::from(path))
    });
    let common_dir = git_line(
        shell,
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .unwrap_or_default();
    let (sidecar, origin_record) = transcript
        .as_ref()
        .map(|t| beside_transcript(&t.path, row.harness_session_id.as_deref().unwrap_or("")))
        .unwrap_or((None, None));
    let head = git_line(shell, &worktree, &["rev-parse", "HEAD"]).unwrap_or_default();
    Facts {
        worktree: PathBuf::from(worktree),
        branch,
        head,
        dirty,
        transcript,
        canon_doc,
        common_dir: PathBuf::from(common_dir),
        sidecar,
        origin_record,
    }
}

/// The `<sid>/` sidecar dir and the `<sid>.fno.json` origin record that sit
/// beside a transcript, each only when it exists.
fn beside_transcript(transcript: &Path, sid: &str) -> (Option<PathBuf>, Option<PathBuf>) {
    let sidecar = transcript.with_extension("");
    let origin = transcript.with_file_name(format!("{sid}.fno.json"));
    (
        sidecar.is_dir().then_some(sidecar),
        origin.is_file().then_some(origin),
    )
}

/// Refuse a target whose paths the move cannot trust: a path with an
/// unsafe character, a worktree path holding another branch, or a copy
/// newer than the source's. Runs before any step touches either machine.
pub fn preflight_paths(shell: &dyn Shell, plan: &MovePlan) -> Result<(), Refusal> {
    let mut paths = vec![&plan.worktree, &plan.common_dir, &plan.transcript.path];
    paths.extend(
        plan.sidecar
            .iter()
            .chain(&plan.origin_record)
            .chain(&plan.canon_doc),
    );
    if let Some(bad) = paths.into_iter().find(|p| !path_is_plain(p)) {
        return Err(Refusal::UnsafePath(bad.clone()));
    }
    let probe = format!(
        "git -C {0} rev-parse --abbrev-ref HEAD && git -C {0} rev-parse HEAD",
        plan.worktree.display()
    );
    let held = shell
        .run("ssh", &ssh_argv(&plan.host, &login(&probe)))
        .ok()
        .filter(|o| o.code == 0)
        .map(|o| o.stdout)
        .unwrap_or_default();
    let mut lines = held.lines();
    let held_branch = lines.next().unwrap_or("").trim();
    let held_head = lines.next().unwrap_or("").trim();
    // The same branch at another commit would run stale code, and a push
    // cannot move a checked-out branch, so the move refuses instead.
    let stale = held_branch == plan.branch && held_head != plan.head;
    if !held_branch.is_empty() && (held_branch != plan.branch || stale) {
        return Err(Refusal::TargetHoldsOther {
            host: plan.host.clone(),
            path: plan.worktree.clone(),
            branch: if stale {
                format!("{held_branch} at {held_head}")
            } else {
                held_branch.to_string()
            },
        });
    }
    for path in std::iter::once(&plan.transcript.path).chain(&plan.canon_doc) {
        let (Some(source), Some(target)) =
            (local_mtime(path), remote_mtime(shell, &plan.host, path))
        else {
            continue;
        };
        if target > source {
            return Err(Refusal::TargetNewer {
                host: plan.host.clone(),
                path: path.clone(),
                source,
                target,
            });
        }
    }
    Ok(())
}

fn dispatch(plan: &MovePlan, step: Step, shell: &dyn Shell) -> Result<(), MoveError> {
    match step {
        Step::Preflight => Ok(()),
        Step::PushCommits | Step::WorktreeSync => code::run(plan, step, shell),
        Step::TranscriptSync | Step::CanonDocSync => carry::run(plan, step, shell),
        Step::StopSource
        | Step::TargetRow
        | Step::ClaimsHandover
        | Step::Resume
        | Step::Verify
        | Step::Retire => cutover::run(plan, step, shell),
    }
}

/// Move one named session. Exit 0 moved or dry-ran, 1 a step failed, 2 a
/// refusal or a bad command line.
pub fn move_one(opts: &Options, shell: &dyn Shell, local_host: &str, locate: Locate<'_>) -> i32 {
    let (Some(name), Some(to)) = (opts.name.as_deref(), opts.to.as_deref()) else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let refuse = |r: Refusal| {
        eprintln!("fno agents session move: {r}");
        2
    };
    if let Err(r) = check_host(to, local_host) {
        return refuse(r);
    }
    let rows = match read_rows(shell) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno agents session move: {e}");
            return 1;
        }
    };
    let row = match check_row(name, &rows) {
        Ok(row) => row,
        Err(r) => return refuse(r),
    };
    let plan = match build_plan(row, gather_facts(shell, row, locate), to, local_host) {
        Ok(plan) => plan,
        Err(r) => return refuse(r),
    };
    if opts.dry_run {
        print!("{}", render_dry_run(&plan));
        return 0;
    }
    if let Err(r) = preflight_target(shell, to).and_then(|()| preflight_paths(shell, &plan)) {
        return refuse(r);
    }
    let mut stopped = false;
    for step in plan.steps.iter().copied().filter(|s| *s != Step::Preflight) {
        if let Err(e) = dispatch(&plan, step, shell) {
            eprintln!("fno agents session move: {e}");
            if stopped {
                eprintln!(
                    "the source is stopped and its row stays; resume it here with: fno agents session resume {}",
                    plan.name
                );
            }
            return 1;
        }
        stopped |= step == Step::StopSource;
    }
    println!("moved: {} {} -> {}", plan.name, plan.session_id, plan.host);
    0
}

fn help_text() -> String {
    format!(
        "usage: {USAGE}\n\nMove a claude session to another machine over ssh: its code, worktree, transcript and claims travel, the target resumes it under the same name and session id, and the source retires. --dry-run prints the steps and moves nothing.\n"
    )
}

/// The native `fno agents session move` entry.
pub fn run(rest: &[OsString]) -> i32 {
    let rest: Vec<String> = rest
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let opts = match parse_args(&rest) {
        Ok(opts) => opts,
        Err(ArgError::Help) => {
            print!("{}", help_text());
            return 0;
        }
        Err(ArgError::Usage(why)) => {
            eprintln!("fno agents session move: {why}\nusage: {USAGE}");
            return 2;
        }
    };
    let host = crate::agents_view::hostname();
    if opts.all {
        return all::run(&opts, &RealShell, &host, &real_locate);
    }
    move_one(&opts, &RealShell, &host, &real_locate)
}

#[cfg(test)]
mod tests;
