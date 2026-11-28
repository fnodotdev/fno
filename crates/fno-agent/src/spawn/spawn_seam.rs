//! The spawn seam, in process. Every `spawn` argv passes here before the
//! client parses it: [`crate::spawn_admission::admit`] (the seed refusals, the
//! node seed rewrite, normalization, the compose and the account pick), then
//! the duties the Python front door ran around it before it exec'd this
//! client: the codex git-grant refusal, the loop gate, the worker
//! writable-dir export, the codex sandbox probe, the worktree-policy pin and
//! the ambient identity scrub. A non-spawn argv passes through untouched.

use crate::harness::{AsHarness, Harness};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::spawn_admission::{flag_value, has_flag, seed_of, HARNESS_FLAGS};

/// The env carrier the worker argv builders read the write grants from.
pub const WORKER_ADD_DIRS_ENV: &str = "FNO_WORKER_ADD_DIRS";
/// Set on a sealed eval worker's spawn, so it carries no state grant.
pub const EVALS_SEALED_ENV: &str = "FNO_EVALS_SEALED";

/// The spawn gate's exit for a sandbox that blocks a tool the worker needs.
pub const EXIT_SANDBOX_UNREACHABLE: i32 = 85;

#[derive(Debug)]
pub struct SpawnTiming {
    entered: Instant,
    seam_ms: u64,
    gate_ms: u64,
    guard_ms: u64,
    queued_ms: u64,
}

impl SpawnTiming {
    pub fn for_spawn(args: &[String]) -> Option<Self> {
        (args.first().map(String::as_str) == Some("spawn")).then(|| Self {
            entered: Instant::now(),
            seam_ms: 0,
            gate_ms: 0,
            guard_ms: 0,
            queued_ms: 0,
        })
    }

    pub fn record_seam(&mut self) {
        self.seam_ms = duration_ms(self.entered.elapsed());
    }

    pub fn record_gate(&mut self, gate: Duration, queued: Duration) {
        self.gate_ms = self.gate_ms.saturating_add(duration_ms(gate));
        self.queued_ms = self.queued_ms.saturating_add(duration_ms(queued));
    }

    pub fn record_guard(&mut self, guard: Duration) {
        self.guard_ms = self.guard_ms.saturating_add(duration_ms(guard));
    }

