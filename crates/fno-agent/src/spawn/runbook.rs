//! Scheduled runbooks: `fno agent spawn --runbook <file>` runs a typed
//! runbook through an agent, and `fno do runbook` is the only door its steps
//! run through.
//!
//! The runbook's YAML frontmatter is the contract and its body is prose for
//! the agent. The spawn seam parses the file, writes the run file header and
//! splices a fixed prompt. The agent then runs each step with
//! `fno do runbook step`, which runs the command itself, so the receipt
//! records what ran, not what the agent says ran. An irreversible step refuses
//! without a `--yes` naming a question the user answered; a failed check
//! refuses every later step until the rollback runs.

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The default bound on one step command. A prod push step can run long, so
/// a step that needs more spells `timeout_s`.
const DEFAULT_STEP_TIMEOUT_S: u64 = 3600;
/// How much of a step's combined output the receipt keeps.
const OUTPUT_TAIL_BYTES: usize = 4000;
const MAIL_SENDER: &str = "fno-runbook";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runbook {
    pub runbook: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub preflight: Vec<Step>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub stop_when: Vec<String>,
    pub rollback: Rollback,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub run: String,
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub irreversible: bool,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackStep {
    pub id: String,
    pub run: String,
}

/// A rollback is a list of commands, or `none - <why>` for a runbook whose
/// steps are safe to re-run and have nothing to undo.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Rollback {
    Steps(Vec<RollbackStep>),
    Text(String),
}

