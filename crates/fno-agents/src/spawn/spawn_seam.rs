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
    let args = match crate::spawn_bare::boundary_rewrite(tail) {
        crate::spawn_bare::Boundary::Fence(rewritten) => {
            let mut out = Vec::with_capacity(rewritten.len() + 1);
            out.extend(args.first().cloned());
            out.extend(rewritten);
            out
        }
        crate::spawn_bare::Boundary::Keep => args,
        crate::spawn_bare::Boundary::Refuse(line) => {
            eprintln!("fno agents spawn: {line}");
            return Err(2);
        }
    };
    let args = crate::spawn_admission::resolve_prompt_file(args).map_err(|(reason, code)| {
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
                    "fno agents spawn: the name {name} is reserved by {holder}, a spawn still \
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
        "fno agents spawn: --resume {} resumes a {} transcript; -H {} would launch it on the \
         wrong harness. The corrected form: fno agents spawn --name {} --resume={} {} -H {} {} \
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
        "fno agents spawn: Codex code payloads require a resolved git grant; cwd={}; \
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
/// claim store's root, the two registry directories, and this spawn's plan
/// directory. Absolute, deduplicated, existing only, in that order.
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
    roots.extend(crate::plans_path::plans_content_dir(cwd));
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
    }

    #[test]
    fn a_refused_admission_reserves_no_name_and_keeps_its_reason() {
        let refused = crate::spawn_admission::Admission {
            argv: Vec::new(),
            stderr: vec!["fno agents spawn: refused".to_string()],
            exit: Some(2),
            stdout: None,
            take_over: None,
            events: Vec::new(),
        };
        assert_eq!(reservable_name(&refused, false), None);
        assert_eq!(refused.stderr, ["fno agents spawn: refused"]);
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
}