    pub fn client_stage_ms(&self) -> serde_json::Value {
        serde_json::json!({
            "seam": self.seam_ms,
            "gate": self.gate_ms,
            "guard": self.guard_ms,
            "queued": self.queued_ms,
        })
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Admit one argv. `Ok` is the argv to parse; `Err` is the exit code of a
/// refusal whose lines are already printed.
pub fn admit_spawn(args: Vec<String>) -> Result<Vec<String>, i32> {
    if args.first().map(String::as_str) != Some("spawn") {
        return Ok(args);
    }
    // The positional boundary reads the CALLER's raw spelling, at the seam's
    // front, before any rewrite: resolve_prompt_file splices --message=<text>
    // and normalize injects --name and reorders the tail, so a later check
    // would judge a line the caller never typed. Refusals quote the caller's
    // own tokens; the fenced form the rewrite returns flows through the
    // prompt-file resolve, normalize and the parse unchanged.
    let Some(tail) = args.get(1..) else {
        // ["spawn"] alone: nothing to rewrite.
        return Ok(args);
    };
    // The + spelling folds before the boundary reads the line, so the
    // boundary, the parse and the daemon vocabulary answer one grammar
    // (the composer converts a picked +word to --word the same way).
    let folded = crate::spawn_bare::plus_fold(tail);
    let args = match crate::spawn_bare::boundary_rewrite(&folded) {
        crate::spawn_bare::Boundary::Fence(rewritten) => {
            let mut out = Vec::with_capacity(rewritten.len() + 1);
            out.extend(args.first().cloned());
            out.extend(rewritten);
            out
        }
        crate::spawn_bare::Boundary::Keep => {
            let mut out = Vec::with_capacity(folded.len() + 1);
            out.extend(args.first().cloned());
            out.extend(folded);
            out
        }
        crate::spawn_bare::Boundary::Refuse(line) => {
            eprintln!("fno agent spawn: {line}");
            return Err(2);
        }
    };
    let args = crate::spawn::runbook::resolve_prompts(args).map_err(|(reason, code)| {
        eprintln!("{reason}");
        code
    })?;
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    refuse_codex_without_git_grant(&args, &here)?;

    let home = crate::paths::AgentsHome::from_env();
    let entries: Vec<crate::state::RegistryEntry> =
        crate::state::load_registry(&home.registry_json())
            .map(|r| r.entries)
            .unwrap_or_default();
    let mut names: HashSet<String> = entries.iter().map(|e| e.name.clone()).collect();
    names.extend(handover_worker_names(None));
    let mut args = args;
    if let Some(line) = resume_route_pin(&mut args, &entries) {
        eprintln!("{line}");
        return Err(2);
    }
    let typed = flag_value(&args[1..], &["--name"]).is_some();
    // `--print` launches nothing, so it reserves nothing.
    let print = has_flag(&args[1..], "", &["--print"]);
    let admitted = loop {
        let admitted = crate::spawn_admission::admit(&args, &here, &names);
        let Some(name) = reservable_name(&admitted, print) else {
            break admitted;
        };
        match reserve_name(&name, None) {
            Ok(()) => break admitted,
            // A minted name another spawn holds: mint the next suffix. The
            // mint gives up past `-5`, and that refusal ends the loop.
            Err(_) if !typed && names.insert(name.clone()) => continue,
            Err(holder) => {
                eprintln!(
                    "fno agent spawn: the name {name} is reserved by {holder}, a spawn still \
                     inside its launch window; pass another --name or wait for that spawn"
                );
                return Err(2);
            }
        }
    };
    for line in &admitted.stderr {
        eprintln!("{line}");
    }
    if let Some(stdout) = &admitted.stdout {
        println!("{}", crate::research::pyfmt::json_dumps(stdout));
    }
    if let Some(code) = admitted.exit {
        return Err(code);
    }
    for event in &admitted.events {
        let fields: Vec<(&str, serde_json::Value)> = event
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        crate::claude_ask::emit_event(&home.events_jsonl(), "model_vendor_mismatch", &fields);
    }
    let argv = admitted.argv;
    if let Some(refusal) = gate_seed(&argv)
        .and_then(|seed| crate::capability_leaves::loop_refusal(&spawn_harness(&argv), &seed, None))
    {
        eprintln!("{refusal}");
        return Err(2);
    }

    let cwd = spawn_cwd(&argv, &here);
    export_worker_dirs(&cwd);
    refuse_codex_unreachable_tools(&argv, &cwd)?;
    pin_worktree_policy(&argv, &here);
    // The harness inference reads the live environment, which loses the markers
    // below, so the inferred harness rides the argv first.
    let mut argv = argv;
    if flag_value(&argv[1..], HARNESS_FLAGS).is_none() {
        let harness = spawn_harness(&argv);
        argv.splice(1..1, ["--harness".to_string(), harness]);
    }
    crate::claims::take_caller_identity(&crate::pane_spawn::env_wrap::AMBIENT_IDENTITY_ENV);
    Ok(argv)
}

/// The name an admitted spawn must reserve. A refusal carries an empty argv,
/// so it reserves nothing and its reason prints.
fn reservable_name(admitted: &crate::spawn_admission::Admission, print: bool) -> Option<String> {
    if admitted.exit.is_some() || print {
        return None;
    }
    flag_value(admitted.argv.get(1..).unwrap_or(&[]), &["--name"]).filter(|n| !n.is_empty())
}

/// The resume ask's route pin: a spawn whose argv resumes a session
/// launches the transcript's own harness and model, never a profile lane's
/// pick. The row answers by `--name`/first positional, then by the resumed
/// session id, the same order `revival-check` reads. No row (a fresh
/// transcript resume) leaves the spawn untouched. The axes the caller left
/// free take the row's values, so the slot walk sees them occupied and stands
/// down; a harness that contradicts the row refuses with the corrected
/// command. `--route`/`-m` the caller named are decisions and stay.
fn resume_route_pin(
    args: &mut Vec<String>,
    entries: &[crate::state::RegistryEntry],
) -> Option<String> {
    let tail = || args.get(1..).unwrap_or(&[]);
    let resume_id = flag_value(tail(), &["--resume", "-r"]).filter(|v| !v.trim().is_empty())?;
    let name = flag_value(tail(), &["--name"])
        .filter(|v| !v.trim().is_empty())
        .or_else(|| seed_of(tail()));
    let row = name
        .as_deref()
        .and_then(|n| entries.iter().find(|e| e.name == n))
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.harness_session_id.as_deref() == Some(resume_id.as_str()))
        })?;
    let row_harness = row
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let given_harness = flag_value(tail(), HARNESS_FLAGS).map(|v| v.trim().to_string());
    // The pin occupies only axes the caller left free: `--route`/`-m` already
    // on the argv are decisions, and the model pin is what a profile lane
    // would otherwise resolve.
    let model_free = flag_value(tail(), &["--model", "-m"]).is_none();
    let route_free = flag_value(tail(), &["--route"]).is_none();
    let row_model = row
        .requested_model
        .as_deref()
        .filter(|m| !m.is_empty())
        .or_else(|| row.model.as_deref().filter(|m| !m.is_empty()));
    // One shared route-or-model resolution feeds the injection and the
    // refusal's corrected command: a declared lane serving this model on this
    // harness carries the account and route too, so the pin rides
    // `--route <lane>`; the lane gate refuses a bare `-m` for exactly that
    // reason.
    let axis_pin: Option<(String, String)> = if model_free && route_free {
        match row_model {
            Some(m) => crate::route_gather::lanes_serving(row_harness, m, &cwd)
                .into_iter()
                .next()
                .map(|lane| ("--route".to_string(), lane))
                .or_else(|| Some(("-m".to_string(), m.to_string()))),
            None => None,
        }
    } else {
        None
    };

    if let Some(given) = given_harness.as_deref() {
        if given != row_harness {
            return Some(resume_route_refusal(row, &resume_id, given, &axis_pin));
        }
    }
    // Mutation last: `tail` borrows `args`, and every read is done.
    if given_harness.is_none() {
        args.insert(1, "--harness".to_string());
        args.insert(2, row_harness.to_string());
    }
    if let Some((flag, value)) = axis_pin {
        args.insert(1, flag);
        args.insert(2, value);
    }
    None
}