impl Rollback {
    /// The reason a `none - <why>` rollback gives, or None for a list.
    pub fn none_reason(&self) -> Option<&str> {
        match self {
            Rollback::Text(t) => t.strip_prefix("none - ").map(str::trim),
            Rollback::Steps(_) => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RunbookError {
    #[error("the runbook has no YAML frontmatter between --- lines")]
    NoFrontmatter,
    #[error("the runbook frontmatter does not parse: {0}")]
    Yaml(String),
    #[error("steps: a runbook needs at least one step")]
    NoSteps,
    #[error("id: {0:?} names more than one step")]
    DuplicateId(String),
    #[error("id: a step id must be non-empty")]
    EmptyId,
    #[error("rollback: a text rollback must read 'none - <why>', got {0:?}")]
    RollbackText(String),
    #[error("irreversible: preflight check {0:?} cannot be irreversible")]
    IrreversiblePreflight(String),
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
/// Parse a runbook file's text into the typed contract.
pub fn parse(text: &str) -> Result<Runbook, RunbookError> {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
        .ok_or(RunbookError::NoFrontmatter)?;
    let end = rest.find("\n---").ok_or(RunbookError::NoFrontmatter)?;
    let yaml = &rest[..end];
    let book: Runbook =
        serde_yaml_ng::from_str(yaml).map_err(|e| RunbookError::Yaml(e.to_string()))?;
    if book.steps.is_empty() {
        return Err(RunbookError::NoSteps);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let rollback_ids: Vec<&str> = match &book.rollback {
        Rollback::Steps(s) => s.iter().map(|r| r.id.as_str()).collect(),
        Rollback::Text(t) => {
            if book.rollback.none_reason().is_none_or(str::is_empty) {
                return Err(RunbookError::RollbackText(t.clone()));
            }
            Vec::new()
        }
    };
    for id in book
        .preflight
        .iter()
        .chain(book.steps.iter())
        .map(|s| s.id.as_str())
        .chain(rollback_ids)
    {
        if id.trim().is_empty() {
            return Err(RunbookError::EmptyId);
        }
        if !seen.insert(id) {
            return Err(RunbookError::DuplicateId(id.to_string()));
        }
    }
    if let Some(p) = book.preflight.iter().find(|p| p.irreversible) {
        return Err(RunbookError::IrreversiblePreflight(p.id.clone()));
    }
    Ok(book)
}

/// Why a runbook door action failed.
#[derive(Debug, thiserror::Error)]
pub enum DoorError {
    #[error("cannot {what} {}: {source}", .path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("runbook {}: {source}", .path.display())]
    Invalid { path: PathBuf, source: RunbookError },
    #[error("run file {} holds a bad line: {source}", .path.display())]
    BadLine {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("run file {} has no header", .0.display())]
    NoHeader(PathBuf),
    /// The door refuses the action; the text names the next step.
    #[error("{0}")]
    Refused(String),
}

impl From<String> for DoorError {
    fn from(text: String) -> Self {
        DoorError::Refused(text)
    }
}

impl From<&str> for DoorError {
    fn from(text: &str) -> Self {
        DoorError::Refused(text.to_string())
    }
}

fn refused<T>(text: String) -> Result<T, DoorError> {
    Err(DoorError::Refused(text))
}

fn io(what: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> DoorError {
    let path = path.to_path_buf();
    move |source| DoorError::Io { what, path, source }
}

impl Runbook {
    /// Preflight then steps: the order the run walks, with each entry's phase.
    fn sequence(&self) -> Vec<(&'static str, &Step)> {
        self.preflight
            .iter()
            .map(|s| ("preflight", s))
            .chain(self.steps.iter().map(|s| ("step", s)))
            .collect()
    }
}

fn read_runbook(path: &Path) -> Result<(Runbook, String), DoorError> {
    let text = std::fs::read_to_string(path).map_err(io("read runbook", path))?;
    let book = parse(&text).map_err(|source| DoorError::Invalid {
        path: path.to_path_buf(),
        source,
    })?;
    Ok((book, sha256_hex(&text)))
}

fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn expand_home(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(raw),
    }
}

fn absolute(raw: &str) -> PathBuf {
    let p = expand_home(raw);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

/// The run files' home: `<agents home>/runbook-runs/`.
pub fn runs_dir() -> PathBuf {
    crate::paths::AgentsHome::from_env()
        .root()
        .join("runbook-runs")
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// The spawn half: validate at schedule time, expand at fire time.
// ---------------------------------------------------------------------------

/// The spawn-owned head of an argv: tokens before `--`/`--argv`.
fn head_end(args: &[String]) -> usize {
    args.iter()
        .position(|t| t == "--" || t == "--argv")
        .unwrap_or(args.len())
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
/// Where `--runbook` sits in `args[..end]`: `(index, value, width)`.
fn find_runbook(
    args: &[String],
    start: usize,
    end: usize,
) -> Option<(usize, Option<String>, usize)> {
    let mut i = start;
    while i < end {
        let t = &args[i];
        if let Some(v) = t.strip_prefix("--runbook=") {
            return Some((i, Some(v.to_string()), 1));
        }
        if t == "--runbook" {
            return Some((i, args.get(i + 1).filter(|_| i + 1 < end).cloned(), 2));
        }
        if t.starts_with('-') && !t.contains('=') && crate::spawn_admission::is_value_flag(t) {
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
fn flag_value(args: &[String], start: usize, end: usize, flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut i = start;
    while i < end {
        let t = &args[i];
        if let Some(v) = t.strip_prefix(&eq) {
            return Some(v.to_string());
        }
        if t == flag {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
/// Schedule time: refuse a bad runbook before a row is written, and record
/// the absolute path so the fire reads the same file from any cwd. `args`
/// excludes the leading `spawn` token. The token count never changes, so the
/// caller's name index stays valid.
pub fn absolutize(args: &[String]) -> Result<Vec<String>, DoorError> {
    let end = head_end(args);
    let mut out = args.to_vec();
    let Some((i, value, width)) = find_runbook(args, 0, end) else {
        return Ok(out);
    };
    let raw = value.ok_or("--runbook needs a file")?;
    let path = absolute(&raw);
    read_runbook(&path)?;
    let shown = path.display().to_string();
    if width == 1 {
        out[i] = format!("--runbook={shown}");
    } else {
        out[i + 1] = shown;
    }
    Ok(out)
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
/// Fire time (and a spawn that runs now): parse the runbook, write the run
/// file header, and splice the fixed prompt as `--message=`. `args` starts
/// with `spawn`. A line with no `--runbook` passes unchanged.
pub fn resolve_runbook(args: Vec<String>) -> Result<Vec<String>, (String, i32)> {
    expand(args).map_err(|(reason, code)| (format!("fno agent spawn: {reason}"), code))
}

/// The seam's one prompt read: a `--prompt-file`, then a `--runbook`.
pub(crate) fn resolve_prompts(args: Vec<String>) -> Result<Vec<String>, (String, i32)> {
    crate::spawn_admission::resolve_prompt_file(args).and_then(resolve_runbook)
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
fn expand(mut args: Vec<String>) -> Result<Vec<String>, (String, i32)> {
    let end = head_end(&args);
    let head = &args[1.min(args.len())..end];
    if head.iter().any(|t| t == "--help" || t == "-h") {
        return Ok(args);
    }
    let rehearse_at = head.iter().position(|t| t == "--rehearse").map(|i| i + 1);
    let Some((at, value, width)) = find_runbook(&args, 1, end) else {
        if rehearse_at.is_some() {
            return Err(("--rehearse needs --runbook <file>".into(), 2));
        }
        return Ok(args);
    };
    let raw = value.ok_or_else(|| ("--runbook needs a file".to_string(), 2))?;
    if head.iter().any(|t| {
        t == "--prompt-file" || t.starts_with("--prompt-file=") || t.starts_with("--message")
    }) || prompt_positional(head)
    {
        return Err((
            "--runbook is the prompt: drop the positional prompt, --message and --prompt-file"
                .into(),
            2,
        ));
    }
    let path = absolute(&raw);
    let (book, sha) = read_runbook(&path).map_err(|e| (e.to_string(), 2))?;
    let scheduler = flag_value(&args, 1, end, "--spawned-by")
        .or_else(|| crate::claims::ambient_parent_edge().0)
        .unwrap_or_default();
    let cwd = book
        .cwd
        .as_deref()
        .map(expand_home)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let rehearse = rehearse_at.is_some();
    let run_file = create_run(&runs_dir(), &path, &sha, &book, rehearse, &scheduler, &cwd)
        .map_err(|e| (e.to_string(), 1))?;
    let prompt = prompt_text(&book, &path, &run_file, rehearse);
    // Drop the runbook tokens and --rehearse from the back, then put the
    // prompt where --runbook stood.
    let mut drop: Vec<usize> = (at..at + width).collect();
    if let Some(r) = rehearse_at {
        drop.push(r);
    }
    drop.sort_unstable();
    for i in drop.into_iter().rev() {
        args.remove(i);
    }
    let insert_at = if rehearse_at.is_some_and(|r| r < at) {
        at - 1
    } else {
        at
    };
    args.insert(insert_at, format!("--message={prompt}"));
    Ok(args)
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
/// Whether the head carries a prompt positional: the name is the first
/// positional unless `--name` names the worker.
fn prompt_positional(head: &[String]) -> bool {
    let named = head
        .iter()
        .any(|t| t == "--name" || t.starts_with("--name="));
    let mut count = 0;
    let mut i = 0;
    while i < head.len() {
        let t = &head[i];
        if t.starts_with('-') {
            if !t.contains('=') && crate::spawn_admission::is_value_flag(t) {
                i += 1;
            }
        } else {
            count += 1;
        }
        i += 1;
    }
    count > usize::from(!named)
}

fn prompt_text(book: &Runbook, book_path: &Path, run_file: &Path, rehearse: bool) -> String {
    let f = run_file.display();
    let mut stop_list = vec!["a check fails", "a command exits nonzero"];
    stop_list.extend(book.stop_when.iter().map(String::as_str));
    let stops = stop_list.join("; ");
    let ids: Vec<&str> = book.sequence().iter().map(|(_, s)| s.id.as_str()).collect();
    let mode = if rehearse {
        "This is a rehearse run: every irreversible step is skipped by the door and still gets its receipt."
    } else {
        "This is a live run: an irreversible step pauses for the user's yes."
    };
    format!(
        "You run the runbook {name} (run file {f}). {mode}\n\
         First read the runbook at {book_path}: its body is your guide. Stop when any of these holds: {stops}.\n\
         Run the steps in this order, each only through the door: {ids}.\n\
         1. For each id run: fno do runbook step {f} <id>. The door runs the command and prints its output. Run it as a background command, so a long step outlives your tool-call limit, and wait for it to exit.\n\
         2. When a step has a check, read the output against it, then run: fno do runbook check {f} <id> --read \"<what you read>\" --verdict pass|fail [--count name=value]...\n\
         3. If the door refuses an irreversible step, ask the user with fno inbox outstanding ask, naming run {run_id} and step <id>, with a first option that starts with Yes. Wait for the answer, then run the step again with --yes <question id>.\n\
         4. On any failed check or nonzero exit, stop. Run: fno do runbook rollback {f}\n\
         5. At the end, or after the rollback, run: fno do runbook close {f}\n\
         Never run a step's command yourself. Never skip a step.",
        name = book.runbook,
        book_path = book_path.display(),
        stops = stops,
        ids = ids.join(", "),
        run_id = run_id_of(run_file),
    )
}

fn run_id_of(run_file: &Path) -> String {
    run_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

fn create_run(
    dir: &Path,
    runbook: &Path,
    sha: &str,
    book: &Runbook,
    rehearse: bool,
    scheduler: &str,
    cwd: &Path,
) -> Result<PathBuf, DoorError> {
    std::fs::create_dir_all(dir).map_err(io("create", dir))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut nonce = [0u8; 2];
    if getrandom::fill(&mut nonce).is_err() {
        nonce = (std::process::id() as u16).to_le_bytes();
    }
    let run_id = format!("rb-{stamp}-{:02x}{:02x}", nonce[0], nonce[1]);
    let path = dir.join(format!("{run_id}.jsonl"));
    let steps: Vec<Value> = book
        .sequence()
        .iter()
        .map(|(phase, s)| json!({"id": s.id, "phase": phase, "irreversible": s.irreversible}))
        .collect();
    let header = json!({
        "kind": "header",
        "run_id": run_id,
        "runbook": runbook.display().to_string(),
        "name": book.runbook,
        "sha256": sha,
        "rehearse": rehearse,
        "scheduler": scheduler,
        "cwd": cwd.display().to_string(),
        "steps": steps,
        "steps_declared": steps.len(),
        "started_at": now(),
    });
    append(&path, &header)?;
    Ok(path)
}

fn append(path: &Path, record: &Value) -> Result<(), DoorError> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io("open run file", path))?;
    writeln!(f, "{record}").map_err(io("write run file", path))
}

// ---------------------------------------------------------------------------
// The receipt door: `fno do runbook step|check|rollback|close <run-file>`.
// ---------------------------------------------------------------------------

/// The run file header, as the seam wrote it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Header {
    run_id: String,
    runbook: String,
    name: String,
    sha256: String,
    rehearse: bool,
    scheduler: String,
    cwd: String,
    steps: Vec<HeaderStep>,
    steps_declared: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeaderStep {
    id: String,
}

/// One run file line. Fields a fold does not read stay unparsed.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Record {
    Header(Header),
    Receipt {
        step: String,
        check: String,
    },
    Check {
        step: String,
        verdict: String,
    },
    Rollback {},
    Close {},
    #[serde(other)]
    Other,
}

/// What the run file says so far.
#[derive(Debug, Default)]
struct RunState {
    header: Header,
    receipts: BTreeMap<String, String>,
    order: Vec<String>,
    pending: Option<String>,
    failed_at: Option<String>,
    rolled_back: bool,
    closed: bool,
}

fn load_run(path: &Path) -> Result<RunState, DoorError> {
    let text = std::fs::read_to_string(path).map_err(io("read run file", path))?;
    let mut st = RunState::default();
    let mut header = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let record: Record = serde_json::from_str(line).map_err(|source| DoorError::BadLine {
            path: path.to_path_buf(),
            source,
        })?;
        match record {
            Record::Header(h) => header = Some(h),
            Record::Receipt { step, check } => {
                if check == "fail" && st.failed_at.is_none() {
                    st.failed_at = Some(step.clone());
                }
                st.pending = (check == "pending").then(|| step.clone());
                st.order.push(step.clone());
                st.receipts.insert(step, check);
            }
            Record::Check { step, verdict } => {
                if verdict == "fail" && st.failed_at.is_none() {
                    st.failed_at = Some(step.clone());
                }
                if st.pending.as_deref() == Some(step.as_str()) {
                    st.pending = None;
                }
                st.receipts.insert(step, verdict);
            }
            Record::Rollback {} => st.rolled_back = true,
            Record::Close {} => st.closed = true,
            Record::Other => {}
        }
    }
    st.header = header.ok_or_else(|| DoorError::NoHeader(path.to_path_buf()))?;
    Ok(st)
}

/// Load the run and its runbook; refuse when the runbook changed under it.
fn open(path: &Path) -> Result<(RunState, Runbook), DoorError> {
    let st = load_run(path)?;
    let (book, sha) = read_runbook(Path::new(&st.header.runbook.clone()))?;
    if sha != st.header.sha256.clone() {
        return refused(format!(
            "the runbook {} changed since this run started; start a new run",
            st.header.runbook.clone()
        ));
    }
    Ok((st, book))
}

/// The door's entry: `args` is the tail after `fno do runbook`.
pub fn cli_run(args: &[String]) -> i32 {
    let action = args.first().map(String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    let result = match action {
        Some("step") => cmd_step(rest),
        Some("check") => cmd_check(rest),
        Some("rollback") => cmd_rollback(rest),
        Some("close") => cmd_close(rest),
        None | Some("--help") | Some("-h") => {
            println!("{USAGE}");
            return 0;
        }
        Some(other) => refused(format!("unknown runbook action {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => 0,
        Err(line) => {
            eprintln!("fno do runbook: {line}");
            2
        }
    }
}

const USAGE: &str = "usage: fno do runbook step <run-file> <step-id> [--yes <question-id>]\n       \
fno do runbook check <run-file> <step-id> --read <text> --verdict pass|fail [--count name=value]...\n       \
fno do runbook rollback <run-file>\n       \
fno do runbook close <run-file>";

fn positional(args: &[String], n: usize, what: &str) -> Result<String, DoorError> {
    args.get(n)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| DoorError::Refused(format!("missing {what}\n{USAGE}")))
}

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn cmd_step(args: &[String]) -> Result<(), DoorError> {
    let file = PathBuf::from(positional(args, 0, "<run-file>")?);
    let id = positional(args, 1, "<step-id>")?;
    let yes = opt(args, "--yes");
    let (st, book) = open(&file)?;
    let approval = gate_step(&st, &book, &id, yes.as_deref(), &|| question_stores(&st))?;
    let seq = book.sequence();
    let Some((phase, step)) = seq.iter().find(|(_, s)| s.id == id) else {
        return refused(format!("no step {id:?} in this runbook"));
    };
    if step.irreversible && st.header.rehearse {
        append(
            &file,
            &json!({"kind": "receipt", "step": id, "phase": phase, "command": step.run,
                    "exit": null, "check": "skipped", "skipped": "rehearse", "at": now()}),
        )?;
        println!("{id}: skipped (rehearse run, irreversible step); receipt written");
        return Ok(());
    }
    let cwd = PathBuf::from(st.header.cwd.clone());
    let (exit, tail) = run_command(
        &step.run,
        &cwd,
        step.timeout_s.unwrap_or(DEFAULT_STEP_TIMEOUT_S),
    );
    let check = if exit != Some(0) {
        "fail"
    } else if step.check.is_some() {
        "pending"
    } else {
        "pass"
    };
    append(
        &file,
        &json!({"kind": "receipt", "step": id, "phase": phase, "command": step.run,
                "exit": exit, "output_tail": tail, "check": check,
                "check_text": step.check, "approval": approval, "at": now()}),
    )?;
    println!("{tail}");
    match check {
        "fail" => println!(
            "{id}: exit {} - the run stops here. Next: fno do runbook rollback {}",
            exit.map_or("none (timed out)".to_string(), |c| c.to_string()),
            file.display()
        ),
        "pending" => println!(
            "{id}: exit 0. Check: {}. Next: fno do runbook check {} {id} --read \"...\" --verdict pass|fail",
            step.check.as_deref().unwrap_or_default(),
            file.display()
        ),
        _ => println!("{id}: exit 0, pass"),
    }
    Ok(())
}

/// The step gate, pure over the run state: order, a failed check, a pending
/// check, and the irreversible pause. `Ok` carries the approval record.
fn gate_step(
    st: &RunState,
    book: &Runbook,
    id: &str,
    yes: Option<&str>,
    stores: &dyn Fn() -> String,
) -> Result<Value, DoorError> {
    if st.closed {
        return refused("this run is closed".into());
    }
    if let Some(at) = &st.failed_at {
        return refused(format!(
            "the check at {at} failed, so no later step runs. Next: fno do runbook rollback <run-file>"
        ));
    }
    if let Some(p) = &st.pending {
        return refused(format!(
            "record the check for {p} first: fno do runbook check <run-file> {p} --read \"...\" --verdict pass|fail"
        ));
    }
    let seq = book.sequence();
    let next = seq
        .iter()
        .map(|(_, s)| s)
        .find(|s| !st.receipts.contains_key(&s.id));
    let Some(next) = next else {
        return refused("every step has its receipt. Next: fno do runbook close <run-file>".into());
    };
    if next.id != id {
        return refused(format!(
            "out of order: the next step is {}, not {id}",
            next.id
        ));
    }
    if !next.irreversible || st.header.rehearse {
        return Ok(Value::Null);
    }
    let run_id = st.header.run_id.clone();
    let Some(qid) = yes else {
        return refused(format!(
            "{id} is irreversible and needs the user's yes. Ask with fno inbox outstanding ask, \
             naming run {run_id} and step {id}, then rerun this step with --yes <question id>"
        ));
    };
    yes_verdict(&stores(), qid, &run_id, id)
}

/// A `--yes` holds only for a question that names this run and step, that
/// the user answered themselves (not a delegate or a sink close), and whose
/// answer starts with yes.
fn yes_verdict(stores: &str, qid: &str, run_id: &str, step: &str) -> Result<Value, DoorError> {
    let asked = question_text(stores, qid)
        .ok_or_else(|| format!("question {qid} is not in the question stores"))?;
    if !names_token(&asked, run_id) || !names_token(&asked, step) {
        return refused(format!(
            "question {qid} does not name run {run_id} and step {step}; ask one that does"
        ));
    }
    if !crate::attention::user_answered(stores).contains(qid) {
        return refused(format!(
            "question {qid} has no answer from the user yet; wait for it"
        ));
    }
    let closes = crate::attention::closes(stores);
    let answer = closes
        .get(qid)
        .map(|c| c.answer.trim().to_string())
        .unwrap_or_default();
    if !answer.to_ascii_lowercase().starts_with("yes") {
        return refused(format!(
            "question {qid} was answered {answer:?}, not yes; the step does not run"
        ));
    }
    Ok(json!({"question": qid, "answer": answer,
              "closed_by": closes.get(qid).map(|c| c.closed_by.clone())}))
}

/// Whether `text` names `token` as a whole id: the characters on both sides
/// are not id characters, so step `visits` is not named by `visits-dry`.
fn names_token(text: &str, token: &str) -> bool {
    let id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    !token.is_empty()
        && text.match_indices(token).any(|(i, _)| {
            let before = text.get(..i).and_then(|s| s.chars().next_back());
            let after = text.get(i + token.len()..).and_then(|s| s.chars().next());
            !before.is_some_and(id_char) && !after.is_some_and(id_char)
        })
}

#[derive(Deserialize)]
struct AskRow {
    #[serde(rename = "type")]
    kind: String,
    data: AskData,
}

#[derive(Deserialize)]
struct AskData {
    question_id: String,
    #[serde(default)]
    question: String,
}

/// The newest `operator_question` text for one question id.
fn question_text(stores: &str, qid: &str) -> Option<String> {
    stores
        .lines()
        .filter(|l| l.contains("operator_question") && l.contains(qid))
        .filter_map(|l| serde_json::from_str::<AskRow>(l).ok())
        .filter(|r| r.kind == "operator_question" && r.data.question_id == qid)
        .map(|r| r.data.question)
        .last()
}

fn question_stores(st: &RunState) -> String {
    let home = crate::paths::AgentsHome::from_env();
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let cwd = PathBuf::from(st.header.cwd.clone());
    let query = crate::event_store::EventQuery::of_types(&[
        "operator_question",
        "operator_question_closed",
        "attention_answer",
    ]);
    let mut paths = crate::needs::question_journals(&fno_dir, &cwd);
    if let Ok(here) = std::env::current_dir() {
        paths.push(crate::paths::space_dir(&here).join("events.jsonl"));
    }
    paths.dedup();
    let mut out = String::new();
    for path in paths {
        if let Ok(text) = crate::event_store::journal_text_checked(&path, &query) {
            out.push_str(&text);
            out.push('\n');
        }
    }
    out
}

/// Run one command under `/bin/sh -c` in `cwd`, bounded with the process
/// group kill. `None` exit means it timed out or never started.
fn run_command(command: &str, cwd: &Path, timeout_s: u64) -> (Option<i32>, String) {
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(command).current_dir(cwd);
    let mut child = match crate::bounded_spawn::spawn_bounded_command(cmd) {
        Ok(c) => c,
        Err(kind) => return (None, format!("the command did not start: {kind:?}")),
    };
    fn drain<R: std::io::Read + Send + 'static>(
        pipe: Option<R>,
    ) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                if p.read_to_end(&mut buf).is_err() {
                    buf.extend_from_slice(b"(output unreadable)");
                }
            }
            buf
        })
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                crate::bounded_spawn::kill_process_group(&mut child);
                break None;
            }
        }
    };
    let mut text = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    text.push_str(&String::from_utf8_lossy(&err.join().unwrap_or_default()));
    if code.is_none() {
        text.push_str(&format!(
            "\n(timed out after {timeout_s}s; process group killed)"
        ));
    }
    (code, tail(&text))
}

// The slices below index at bounds the walk just computed.
#[allow(clippy::indexing_slicing)]
fn tail(text: &str) -> String {
    if text.len() <= OUTPUT_TAIL_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - OUTPUT_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn cmd_check(args: &[String]) -> Result<(), DoorError> {
    let file = PathBuf::from(positional(args, 0, "<run-file>")?);
    let id = positional(args, 1, "<step-id>")?;
    let read = opt(args, "--read").ok_or("--read <what you read> is required")?;
    let verdict = opt(args, "--verdict").ok_or("--verdict pass|fail is required")?;
    if verdict != "pass" && verdict != "fail" {
        return refused(format!("--verdict takes pass or fail, got {verdict:?}"));
    }
    let mut counts = serde_json::Map::new();
    for (i, a) in args.iter().enumerate() {
        if a == "--count" {
            let kv = args.get(i + 1).ok_or("--count needs name=value")?;
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| format!("--count takes name=value, got {kv:?}"))?;
            let value = v
                .parse::<i64>()
                .map(Value::from)
                .unwrap_or_else(|_| Value::from(v));
            counts.insert(k.to_string(), value);
        }
    }
    let st = load_run(&file)?;
    if st.pending.as_deref() != Some(id.as_str()) {
        return refused(format!("{id} has no check waiting for its read"));
    }
    append(
        &file,
        &json!({"kind": "check", "step": id, "read": read, "verdict": verdict,
                "counts": counts, "at": now()}),
    )?;
    if verdict == "fail" {
        println!(
            "{id}: check failed - the run stops. Next: fno do runbook rollback {}",
            file.display()
        );
    } else {
        println!("{id}: check passed");
    }
    Ok(())
}

fn cmd_rollback(args: &[String]) -> Result<(), DoorError> {
    let file = PathBuf::from(positional(args, 0, "<run-file>")?);
    let (st, book) = open(&file)?;
    if st.closed {
        return refused("this run is closed".into());
    }
    if st.failed_at.is_none() {
        return refused("no check failed, so there is nothing to roll back".into());
    }
    if st.rolled_back {
        return refused("the rollback already ran".into());
    }
    match &book.rollback {
        Rollback::Text(_) => {
            let why = book.rollback.none_reason().unwrap_or_default();
            append(
                &file,
                &json!({"kind": "rollback", "step": null, "note": format!("none - {why}"), "at": now()}),
            )?;
            println!("rollback: none - {why}");
        }
        Rollback::Steps(steps) => {
            let cwd = PathBuf::from(st.header.cwd.clone());
            for r in steps {
                let (exit, tail) = run_command(&r.run, &cwd, DEFAULT_STEP_TIMEOUT_S);
                append(
                    &file,
                    &json!({"kind": "rollback", "step": r.id, "command": r.run,
                            "exit": exit, "output_tail": tail, "at": now()}),
                )?;
                println!("rollback {}: exit {exit:?}", r.id);
            }
        }
    }
    println!("Next: fno do runbook close {}", file.display());
    Ok(())
}

fn cmd_close(args: &[String]) -> Result<(), DoorError> {
    let file = PathBuf::from(positional(args, 0, "<run-file>")?);
    let st = load_run(&file)?;
    if st.closed {
        return refused("this run is already closed".into());
    }
    if st.failed_at.is_some() && !st.rolled_back {
        return refused(format!(
            "the check at {} failed; run the rollback first: fno do runbook rollback {}",
            st.failed_at.as_deref().unwrap_or_default(),
            file.display()
        ));
    }
    let declared = st.header.steps_declared;
    let receipts = st.order.len();
    let next = st
        .header
        .steps
        .iter()
        .map(|s| s.id.as_str())
        .find(|id| !st.receipts.contains_key(*id))
        .map(str::to_string);
    let stopped_at = st.failed_at.clone().or(next);
    let verdict = match (&st.failed_at, &stopped_at) {
        (Some(at), _) => format!("stopped at {at}, rolled back"),
        (None, Some(at)) => format!("incomplete, stopped before {at}"),
        (None, None) => "complete".to_string(),
    };
    let scheduler = st.header.scheduler.clone();
    let body = format!(
        "runbook {} run {}: {verdict}; {receipts} of {declared} receipts; run file {}",
        st.header.name.clone(),
        st.header.run_id.clone(),
        file.display()
    );
    let mailed = !scheduler.is_empty()
        && crate::lead_mail::send_to_name(MAIL_SENDER, &scheduler, &body).is_ok();
    append(
        &file,
        &json!({"kind": "close", "steps_declared": declared, "receipts": receipts,
                "stopped_at": stopped_at, "rolled_back": st.rolled_back,
                "verdict": verdict, "mailed": mailed, "at": now()}),
    )?;
    println!("{body}; mailed: {mailed}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "---\nrunbook: promote-local-to-prod\ncwd: /tmp\npreflight:\n  - id: local-db\n    run: \"true\"\nsteps:\n  - id: facdetail-dry\n    run: \"true\"\n    check: the dry-run line reports a count\n  - id: facdetail\n    run: \"true\"\n    irreversible: true\nstop_when:\n  - any check fails\nrollback: none - every upsert is COALESCE or ON CONFLICT DO NOTHING\n---\n# prose\n";

    #[test]
    fn the_example_parses_into_the_typed_contract() {
        let book = parse(EXAMPLE).expect("parses");
        assert_eq!(book.preflight.len(), 1);
        assert_eq!(book.steps.len(), 2);
        assert_eq!(book.steps.iter().filter(|s| s.irreversible).count(), 1);
        assert_eq!(
            book.rollback.none_reason(),
            Some("every upsert is COALESCE or ON CONFLICT DO NOTHING")
        );
    }

    #[test]
    fn a_bad_runbook_refuses_naming_the_field() {
        let dup = EXAMPLE.replace("id: facdetail\n", "id: facdetail-dry\n");
        assert_eq!(
            parse(&dup).unwrap_err(),
            RunbookError::DuplicateId("facdetail-dry".into())
        );
        let unknown = EXAMPLE.replace("stop_when:", "stopwhen:");
        let err = parse(&unknown).unwrap_err().to_string();
        assert!(err.contains("stopwhen"), "{err}");
        let text = EXAMPLE.replace("rollback: none - ", "rollback: nothing - ");
        assert!(matches!(
            parse(&text).unwrap_err(),
            RunbookError::RollbackText(_)
        ));
    }

    fn fixture(dir: &Path, steps: &str, rehearse: bool) -> PathBuf {
        let book_path = dir.join("book.md");
        let text = format!(
            "---\nrunbook: fixture\ncwd: {}\nsteps:\n{steps}rollback:\n  - id: undo\n    run: \"true\"\n---\n",
            dir.display()
        );
        std::fs::write(&book_path, &text).unwrap();
        let book = parse(&text).unwrap();
        create_run(
            &dir.join("runs"),
            &book_path,
            &sha256_hex(&text),
            &book,
            rehearse,
            "",
            dir,
        )
        .unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_failed_step_stops_the_run_and_the_rollback_runs() {
        let dir = tempfile::tempdir().unwrap();
        let run = fixture(
            dir.path(),
            "  - id: a\n    run: \"true\"\n  - id: b\n    run: \"false\"\n  - id: c\n    run: \"true\"\n",
            false,
        );
        let f = run.display().to_string();
        assert_eq!(cli_run(&s(&["step", &f, "b"])), 2, "out of order refuses");
        assert_eq!(cli_run(&s(&["step", &f, "a"])), 0);
        assert_eq!(cli_run(&s(&["step", &f, "b"])), 0);
        assert_eq!(
            cli_run(&s(&["step", &f, "c"])),
            2,
            "a step after a fail refuses"
        );
        assert_eq!(
            cli_run(&s(&["close", &f])),
            2,
            "close waits for the rollback"
        );
        assert_eq!(cli_run(&s(&["rollback", &f])), 0);
        assert_eq!(cli_run(&s(&["close", &f])), 0);
        let st = load_run(&run).unwrap();
        assert_eq!(st.order, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(st.failed_at.as_deref(), Some("b"));
        assert!(st.rolled_back && st.closed);
    }

    #[test]
    fn an_irreversible_step_pauses_without_a_user_yes() {
        let dir = tempfile::tempdir().unwrap();
        let steps = "  - id: push\n    run: \"true\"\n    irreversible: true\n";
        let live = fixture(dir.path(), steps, false);
        let (st, book) = open(&live).unwrap();
        let err = gate_step(&st, &book, "push", None, &String::new)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fno inbox outstanding ask"), "{err}");
        assert_eq!(
            cli_run(&s(&["step", &live.display().to_string(), "push"])),
            2
        );
        assert!(
            load_run(&live).unwrap().order.is_empty(),
            "no receipt written"
        );
        let rehearse = fixture(dir.path(), steps, true);
        assert_eq!(
            cli_run(&s(&["step", &rehearse.display().to_string(), "push"])),
            0
        );
        assert_eq!(
            load_run(&rehearse).unwrap().receipts,
            BTreeMap::from([("push".to_string(), "skipped".to_string())])
        );
    }

    #[test]
    fn a_yes_holds_only_for_the_users_own_answer_naming_the_step() {
        let ask = r#"{"type":"operator_question","data":{"question_id":"D1","question":"run rb-1 step push: yes?"}}"#;
        let user = r#"{"type":"attention_answer","data":{"item_id":"D1","delegate":false}}"#;
        let closed = r#"{"type":"operator_question_closed","data":{"question_id":"D1","answer":"Yes, run it.","closed_by":"s"}}"#;
        let all = format!("{ask}\n{user}\n{closed}\n");
        assert!(yes_verdict(&all, "D1", "rb-1", "push").is_ok());
        assert!(
            yes_verdict(&format!("{ask}\n{closed}\n"), "D1", "rb-1", "push").is_err(),
            "no user answer"
        );
        assert!(
            yes_verdict(&all, "D1", "rb-2", "push").is_err(),
            "another run"
        );
        assert!(
            yes_verdict(&all, "D1", "rb-1", "pus").is_err(),
            "a step id inside a longer id"
        );
        let delegated = all.replace("\"delegate\":false", "\"delegate\":true");
        assert!(
            yes_verdict(&delegated, "D1", "rb-1", "push").is_err(),
            "a delegate's answer"
        );
        let no = all.replace("Yes, run it.", "No.");
        assert!(yes_verdict(&no, "D1", "rb-1", "push").is_err(), "a no");
    }

    #[test]
    fn the_spawn_line_swaps_runbook_for_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("b.md");
        std::fs::write(&book, EXAMPLE).unwrap();
        let p = book.display().to_string();
        assert!(absolutize(&s(&["r1", "--runbook", "missing.md"])).is_err());
        assert_eq!(
            absolutize(&s(&["r1", "--runbook", &p])).unwrap().get(2),
            Some(&p)
        );
        let refused = resolve_runbook(s(&["spawn", "r1", "a prompt", "--runbook", &p]));
        assert!(refused.unwrap_err().0.contains("--runbook is the prompt"));
        assert!(resolve_runbook(s(&["spawn", "--rehearse", "r1"])).is_err());
    }
}