/// The refusal for a resume spawn whose harness contradicts the row. It
/// carries the working command, not a principle: the paste-back launches the
/// transcript on its own axes.
fn resume_route_refusal(
    row: &crate::state::RegistryEntry,
    resume_id: &str,
    given: &str,
    axis_pin: &Option<(String, String)>,
) -> String {
    let quote = crate::client_commands::shlex_quote;
    let row_harness = row.harness.as_deref().unwrap_or_default();
    let route_or_model = axis_pin
        .as_ref()
        .map(|(flag, value)| format!("{flag} {}", quote(value)))
        .unwrap_or_default();
    let cwd_flag = if row.cwd.is_empty() {
        String::new()
    } else {
        format!("--cwd {}", quote(&row.cwd))
    };
    format!(
        "fno agent spawn: --resume {} resumes a {} transcript; -H {} would launch it on the \
         wrong harness. The corrected form: fno agent spawn --name {} --resume={} {} -H {} {} \
         --substrate thread",
        quote(resume_id),
        quote(row_harness),
        quote(given),
        quote(&row.name),
        quote(resume_id),
        cwd_flag,
        quote(row_harness),
        route_or_model,
    )
}

/// Worker names that a live `spawn-handover:<name>` window claim holds on a
/// node. The name reservation below dies with its client and the registry row
/// lands seconds later, so a second spawn in that gap reads the name free and
/// writes the same holder on its own node. The window claim outlives both.
pub(crate) fn handover_worker_names(root: Option<&Path>) -> HashSet<String> {
    crate::claims::list(Some("node:"), root, false)
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            r.holder
                .strip_prefix(crate::spawn_guard::HANDOVER_HOLDER_PREFIX)
        })
        .map(str::to_string)
        .collect()
}

/// Reserve a worker name while this spawn client runs. The registry holds no
/// row until the child boots (p50 8 s), so two concurrent spawns of one seed
/// both read a name free; a claim decides the race. The claim names this
/// process's pid and takes no TTL, so it reads stale the moment the client
/// exits and needs no release. The key is not `name:`, which marks a lead's
/// drawn person name used forever. A broken claims store fails open to the
/// registry read. `Err` names the holder.
pub(crate) fn reserve_name(name: &str, root: Option<&Path>) -> Result<(), String> {
    let key = format!("spawn-name:{}", name.to_ascii_lowercase());
    let holder = format!(
        "{}{}",
        crate::spawn_guard::SPAWN_CLI_HOLDER_PREFIX,
        std::process::id()
    );
    let opts = crate::claims::AcquireOpts {
        reason: Some(format!("spawn launch window for worker {name}")),
        root: root.map(Path::to_path_buf),
        ..Default::default()
    };
    match crate::claims::acquire(&key, &holder, opts) {
        crate::claims::AcquireOutcome::HeldByOther { holder, .. } => Err(holder),
        crate::claims::AcquireOutcome::Error(e) => {
            eprintln!("spawn note: {key} could not be reserved ({e}); the registry read decides");
            Ok(())
        }
        crate::claims::AcquireOutcome::Acquired(_) => Ok(()),
    }
}

/// The spawn's effective working directory: `--cwd`/`-c`, else the caller's.
pub(crate) fn spawn_cwd(args: &[String], here: &Path) -> PathBuf {
    flag_value(&args[1..], &["--cwd", "-c"])
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| here.to_path_buf())
}

/// The seed the loop gate judges: the inline message, else the contents of a
/// `--prompt-file` path. `-` (stdin) is left for the parse to read once.
fn gate_seed(args: &[String]) -> Option<String> {
    seed_of(&args[1..]).or_else(|| {
        flag_value(&args[1..], &["--prompt-file"])
            .filter(|p| !p.is_empty() && p != "-")
            .and_then(|p| std::fs::read_to_string(p).ok())
    })
}

/// The harness a spawn dispatches to: the flag, else the ambient harness,
/// else claude.
fn spawn_harness(args: &[String]) -> String {
    flag_value(&args[1..], HARNESS_FLAGS)
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .or_else(|| crate::claims::resolve_harness().map(|h| h.to_lowercase()))
        .unwrap_or_else(|| "claude".to_string())
}

/// A codex spawn whose payload is a code workflow: a `--node` dispatch, or a
/// token whose first word names a fno command. The scan reads every token,
/// the fenced seed included.
fn is_codex_code_payload(args: &[String]) -> bool {
    let tail = &args[1..];
    if flag_value(tail, &["--node"]).is_some_and(|n| !n.is_empty()) {
        return true;
    }
    let mut i = 0;
    while i < tail.len() {
        let mut token = tail[i].as_str();
        if token == "--message" && i + 1 < tail.len() {
            token = tail[i + 1].as_str();
        } else if let Some(v) = token.strip_prefix("--message=") {
            token = v;
        }
        if token
            .split_whitespace()
            .next()
            .is_some_and(|w| crate::pane_spawn::codex::CODE_PAYLOAD_PREFIXES.contains(&w))
        {
            return true;
        }
        i += 1;
    }
    false
}

/// A codex code spawn that will run under a bounded sandbox. A bypass
/// posture, a promoted spawn and the headless lane run with full access, so
/// neither the git grant nor the probe applies to them.
fn is_bounded_codex_code_spawn(args: &[String]) -> bool {
    let tail = &args[1..];
    if spawn_harness(args).as_harness() != Harness::Codex || !is_codex_code_payload(args) {
        return false;
    }
    if has_flag(
        args,
        "-Y",
        &["--yolo", "--dangerously-bypass-approvals-and-sandbox"],
    ) {
        return false;
    }
    let promoted = has_flag(args, "", &["--promote"])
        || (flag_value(tail, &["--role-level"]).is_some_and(|v| !v.is_empty())
            && flag_value(tail, &["--role-scope"]).is_some_and(|v| !v.is_empty()));
    if promoted {
        return false;
    }
    let headless = has_flag(args, "", &["--once"])
        || flag_value(tail, &["--substrate"])
            .is_some_and(|s| s.trim().to_lowercase() == "headless");
    if headless {
        return false;
    }
    let sandbox = flag_value(tail, &["--permission-mode"]).unwrap_or_default();
    let sandbox = sandbox
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    !["yolo", "bypasspermissions", "danger-full-access"].contains(&sandbox.as_str())
}

/// A bounded codex code spawn with no git common dir refuses before any row.
fn refuse_codex_without_git_grant(args: &[String], here: &Path) -> Result<(), i32> {
    if !is_bounded_codex_code_spawn(args) {
        return Ok(());
    }
    let cwd = spawn_cwd(args, here);
    if crate::provider::git_common_dir(&cwd).is_some() {
        return Ok(());
    }
    eprintln!(
        "fno agent spawn: Codex code payloads require a resolved git grant; cwd={}; \
         git rev-parse --git-common-dir failed; no worker launched",
        cwd.to_string_lossy()
    );
    Err(2)
}

/// A bounded codex code spawn whose sandbox blocks a tool it needs refuses
/// before any row. An unjudged sandbox launches with a named caveat.
fn refuse_codex_unreachable_tools(args: &[String], cwd: &Path) -> Result<(), i32> {
    if !is_bounded_codex_code_spawn(args) {
        return Ok(());
    }
    let mode = flag_value(&args[1..], &["--permission-mode"]);
    let probe = crate::sandbox_probe::probe_codex_sandbox(cwd, mode.as_deref(), &[]);
    if probe.verdict == "unknown" {
        eprintln!(
            "sandbox-probe: could not judge the codex sandbox ({}); launching unprobed",
            probe.note
        );
    }
    for (tool, evidence) in &probe.blocked {
        eprintln!(
            "sandbox-probe: {tool} is unreachable inside the codex workspace-write sandbox \
             ({evidence}); no worker launched, node stays dispatchable"
        );
    }
    if probe.blocked.is_empty() {
        return Ok(());
    }
    eprintln!(
        "remedy: allow it in ~/.codex/config.toml (gh needs [sandbox_workspace_write] \
         network_access = true; git needs the git common dir writable), or dispatch \
         unsandboxed with --yolo"
    );
    Err(EXIT_SANDBOX_UNREACHABLE)
}

/// The write grants a worker spawned at `cwd` needs: the state root, the
/// claim store's root, the two registry directories, this spawn's plan
/// directory, and the vault slice beside that plans directory (the parent
/// that carries `decision-log.md` and the backlog docs). Absolute,
/// deduplicated, existing only, in that order.
pub fn worker_writable_dirs(cwd: &Path) -> Vec<String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    roots.extend(crate::agents_config::state_dir(cwd));
    roots.extend(
        crate::claims_root::global_claims_dir().and_then(|d| d.parent().map(Path::to_path_buf)),
    );
    let home = crate::paths::AgentsHome::from_env();
    for dir in [
        home.registry_json().parent().map(Path::to_path_buf),
        crate::agents_config::state_dir(cwd).map(|s| s.join("agents")),
    ]
    .into_iter()
    .flatten()
    {
        if !roots.iter().any(|root| dir.starts_with(root)) {
            roots.push(dir);
        }
    }
    if let Some(plans) = crate::plans_path::plans_content_dir(cwd) {
        roots.push(plans.clone());
        // The vault slice beside the plans dir: decision_log reads
        // `plans.parent()`, so the ruling file and the backlog docs the
        // worker must reach live there. Two guards keep the parent from
        // widening the grant: the plans leaf contract (require_plans_leaf)
        // and the slice being named for the project (project_name), so a
        // shallow configured plans dir (`~/plans`, a vault root) never
        // grants the tree above it. Under the default sentinel the parent
        // is the space dir inside the state-root grant, and the containment
        // check adds nothing.
        if let Some(slice) = plans.parent().map(Path::to_path_buf) {
            let leaf = plans.file_name().and_then(|n| n.to_str()) == Some("plans");
            let named = crate::plans_path::project_folder(Some(cwd), false)
                .ok()
                .is_some_and(|name| {
                    slice
                        .file_name()
                        .is_some_and(|n| n == std::ffi::OsStr::new(&name))
                });
            if leaf && named && !roots.iter().any(|root| slice.starts_with(root)) {
                roots.push(slice);
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for cand in roots {
        let Ok(resolved) = cand.canonicalize() else {
            continue;
        };
        if !resolved.is_dir() {
            continue;
        }
        let token = resolved.to_string_lossy().into_owned();
        if !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

/// Publish the grants for the worker argv builders. An empty set CLEARS the
/// carrier: a spawn made from inside a worker inherits its parent's value,
/// another project's directories, which must never ride as this one's grant.
///
/// A sealed eval worker gets no grant. An `--add-dir` joins the working
/// directories, so a grant would pass the read fence the worker runs behind.
pub(crate) fn export_worker_dirs(cwd: &Path) {
    let sealed = std::env::var_os(EVALS_SEALED_ENV).is_some_and(|v| v == "1");
    let dirs = granted_dirs(cwd, sealed);
    if dirs.is_empty() {
        std::env::remove_var(WORKER_ADD_DIRS_ENV);
    } else {
        let sep = if cfg!(windows) { ";" } else { ":" };
        std::env::set_var(WORKER_ADD_DIRS_ENV, dirs.join(sep));
    }
}

pub(crate) fn granted_dirs(cwd: &Path, sealed: bool) -> Vec<String> {
    if sealed {
        Vec::new()
    } else {
        worker_writable_dirs(cwd)
    }
}

/// The eval sandbox's Claude config root: `<state dir>/db/evals/claude-config`,
/// or the legacy `<state dir>/evals/claude-config` while it exists. The
/// sealed login is a Keychain item keyed by this path, so the mover leaves
/// the legacy folder in place and this read keeps naming it.
/// A sealed claude eval worker pins this root, so the harness's own state
/// writes (transcripts, history, config) land under the fno eval dir instead
/// of the user's `~/.claude`. `None` when no state dir resolves.
pub fn evals_claude_config_dir() -> Option<PathBuf> {
    let state = crate::backlog::settings::state_dir()?;
    let legacy = state.join("evals").join("claude-config");
    if legacy.is_dir() {
        return Some(legacy);
    }
    let evals = crate::state_layout::place_dir(&state, "evals")?;
    Some(evals.join("claude-config"))
}

/// Stage the sandbox so a pinned worker keeps its ambient context while its
/// state writes land sandbox-side. State files (`.credentials.json` auth,
/// `.claude.json` onboarding, `settings.json`) copy in fresh on every call,
/// so a rotation at the source reaches the next spawn. Context entries
/// (`plugins`, `commands`, `agents`, `skills`, `CLAUDE.md`) symlink through
/// to the ambient ones: a capability worker reads the installed plugin and
/// the user's global memory exactly as an unpinned spawn would, and no
/// eval-time write travels those trees. Each copy lands through a temp-file
/// rename, so concurrent stages of one run's parallel spawns cannot tear it.
/// Returns the sandbox root, or `None` when no state dir resolves; a missing
/// source entry stages nothing for it and never fails the spawn.
pub fn stage_evals_claude_config() -> Option<PathBuf> {
    let dir = evals_claude_config_dir()?;
    let source = crate::claude_drive::ambient_claude_config_root()?;
    std::fs::create_dir_all(&dir).ok()?;
    // A stage interrupted by a dead runner leaves its temp behind; a stage
    // past this age sweeps it. A live stage's temp is seconds old and is
    // never swept mid-write.
    let now = std::time::SystemTime::now();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().contains(".stage-") {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= STAGE_MIN_AGE);
            if stale {
                std::fs::remove_file(entry.path()).ok();
            }
        }
    }
    static STAGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tag = format!(
        "{}-{}",
        std::process::id(),
        STAGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    for name in STAGED_STATE_FILES {
        let from = source.join(name);
        let Ok(bytes) = std::fs::read(&from) else {
            continue;
        };
        let tmp = dir.join(format!("{name}.stage-{tag}"));
        if std::fs::write(&tmp, &bytes).is_ok() {
            std::fs::rename(&tmp, dir.join(name)).ok();
        } else {
            std::fs::remove_file(&tmp).ok();
        }
    }
    for name in MIRRORED_CONTEXT_ENTRIES {
        let from = source.join(name);
        if !from.exists() {
            continue;
        }
        let to = dir.join(name);
        if std::fs::symlink_metadata(&to)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        std::os::unix::fs::symlink(&from, &to).ok();
    }
    Some(dir)
}

/// The state files a pinned worker needs copied to run headless.
const STAGED_STATE_FILES: [&str; 3] = [".credentials.json", ".claude.json", "settings.json"];

/// How old a stage temp must read before a sweep calls it abandoned and not
/// a write still settling (the same bound the Claude temp reaper holds).
const STAGE_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// The context entries a pinned worker reads through symlinks to the ambient
/// root, so a capability worker keeps the installed plugin and the user's
/// global memory. A golden fence changes none of this: it already ran with
/// the user's memory loaded and its plugins off through the settings gate.
const MIRRORED_CONTEXT_ENTRIES: [&str; 5] =
    ["plugins", "commands", "agents", "skills", "CLAUDE.md"];

/// A `--cwd` naming a repo no project declares pins the worktree policy to
/// `never` for this spawn and prints the receipt. An operator-set policy wins.
pub(crate) fn pin_worktree_policy(args: &[String], here: &Path) {
    if std::env::var_os("FNO_WORKTREE_POLICY").is_some_and(|v| !v.is_empty()) {
        return;
    }
    let tail = &args[1..];
    let Some(target) = flag_value(tail, &["--cwd", "-c"]).filter(|c| !c.is_empty()) else {
        return;
    };
    let harness = flag_value(tail, HARNESS_FLAGS).unwrap_or_default();
    let pinned =
        crate::worktree::policy::undeclared_dispatch_pin(Path::new(&target), here, Some(&harness));
    if pinned.is_some() {
        eprintln!("{}", crate::worktree::policy::UNDECLARED_REPO_RECEIPT);
        std::env::set_var("FNO_WORKTREE_POLICY", "never");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    fn resume_row(harness: &str, model: Option<&str>) -> crate::state::RegistryEntry {
        let mut row = crate::state::RegistryEntry::default();
        row.name = "t-resumed".to_string();
        row.harness = Some(harness.to_string());
        row.model = model.map(str::to_string);
        row.cwd = "/wt/t-resumed".to_string();
        row.harness_session_id = Some("11111111-2222-3333-4444-555555555555".to_string());
        row
    }

    #[test]
    fn resume_pin_injects_the_row_harness_and_model_axes_left_free() {
        let row = resume_row("claude", Some("never-a-declared-model"));
        let mut args = argv(&[
            "spawn",
            "--name",
            "t-resumed",
            "--resume=11111111-2222-3333-4444-555555555555",
            "--cwd",
            "/wt/t-resumed",
            "hello",
        ]);
        let refusal = resume_route_pin(&mut args, &[row]);
        assert!(refusal.is_none(), "the pin admits: {refusal:?}");
        let joined = args.join(" ");
        assert!(
            joined.contains("--harness claude"),
            "the row harness pins: {joined}"
        );
        assert!(
            joined.contains("-m never-a-declared-model"),
            "the row model pins when no lane serves it: {joined}"
        );
    }

    #[test]
    fn resume_pin_leaves_a_no_row_resume_untouched() {
        let mut args = argv(&[
            "spawn",
            "--name",
            "t-unknown",
            "--resume=99999999-2999-3333-4444-555555555555",
            "hello",
        ]);
        let refusal = resume_route_pin(&mut args, &[]);
        assert!(refusal.is_none(), "no row admits unchanged: {refusal:?}");
        assert!(
            !args.contains(&"--harness".to_string()),
            "no row, no pin: {:?}",
            args
        );
    }

    #[test]
    fn a_resume_harness_contradiction_refuses_with_the_corrected_command() {
        let row = resume_row("claude", None);
        let mut args = argv(&[
            "spawn",
            "--name",
            "t-resumed",
            "--resume=11111111-2222-3333-4444-555555555555",
            "-H",
            "codex",
            "hello",
        ]);
        let err = resume_route_pin(&mut args, &[row])
            .expect("a harness contradiction refuses with the corrected command");
        assert!(
            err.contains("--resume=11111111-2222-3333-4444-555555555555"),
            "the corrected command carries the glued spelling: {err}"
        );
        assert!(
            err.contains("-H claude") && err.contains("--substrate thread"),
            "the corrected command carries the row harness and thread lane: {err}"
        );
    }

    #[test]
    fn a_name_another_spawn_reserved_refuses_and_names_its_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let rival = |pid: u32| crate::claims::AcquireOpts {
            pid: Some(pid),
            root: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        // A live rival client holds the name: the race refuses and names it.
        let _ = crate::claims::acquire(
            "spawn-name:t-second-pass",
            "spawn-cli:1",
            rival(std::process::id()),
        );
        assert_eq!(
            reserve_name("t-Second-Pass", Some(tmp.path())),
            Err("spawn-cli:1".to_string())
        );
        // A rival client that exited frees the name with no release.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let _ = crate::claims::acquire("spawn-name:t-gone", "spawn-cli:2", rival(dead));
        assert_eq!(reserve_name("t-gone", Some(tmp.path())), Ok(()));
        // The node's window claim outlives that client: its worker name
        // stays taken, so a sibling spawn mints the next suffix.
        let window = crate::claims::AcquireOpts {
            pid_unavailable: true,
            ttl_ms: Some(900_000),
            root: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let _ = crate::claims::acquire("node:x-win", "spawn-handover:t-gone", window);
        assert!(handover_worker_names(Some(tmp.path())).contains("t-gone"));
    }

    #[test]
    fn a_refused_admission_reserves_no_name_and_keeps_its_reason() {
        let refused = crate::spawn_admission::Admission {
            argv: Vec::new(),
            stderr: vec!["fno agent spawn: refused".to_string()],
            exit: Some(2),
            stdout: None,
            take_over: None,
            events: Vec::new(),
        };
        assert_eq!(reservable_name(&refused, false), None);
        assert_eq!(refused.stderr, ["fno agent spawn: refused"]);
        let ok = crate::spawn_admission::Admission {
            argv: argv(&["spawn", "--name", "w1"]),
            exit: None,
            ..refused
        };
        assert_eq!(reservable_name(&ok, false), Some("w1".to_string()));
        assert_eq!(reservable_name(&ok, true), None);
    }

    #[test]
    fn a_sealed_spawn_carries_no_grant_and_any_other_keeps_them() {
        let cwd = Path::new(".");
        assert!(granted_dirs(cwd, true).is_empty());
        assert_eq!(granted_dirs(cwd, false), worker_writable_dirs(cwd));
    }

    /// One env pin that restores the previous value on drop. Scoped here so
    /// the seam test does not depend on another module's fixture.
    struct EnvPin(&'static str, Option<std::ffi::OsString>);

    impl EnvPin {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old = std::env::var_os(name);
            std::env::set_var(name, value);
            EnvPin(name, old)
        }
    }

    impl Drop for EnvPin {
        fn drop(&mut self) {
            match self.1.take() {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }

    #[test]
    fn the_vault_slice_beside_a_vault_plans_dir_is_granted_and_a_covered_parent_is_not() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("fno-seam-vault-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join(".fno")).unwrap();
        let config = base.join("config.toml");

        // Scenario A: a vault plans dir grants the slice beside it.
        let plans = base.join("vault/internal/fno/plans");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(
            &config,
            format!(
                "plans_dir = \"{}\"\n\n[project]\nid = \"fno\"\n",
                plans.display()
            ),
        )
        .unwrap();
        let _cfg = EnvPin::set("FNO_CONFIG", &config);
        let _state = EnvPin::set("FNO_STATE_DIR", &base.join("state"));
        let dirs = worker_writable_dirs(&proj);
        let slice = plans
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            dirs.contains(&slice),
            "the vault slice rides the grant: {dirs:?}"
        );

        // Scenario B: a plans dir inside the state root adds no parent grant.
        let covered = base.join("state/spaces/fno/plans");
        std::fs::create_dir_all(&covered).unwrap();
        std::fs::write(
            &config,
            format!(
                "plans_dir = \"{}\"\n\n[project]\nid = \"fno\"\n",
                covered.display()
            ),
        )
        .unwrap();
        let dirs = worker_writable_dirs(&proj);
        let covered_str = covered
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let slice_str = covered.parent().unwrap().to_string_lossy().into_owned();
        assert!(
            dirs.contains(&covered_str),
            "the covered plans dir still rides: {dirs:?}"
        );
        assert!(
            !dirs.contains(&slice_str),
            "no parent already under a grant joins: {dirs:?}"
        );

        // Scenario C: a plans dir whose leaf is not `plans` grants no parent.
        let shallow = base.join("shallow/plans-dir");
        std::fs::create_dir_all(&shallow).unwrap();
        std::fs::write(
            &config,
            format!(
                "plans_dir = \"{}\"\n\n[project]\nid = \"fno\"\n",
                shallow.display()
            ),
        )
        .unwrap();
        let dirs = worker_writable_dirs(&proj);
        let shallow_str = shallow
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let shallow_parent = shallow
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            dirs.contains(&shallow_str),
            "the plans dir itself still rides: {dirs:?}"
        );
        assert!(
            !dirs.contains(&shallow_parent),
            "no parent of a non-plans-leaf dir joins: {dirs:?}"
        );

        // Scenario D: a plans leaf under a slice not named for the project
        // grants no parent.
        let wide = base.join("wide/plans");
        std::fs::create_dir_all(&wide).unwrap();
        std::fs::write(
            &config,
            format!(
                "plans_dir = \"{}\"\n\n[project]\nid = \"fno\"\n",
                wide.display()
            ),
        )
        .unwrap();
        let dirs = worker_writable_dirs(&proj);
        let wide_str = wide.canonicalize().unwrap().to_string_lossy().into_owned();
        let wide_parent = wide
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            dirs.contains(&wide_str),
            "the plans dir still rides: {dirs:?}"
        );
        assert!(
            !dirs.contains(&wide_parent),
            "no parent outside the project-named slice joins: {dirs:?}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn only_a_bounded_codex_code_spawn_needs_the_grant_and_the_probe() {
        let code = |extra: &[&str]| {
            let mut t = vec!["spawn", "w", "-H", "codex"];
            t.extend_from_slice(extra);
            t.extend_from_slice(&["--", "/fno:target x-1"]);
            is_bounded_codex_code_spawn(&argv(&t))
        };
        assert!(code(&[]));
        assert!(!code(&["--yolo"]));
        assert!(!code(&["--promote"]));
        assert!(!code(&["--once"]));
        assert!(!code(&["--permission-mode", "danger-full-access"]));
        assert!(is_bounded_codex_code_spawn(&argv(&[
            "spawn", "w", "-H", "codex", "--node", "x-1", "--", "hi"
        ])));
        assert!(!is_bounded_codex_code_spawn(&argv(&[
            "spawn", "w", "-H", "codex", "--", "say hi"
        ])));
        assert!(!is_bounded_codex_code_spawn(&argv(&[
            "spawn",
            "w",
            "-H",
            "claude",
            "--",
            "/fno:target x-1"
        ])));
    }

    #[test]
    fn the_loop_gate_judges_a_prompt_file_seed_but_never_reads_stdin() {
        let file = std::env::temp_dir().join(format!("fno-gate-seed-{}.txt", std::process::id()));
        std::fs::write(&file, "/fno:target x-1").unwrap();
        let path = file.to_string_lossy().into_owned();
        assert_eq!(
            gate_seed(&argv(&["spawn", "--name", "w", "--prompt-file", &path])).as_deref(),
            Some("/fno:target x-1")
        );
        assert_eq!(
            gate_seed(&argv(&[
                "spawn",
                "--name",
                "w",
                "--prompt-file",
                &path,
                "--",
                "hi"
            ]))
            .as_deref(),
            Some("hi")
        );
        assert_eq!(
            gate_seed(&argv(&["spawn", "--name", "w", "--prompt-file", "-"])),
            None
        );
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn evals_sandbox_resolves_under_the_state_dir() {
        let state = tempfile::tempdir().expect("tempdir");
        // SAFETY: single-key env write, restored before the test returns; the
        // crate's other env-scoped tests hold the same contract.
        let previous = std::env::var_os("FNO_STATE_DIR");
        unsafe { std::env::set_var("FNO_STATE_DIR", state.path()) };
        assert_eq!(
            evals_claude_config_dir(),
            Some(state.path().join("db/evals/claude-config"))
        );
        match previous {
            Some(v) => unsafe { std::env::set_var("FNO_STATE_DIR", v) },
            None => unsafe { std::env::remove_var("FNO_STATE_DIR") },
        }
    }

    #[test]
    fn staging_copies_the_ambient_auth_files_into_the_sandbox() {
        let state = tempfile::tempdir().expect("tempdir");
        let ambient = tempfile::tempdir().expect("tempdir");
        std::fs::write(ambient.path().join(".credentials.json"), br#"{"auth":1}"#).unwrap();
        std::fs::write(
            ambient.path().join(".claude.json"),
            br#"{"onboarded":true}"#,
        )
        .unwrap();
        std::fs::write(ambient.path().join("settings.json"), br#"{"model":"opus"}"#).unwrap();
        let plugins = ambient.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(ambient.path().join("unrelated.txt"), b"skip").unwrap();
        // SAFETY: same single-key contract as the resolver test above.
        let state_previous = std::env::var_os("FNO_STATE_DIR");
        let config_previous = std::env::var_os("CLAUDE_CONFIG_DIR");
        unsafe {
            std::env::set_var("FNO_STATE_DIR", state.path());
            std::env::set_var("CLAUDE_CONFIG_DIR", ambient.path());
        }
        let staged = stage_evals_claude_config().expect("staged sandbox");
        assert_eq!(staged, state.path().join("db/evals/claude-config"));
        // An abandoned temp from a dead runner sweeps on a later stage; a
        // fresh temp (a live stage) survives.
        let orphan = staged.join(".claude.json.stage-dead");
        std::fs::write(&orphan, b"{}").unwrap();
        {
            let f = std::fs::File::options().write(true).open(&orphan).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
                .unwrap();
        }
        let fresh = staged.join(".credentials.json.stage-live");
        std::fs::write(&fresh, b"{}").unwrap();
        assert!(stage_evals_claude_config().is_some());
        assert!(!orphan.exists());
        assert!(fresh.exists());
        std::fs::remove_file(&fresh).ok();
        let creds = std::fs::read(staged.join(".credentials.json")).unwrap();
        assert_eq!(creds, br#"{"auth":1}"#);
        let onboarding = std::fs::read(staged.join(".claude.json")).unwrap();
        assert_eq!(onboarding, br#"{"onboarded":true}"#);
        assert!(!staged.join("unrelated.txt").exists());
        let link = std::fs::read_link(staged.join("plugins")).expect("plugins symlink");
        assert_eq!(link, plugins);
        assert!(staged.join("settings.json").is_file());
        match state_previous {
            Some(v) => unsafe { std::env::set_var("FNO_STATE_DIR", v) },
            None => unsafe { std::env::remove_var("FNO_STATE_DIR") },
        }
        match config_previous {
            Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
        }
    }
}
