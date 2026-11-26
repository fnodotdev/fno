//! `fno-agent` client entrypoint. Parses a command + flags into a JSON-RPC
//! request, lazy-starts the daemon, forwards the request, prints the result,
//! and maps the daemon's error code to a process exit code. Power users can
//! call it directly.

use fno_agent::cli_args::SpawnAxes;
use fno_agent::client::resolve_daemon_bin;
use fno_agent::client::{
    call, call_if_running, check_daemon_drift, drift_from_status, ClientError,
};
use fno_agent::drift::{drift_warning, DriftState};
use fno_agent::harness::{AsHarness, Harness};
use fno_agent::paths::AgentsHome;
use fno_agent::protocol::{ErrorCode, Request, ResponsePayload};
use fno_agent::provider::{known_providers_csv, KNOWN_PROVIDERS};
use fno_agent::usage::{command_help, command_usage, CLIENT_COMMAND_USAGE};
use serde_json::{json, Map, Value};
use std::io::IsTerminal;

// The client-side `ask` provider hooks live beside the entrypoint for the
// file budget; `use` keeps every call site below unchanged.
#[path = "asks/client_asks.rs"]
mod client_asks;
use client_asks::{
    maybe_run_agy_ask, maybe_run_claude_ask, maybe_run_codex_ask, maybe_run_gemini_ask,
    maybe_run_opencode_ask, opt_timeout,
};

// The binary-direct doors `run` tries before the daemon path, grouped by the
// question each answers.
#[path = "doors/client_doors.rs"]
mod client_doors;

// Registered spellings are frozen at the pre-rename set (the shrink gate
// allows no added token); the lead-/org- spellings dispatch below as
// accepted aliases and register when the gate learns to bank a swap.
const ALL_CLIENT_ACTIONS: &[&str] = &[
    "--emit-schema",
    "active-backlog-receipt",
    "adopt",
    "announce",
    "canonical-check",
    "ask",
    "attach",
    "authorized-merge",
    "bash-census",
    "board",
    "claim",
    "codex-assign-project",
    "compaction",
    "component-verdict",
    "provider-cap",
    "source-pin",
    "detect",
    "digest",
    "distress-scan",
    "drive",
    "drive-authority",
    "evals-macro",
    "evidence-gate",
    "finalize",
    "fleet-incident",
    "grid",
    "help",
    "host",
    "judge",
    "kill-check",
    "lead-checkin",
    "lead-escalation-text",
    "lead-history",
    "route-slot",
    "list",
    "logs",
    "loop",
    "loop-check",
    "loops",
    "mail-inject",
    "manifest-eval",
    "manifest-for-session",
    "name-codes",
    "name-mint",
    "name-parse",
    "needs",
    "node-origin",
    "node-route",
    "notify-watch",
    "orphan-reap",
    "feed",
    "pr-heal",
    "probe-run",
    "honesty-sweep",
    "prove-it-verdicts",
    "test-run",
    "promote",
    "publish-review",
    "reap",
    "roster-reap",
    "reconcile",
    "reclaim",
    "plugin-install",
    "recover",
    "registry-json",
    "reentry-plan",
    "rename",
    "term-shape",
    "term-state",
    "report",
    "review-coverage",
    "review-summary",
    "census",
    "restart",
    "resume",
    "resume-argv",
    "review-start",
    "rm",
    "scratch",
    "session-start-bytes",
    "spawn",
    "spawn-gate",
    "spawn-overlay",
    "spawn-axes",
    "fallback-chain",
    "state",
    "status",
    "stop",
    "subscribe",
    "task-context-gate",
    "task-context-payload",
    "task-context-prepare",
    "task-context-revalidate",
    "task-context-show",
    "task-context-stage",
    "territory-rows",
    "territory-verdict",
    "trace",
    "verify-evidence",
    "version",
    "wait",
];

fn main() {
    fno_agent::legacy_env::adopt();
    fno_agent::chokepoint_events::install();
    fno_agent::usage_events::take_cli_start();
    let args: Vec<String> = std::env::args().skip(1).collect();
    run_gh_and_front_doors(&args);
    run_transport_doors(&args);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    run_evals_and_watch_doors(&args);
    // A spawn is admitted in process (spawn_seam) before run() parses it.
    let mut spawn_timing = fno_agent::spawn_seam::SpawnTiming::for_spawn(&args);
    // The schedule arm answers before admission: a trigger flag or an
    // action like --list-scheduled carries its own grammar, and the strict
    // admission would demand a spawn name no such request carries.
    if args.first().map(String::as_str) == Some("spawn") {
        let home = AgentsHome::from_env();
        let rest = args.get(1..).unwrap_or(&[]);
        if let Some(code) = fno_agent::spawn::spawn_schedule::front_door(&home, rest) {
            fno_agent::usage_events::exit_cli(code);
        }
    }
    let args = match fno_agent::spawn_seam::admit_spawn(args) {
        Ok(args) => args,
        Err(code) => fno_agent::usage_events::exit_cli(code),
    };
    if let Some(timing) = spawn_timing.as_mut() {
        timing.record_seam();
    }
    // Every other client command passes its own seam (agents_seam).
    let command = args.first().cloned().unwrap_or_default();
    let args = match fno_agent::agents_seam::prepare(&command, args) {
        Ok(args) => args,
        Err(code) => fno_agent::usage_events::exit_cli(code),
    };
    let code = rt.block_on(run(args, spawn_timing));
    fno_agent::usage_events::exit_cli(code);
}

/// The doors that answer before any other arm: the gh transport, the
/// backlog, the family doors and the `do` fronts. Each exits when it answers.
fn run_gh_and_front_doors(args: &[String]) {
    // First, so a proxied gh call pays no other arm; transport-only.
    if let Some(code) = fno_agent::pr_status::adapter::gh_transport_door(&args) {
        fno_agent::usage_events::exit_cli(code);
    }
    if args.first().map(String::as_str) == Some("backlog") {
        fno_agent::usage_events::exit_cli(fno_agent::backlog::cli::run(&args[1..]));
    }
    // The ported target-family doors dispatch through family_doors: hidden
    // daemon-free leaves in the graph-get mold, never registered
    // ALL_CLIENT_ACTIONS tokens (the shrink law allows no added token).
    if let Some(code) = fno_agent::family_doors::client_dispatch(&args) {
        fno_agent::usage_events::exit_cli(code);
    }
    if args.first().map(String::as_str) == Some("question-intake") {
        fno_agent::usage_events::exit_cli(fno_agent::question_intake::run_question_intake());
    }
    if args.first().map(String::as_str) == Some("question-clear") {
        fno_agent::usage_events::exit_cli(fno_agent::question_clear::run_question_clear());
    }
    // The SessionStart reconcile sweep execs here; see backlog::orphan_plans.
    if args.first().map(String::as_str) == Some("backlog-orphan-plans") {
        fno_agent::usage_events::exit_cli(fno_agent::backlog::orphan_plans::run_orphan_plans(
            &args[1..],
        ));
    }
    // scripts/validate-plan.sh shells HERE and reads the E/W/X/O/U line
    // protocol back.
    if args.first().map(String::as_str) == Some("surface-check") {
        fno_agent::usage_events::exit_cli(fno_agent::surface_check::run_surface_check(&args[1..]));
    }
    // executor-route: the parser door; body in executor_route.rs (registers no command).
    if args.first().map(String::as_str) == Some("executor-route") {
        fno_agent::usage_events::exit_cli(fno_agent::executor_route::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("wave") {
        fno_agent::usage_events::exit_cli(fno_agent::wave::run(&args[1..]));
    }
    // cli/src/fno/pr/_sync_canonical.py transports HERE through command_call:
    // the post-merge sync + its catch-up sweep and staleness alarm, native.
    // Registers no command (the shrink law allows no new action).
    if args.first().map(String::as_str) == Some("sync-canonical") {
        fno_agent::usage_events::exit_cli(fno_agent::sync_canonical::run_sync_canonical_command(
            &args[1..],
        ));
    }
    // PR-scoped callers resolve the local checkout from the head branch; transport-only.
    if args.first().map(String::as_str) == Some("pr-worktree") {
        fno_agent::usage_events::exit_cli(fno_agent::pr_worktree::run());
    }
    if args.first().map(String::as_str) == Some("do")
        && args.get(1).map(String::as_str) == Some("pr")
    {
        fno_agent::usage_events::exit_cli(fno_agent::pr_front::run(&args[2..]));
    }
    if args.first().map(String::as_str) == Some("pr") {
        fno_agent::usage_events::exit_cli(fno_agent::pr_front::run(&args[1..]));
    }
    if args.first() == Some(&"do".to_string()) && args.get(1) == Some(&"ci".to_string()) {
        fno_agent::usage_events::exit_cli(fno_agent::ci_front::run(&args[2..]));
    }
    if args.first().map(String::as_str) == Some("do")
        && args.get(1).map(String::as_str) == Some("docs")
    {
        fno_agent::usage_events::exit_cli(fno_agent::docs_front::run(args.get(2..).unwrap_or(&[])));
    }
    if args.first().map(String::as_str) == Some("do")
        && matches!(args.get(1).map(String::as_str), Some("show" | "diff"))
    {
        fno_agent::usage_events::exit_cli(fno_agent::rev_front::run(&args[1..]));
    }
}

/// The early transport arms: daemon-free reads and writes that answer in
/// microseconds, before the runtime builds. Each exits when it answers.
fn run_transport_doors(args: &[String]) {
    // gh-cache, target-status: the early transport arms; client_transport.rs
    // holds their bodies and comments. worktree-reapable and launch-workdir
    // stay here: their own ports own the moves into their modules.
    if let Some(code) = fno_agent::client_transport::transport_early_arms(&args) {
        fno_agent::usage_events::exit_cli(code);
    }
    // One dispatch site per family; the op tables live in the modules.
    if let Some(code) = fno_agent::worktree::client_dispatch(&args) {
        fno_agent::usage_events::exit_cli(code);
    }
    if let Some(code) = fno_agent::launch_workdir::client_dispatch(&args) {
        fno_agent::usage_events::exit_cli(code);
    }
    // `harness-probe` / `harness-matrix` / `harness-beat`: capability reads, transport-only.
    if matches!(
        args.first().map(String::as_str),
        Some("harness-probe" | "harness-matrix" | "harness-beat")
    ) {
        fno_agent::usage_events::exit_cli(fno_agent::harness_reader::transport_doors(&args));
    }
    // `harness-roster`: the one-roster JSON read.
    if args.first().map(String::as_str) == Some("harness-roster") {
        fno_agent::usage_events::exit_cli(fno_agent::harness_roster::run_harness_roster(
            &args[1..],
        ));
    }
    // `user-hours`: refusal-or-ok JSON read of `[user.working_hours]`.
    if args.first().map(String::as_str) == Some("user-hours") {
        fno_agent::usage_events::exit_cli(fno_agent::agents_config::run_user_hours(
            args.get(1..).unwrap_or_default(),
        ));
    }
    // `terminals`: the delivered-terminal vocabulary JSON read; Python's
    // ledger promotion gate and scoreboard fold read it like the roster.
    if args.first().map(String::as_str) == Some("terminals") {
        fno_agent::usage_events::exit_cli(fno_agent::terminal_vocab::run_terminals(&args[1..]));
    }
    // `remote`: the tailnet door's family (serve, pair, devices, revoke,
    // status). People reach it as `fno agent workspace remote`; `serve`
    // builds its own runtime.
    if args.first().map(String::as_str) == Some("remote") {
        fno_agent::usage_events::exit_cli(fno_agent::remote::run(
            args.get(1..).unwrap_or_default(),
        ));
    }
    if args.first().map(String::as_str) == Some("pending-session-row") {
        fno_agent::usage_events::exit_cli(fno_agent::pending_session_row::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("registry-commit") {
        fno_agent::usage_events::exit_cli(fno_agent::registry_commit::run(&args[1..]));
    }
    // hooks/context-run.sh is the only caller.
    if args.first().map(String::as_str) == Some("context-run") {
        fno_agent::usage_events::exit_cli(fno_agent::context_run::run_context_run(&args[1..]));
    }
    // A fire answers in microseconds; the runtime never builds for one.
    if args.first().map(String::as_str) == Some("hook") {
        fno_agent::usage_events::exit_cli(fno_agent::hook::dispatch(&args[1..]));
    }
    // `mail-hold`: transport-only (no client action - the shrink law allows
    // none); the mux server's keystroke arm and `lead cancel` are the
    // callers. The arm runs detached-safe and answers in microseconds, so
    // it dispatches before the runtime builds, with the other early arms.
    if args.first().map(String::as_str) == Some("mail-hold") {
        fno_agent::usage_events::exit_cli(fno_agent::mail_hold::run_mail_hold(&args[1..]));
    }
    // The resume receipt's argv door: transport-only, like mail-hold. The
    // forwarding leaf (cli/src/fno/resume_receipt_cli.py) shells the do
    // command here with the same options; the menu counter never sees
    // transport arms.
    if args.first().map(String::as_str) == Some("resume-receipt") {
        fno_agent::usage_events::exit_cli(fno_agent::resume_receipt::run(&args[1..]));
    }
    // `delivery-evaluate`: the native evaluator behind the `fno delivery
    // evaluate` forward leaf; transport-only (no client action - the shrink
    // law allows none), answers in microseconds, so it dispatches before the
    // runtime builds, with the other early arms.
    if args.first().map(String::as_str) == Some("delivery-evaluate") {
        fno_agent::usage_events::exit_cli(fno_agent::delivery_eval::run(&args[1..]));
    }
    // `mail-notify-self`: transport-only; the inject-mail-notify hook is the caller; early arm, answers in microseconds.
    if args.first().map(String::as_str) == Some("mail-notify-self") {
        fno_agent::usage_events::exit_cli(fno_agent::mail_notify_self::run(&args[1..]));
    }
    // `pane-rebind`: the mux workspace restore's registry rebind door;
    // transport-only (no client action - the shrink law allows no new
    // action); the mux server's restore walk is the only caller. Answers in
    // one registry transaction, so it dispatches before the runtime builds,
    // with the other early arms.
    if args.first().map(String::as_str) == Some("pane-rebind") {
        fno_agent::usage_events::exit_cli(fno_agent::pane_rebind::run(&args[1..]));
    }
    // `state-root`: the seal-handshake door (agents_config::run_state_root_probe).
    if args.first().map(String::as_str) == Some("state-root") {
        fno_agent::usage_events::exit_cli(fno_agent::agents_config::run_state_root_probe());
    }
}

/// The evals, scoreboard and PR watch families, after the runtime builds.
/// Each exits when it answers.
fn run_evals_and_watch_doors(args: &[String]) {
    // The Python evals phase reaches this arm through resolve_binary.
    if args.first().map(String::as_str) == Some("evals-arm") {
        fno_agent::usage_events::exit_cli(fno_agent::evals_arm::run_evals_arm(&args[1..]));
    }
    // `evals`: the leaf's family dispatch; the shrink law bars a new `run` arm (contract in evals.rs).
    if args.first().map(String::as_str) == Some("evals") {
        fno_agent::usage_events::exit_cli(fno_agent::evals::run(&args[1..]));
    }
    // `evals-trend`: a transport-only fold (the shrink law allows no new
    // `run` arm); the module doc carries the contract.
    if args.first().map(String::as_str) == Some("evals-trend") {
        fno_agent::usage_events::exit_cli(fno_agent::evals_trend::run_evals_trend(&args[1..]));
    }
    // `evals-attempt`: the native attempt-eligibility verdict for eval history
    // rows. Transport-only, dispatched here like evals-trend: the
    // Python runner asks it per attempt at write time; the report re-asks it
    // over whole-history batches at read time.
    if args.first().map(String::as_str) == Some("evals-attempt") {
        fno_agent::usage_events::exit_cli(fno_agent::eval_attempt::run_evals_attempt(&args[1..]));
    }
    // `scoreboard`: the telemetry family's transport door, dispatched here
    // like evals-trend because the shrink law bars a new `run` arm. The
    // front door (`fno whoami scoreboard`) execs here; the parity goldens
    // replay the same spelling.
    if args.first().map(String::as_str) == Some("scoreboard") {
        fno_agent::usage_events::exit_cli(fno_agent::scoreboard::run_scoreboard(&args[1..]));
    }
    // `pr-park` and `pr-watch`: the two command families behind `fno do pr
    // watch`; both dispatch here like evals-arm because the shrink law bars
    // a new `run` arm. Their module docs carry the per-command contract.
    if args.first().map(String::as_str) == Some("pr-park") {
        fno_agent::usage_events::exit_cli(fno_agent::pr_park::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("pr-watch") {
        fno_agent::usage_events::exit_cli(fno_agent::pr_watch::run(&args[1..]));
    }
}

async fn run(args: Vec<String>, spawn_timing: Option<fno_agent::spawn_seam::SpawnTiming>) -> i32 {
    if args.is_empty() {
        print_help();
        return 0;
    }
    let command = args[0].as_str();
    if matches!(command, "-h" | "--help" | "help") {
        print_help();
        return 0;
    }

    // `version` / `-V` / `--version`: report which commit this binary was built
    // from -- the prerequisite for Rust-side `fno doctor` staleness. `--json`
    // emits the machine surface `fno doctor` reads off the resolved binary path.
    // Side-effect-free, like `--emit-schema`/`help`: it never starts the daemon
    // and is NOT a routable daemon command, so it stays out of CLIENT_COMMAND_USAGE /
    // RUST_CLIENT_COMMANDS (callers invoke the binary directly). Matched here rather
    // than as a dispatch arm so the routable-command parity guard does not see it.
    if matches!(command, "version" | "-V" | "--version") {
        let json = fno_agent::json_output::requested(&args[1..]);
        fno_agent::version::print_version(json);
        return 0;
    }

    if let Some(code) = client_doors::before_help(command, &args).await {
        return code;
    }
    if let Some(code) = command_help_exit(command, &args) {
        return code;
    }
    if let Some(code) = client_doors::after_help(command, &args).await {
        return code;
    }
    run_daemon_command(command, &args, spawn_timing).await
}

/// Per-command `--help`: the command's own help body or usage line, exit 0.
fn command_help_exit(command: &str, args: &[String]) -> Option<i32> {
    // Per-command help: `fno agent <command> --help` prints that command's usage line
    // and exits 0, instead of the command's arg parser erroring "unknown flag:
    // --help" / "takes no arguments". Only fires for a recognized
    // command; an unknown command falls through to its normal error path. The scan
    // stops at an `--argv`/`--` boundary so a `--help` inside a spawn/host argv
    // payload reaches the spawned command instead of being captured here.
    // A leaf that rides `status` as its first argument parses its own
    // `--help`; the status usage line would answer for the wrong command.
    let status_leaf = command == "status"
        && args
            .get(1)
            .is_some_and(|a| STATUS_LEAVES_WITH_HELP.contains(&a.as_str()));
    if !status_leaf && is_help_request(&args[1..]) {
        // A command with a full help body owns its --help; the one-line table
        // entry stays for the top-level list.
        if let Some(body) = command_help(command) {
            println!("{body}");
            return Some(0);
        }
        if let Some(usage) = command_usage(command) {
            println!("usage: fno-agent {usage}");
            return Some(0);
        }
    }
    None
}

/// The client-side rendering flags, read off argv before build_request strips
/// them: they never reach the daemon.
struct RenderFlags {
    json: bool,
    is_tty: bool,
    discover: bool,
}

impl RenderFlags {
    fn read(args: &[String]) -> Self {
        // Task 3.1: capture --json before build_request strips it, and detect TTY.
        // --json is a client-side rendering flag and must NOT be forwarded to the daemon.
        // Stop scanning at `--argv`: a `--json`/`-J` in the spawned process's argv
        // payload must not trip client-side JSON rendering (gemini review, PR #431).
        let json_flag = args[1..]
            .iter()
            .take_while(|a| a.as_str() != "--argv")
            .any(|a| a == "--json" || a == "-J");
        let is_tty = std::io::stdout().is_terminal();
        // the P1 discovered-live-sessions lane is on by default for
        // `list`; --no-discovered opts out of the ~/.claude/sessions scan.
        let discover_flag = !args[1..]
            .iter()
            .take_while(|a| a.as_str() != "--argv")
            .any(|a| a == "--no-discovered");
        Self {
            json: json_flag,
            is_tty,
            discover: discover_flag,
        }
    }
}

/// What a client-side `spawn` holds until the daemon exchange settles it: the
/// spawn guard's hold, the promote plan, and the roster read the gate closes.
/// The roster scope is a guard: dropping it closes the read.
#[derive(Default)]
struct SpawnLease {
    hold: Option<fno_agent::spawn_guard::SpawnHold>,
    promotion: Option<fno_agent::spawn_promote::Promotion>,
    roster_scope: Option<fno_agent::backlog::worked::RosterScope>,
}

impl SpawnLease {
    /// Settle the hold and the promote plan with `exit`.
    fn settle(&mut self, exit: i32, name: &str) {
        fno_agent::spawn_guard::settle(&mut self.hold, exit, name);
        fno_agent::spawn_promote::settle(self.promotion.as_ref(), exit, name);
    }
}

/// The agent name the parsed params carry; commands that never set
/// params.name resolve to the empty string.
fn spawn_agent_name(params: &Value) -> String {
    params
        .pointer("/name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default()
}

/// The daemon-bound path: build the request, run the client-side ask and
/// spawn lanes, then forward what is left to the daemon and render it.
async fn run_daemon_command(
    command: &str,
    args: &[String],
    mut spawn_timing: Option<fno_agent::spawn_seam::SpawnTiming>,
) -> i32 {
    let flags = RenderFlags::read(args);
    let (method, mut params) = match build_request(command, &args[1..]) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("fno-agent: {msg}");
            return 2;
        }
    };

    // An anycast ask swaps its project for the one live peer before any
    // lane reads the name.
    if method == "agent.ask" {
        if let Err((msg, code)) = fno_agent::ask_project::resolve(&mut params) {
            eprintln!("{msg}");
            return code;
        }
    }

    // Resolve the agent name from the PARSED params, not args.get(1): build_request
    // strips leading flags (and their values) when collecting positionals, so
    // `fno agent stop --force worker-A` yields name="worker-A". Reading args.get(1)
    // would capture "--force" and print the wrong success line (gemini-code-assist
    // high on PR #361). Falls back to the raw first positional for commands that don't
    // set params.name (none of the formatted commands hit that path today).
    let mut agent_name = spawn_agent_name(&params);

    let home = AgentsHome::from_env();

    // `ask` runs client-side for every harness: each lane takes the rows it
    // owns, and only a codex thread target reaches the daemon.
    if method == "agent.ask" {
        if let Some(code) = run_client_ask(&home, &params, &agent_name) {
            return code;
        }
    }

    // Intercept `spawn` only: `host` and `promote` also map to agent.spawn
    // and fall through to the daemon RPC unchanged.
    let mut lease = SpawnLease::default();
    if method == "agent.spawn" && command == "spawn" {
        fno_agent::spawn::watcher_probe::probe_client_spawn(&params);
        if let Some(code) = run_client_spawn(
            &home,
            args,
            &mut params,
            &mut agent_name,
            &mut lease,
            spawn_timing.as_mut(),
        ) {
            return code;
        }
        // No client-side handler matched: fall through to the daemon RPC below.
    }

    let daemon_bin = resolve_daemon_bin();
    if let Err(code) = stamp_request_cwd(&method, &mut params) {
        return code;
    }
    let DaemonRequest {
        req,
        spawn_gate: daemon_spawn_gate,
        thread_portal_params,
    } = match DaemonRequest::build(
        method,
        params,
        &home,
        &agent_name,
        &mut lease,
        spawn_timing.as_mut(),
    ) {
        Ok(built) => built,
        Err(code) => return code,
    };

    let call_result = if command == "rm" {
        let Ok(result) = fno_agent::restart_run::rm_after_drift_repair(
            &check_daemon_drift(&home).await,
            fno_agent::restart_run::restart_daemon_for_rm(),
            call(&home, &daemon_bin, &req),
        )
        .await
        else {
            return 21;
        };
        result
    } else {
        call(&home, &daemon_bin, &req).await
    };
    drop(daemon_spawn_gate);
    if matches!(&call_result, Ok(resp) if matches!(resp.payload, ResponsePayload::Ok(_))) {
        lease.settle(0, &agent_name);
    }
    match call_result {
        Ok(resp) => match resp.payload {
            ResponsePayload::Err(err) => {
                eprintln!("fno-agent: {}", err.message);
                exit_code_for(err.code)
            }
            ResponsePayload::Ok(result) => {
                let reply = DaemonReply {
                    command,
                    agent_name: &agent_name,
                    home: &home,
                    flags: &flags,
                    thread_portal_params: thread_portal_params.as_ref(),
                };
                reply.render(result).await
            }
        },
        Err(e) => report_call_failure(command, &agent_name, &home, &e),
    }
}

/// The daemon request, plus what the client holds across its exchange: the
/// spawn gate guard (released on drop after the response read) and the
/// portal-placement snapshot of a thread spawn's params.
struct DaemonRequest {
    req: Request,
    spawn_gate: Option<fno_agent::spawn_gate::GateGuard>,
    thread_portal_params: Option<Value>,
}

impl DaemonRequest {
    /// Build the request, taking the spawn gate first for a daemon-bound
    /// thread spawn. A gate refusal answers its exit.
    fn build(
        method: String,
        mut params: Value,
        home: &AgentsHome,
        agent_name: &str,
        lease: &mut SpawnLease,
        mut spawn_timing: Option<&mut fno_agent::spawn_seam::SpawnTiming>,
    ) -> Result<Self, i32> {
        // (AC19): for a codex THREAD spawn the daemon RPC is what creates
        // the registry row, so the spawn gate is held across exactly this exchange
        // (acquire before the write, release after the response read) - one gate
        // evaluation per spawn, and the first spawn's row is counted before the
        // second is evaluated. Other commands skip this entirely.
        let daemon_bound_thread_spawn = is_daemon_bound_thread_spawn(&method, &params);
        // Hop 1 of the state-root grant. The client inherits FNO_WORKER_ADD_DIRS
        // from the Python seam across `os.execv`, so it reads the ALREADY-RESOLVED
        // set with the same reader every other lane uses. It travels as a param,
        // never environment: the daemon is long-lived and SHARED and does not
        // inherit this spawn's env, so a `state_dirs_from_env()` call over there
        // would read the daemon's own env instead of ours.
        if daemon_bound_thread_spawn {
            fno_agent::codex_thread::attach_codex_thread_state_dirs(&mut params);
        }
        // Snapshot for the portal placement, after the receipt.
        let thread_portal_params = if method == "agent.spawn"
            && params.get("substrate").and_then(|v| v.as_str()) == Some("thread")
        {
            Some(params.clone())
        } else {
            None
        };
        let daemon_spawn_gate = if daemon_bound_thread_spawn {
            match hold_daemon_spawn_gate(
                home,
                &params,
                agent_name,
                lease,
                spawn_timing.as_deref_mut(),
            ) {
                Ok(guard) => Some(guard),
                Err(code) => return Err(code),
            }
        } else {
            None
        };
        let mut req = Request::new(1, method, params);
        if daemon_bound_thread_spawn {
            stamp_stage_ms(&mut req, spawn_timing.as_deref());
        }
        Ok(Self {
            req,
            spawn_gate: daemon_spawn_gate,
            thread_portal_params,
        })
    }
}

/// The client-side `ask`: `Some(exit)` when a lane answered or the target is
/// unresolvable, `None` only for a codex thread target, which the daemon asks.
fn run_client_ask(home: &AgentsHome, params: &Value, agent_name: &str) -> Option<i32> {
    let codex_thread_target = match ask_target(home, agent_name) {
        Ok(codex) => codex,
        Err(code) => return Some(code),
    };
    if let Some(code) = maybe_run_claude_ask(home, params, agent_name) {
        return Some(code);
    }
    // Codex `ask` is handled client-side: codex is a
    // one-shot `codex exec --json` subprocess, not a PTY agent, so it
    // bypasses the daemon RPC. Same Option<i32> contract as claude.
    if let Some(code) = maybe_run_codex_ask(home, params, agent_name) {
        return Some(code);
    }
    // Gemini `ask` is handled client-side: gemini is a
    // one-shot `gemini -p --output-format json` subprocess. Same contract.
    if let Some(code) = maybe_run_gemini_ask(home, params, agent_name) {
        return Some(code);
    }
    // Agy `ask` is intercepted client-side (Phase C): agy is plain-text with
    // no session id, so a stateful resume is unsupported - this surfaces a
    // clear error directing the caller to `spawn --harness agy --once`.
    if let Some(code) = maybe_run_agy_ask(home, params, agent_name) {
        return Some(code);
    }
    // zcode `ask` resumes by name over the headless lane: one -p --resume
    // turn per ask, session id from the row's harness_session_id.
    if let Some(code) = fno_agent::zcode_ask::maybe_run_zcode_ask(home, params, agent_name) {
        return Some(code);
    }
    if let Some(code) = fno_agent::thread_host::maybe_run_ask(home, params, agent_name) {
        return Some(code);
    }
    // Opencode `ask` is intercepted client-side: opencode is
    // pane-hosted only in v1, so a stateful resume is unsupported - this
    // surfaces a clear error directing the caller to drive the pane
    // directly, rather than the generic "provider required for new
    // agent" text an existing opencode row would otherwise hit below
    // (that text is both wrong - the agent already exists - and a dead
    // end, since retrying with --harness opencode reproduces it).
    if let Some(code) = maybe_run_opencode_ask(home, params, agent_name) {
        return Some(code);
    }
    // Unconditional flip: `ask` now auto-routes to this
    // client for every provider, so an ask that matched none of the four
    // provider hooks is a create with no/unknown `--provider`. Surface
    // Python's `select_provider` exit-2 error here rather than falling
    // through to the daemon RPC, whose `handle_ask` PTY screen is the wrong
    // shape for `ask` (Locked Decision 3). The daemon path is now
    // unreachable for `agent.ask`.
    if !codex_thread_target {
        return Some(unresolvable_ask_exit(params, agent_name));
    }
    None
}

/// Whether the ask names a known row: `Ok(true)` for a codex thread target,
/// `Ok(false)` for any other known row, `Err(exit)` for an unreadable registry
/// or an unknown name. Ask never creates: an unknown name exits 16 whatever
/// --harness says. One registry read serves the whole ask path.
fn ask_target(home: &AgentsHome, agent_name: &str) -> Result<bool, i32> {
    use fno_agent::claude_ask::{emit_event, py_repr};
    use fno_agent::state::load_registry;
    // An unreadable registry exits 12, never an empty registry where every
    // name looks unknown. A MISSING file is not an error.
    let registry = match load_registry(&home.registry_json()) {
        Ok(r) => r,
        Err(e) => {
            emit_event(
                &home.events_jsonl(),
                "agent_ask_failed",
                &[
                    ("stage", "registry-read".into()),
                    ("name", agent_name.to_string().into()),
                    ("error", e.to_string().into()),
                ],
            );
            eprintln!("registry read failed: {e}");
            return Err(12);
        }
    };
    if registry.find_name_or_full_session_id(agent_name).is_none() {
        // Event parity: Python's dispatch_ask emits agent_ask_failed
        // stage="unknown-name" before raising; this pre-check is the
        // only emitter on the Rust CLI path (the lib None-arms are
        // unreachable from here once this fires).
        emit_event(
            &home.events_jsonl(),
            "agent_ask_failed",
            &[
                ("stage", "unknown-name".into()),
                ("name", agent_name.to_string().into()),
            ],
        );
        eprintln!(
            "unknown agent {}; spawn it first: fno agent spawn {} --harness <harness>",
            py_repr(agent_name),
            agent_name
        );
        return Err(16);
    }
    // A codex thread target falls through to the daemon ask instead of the
    // unresolvable-create error.
    Ok(fno_agent::codex_ask::is_codex_thread_target(
        &registry, agent_name,
    ))
}

/// The client-side `spawn`: `Some(exit)` when a client lane took the spawn,
/// `None` when it falls through to the daemon RPC.
fn run_client_spawn(
    home: &AgentsHome,
    args: &[String],
    params: &mut Value,
    agent_name: &mut String,
    lease: &mut SpawnLease,
    spawn_timing: Option<&mut fno_agent::spawn_seam::SpawnTiming>,
) -> Option<i32> {
    let substrate = match admit_client_spawn(params, agent_name, lease) {
        Ok(substrate) => substrate,
        Err(code) => return Some(code),
    };
    let substrate = substrate.as_str();
    let params: &Value = params;
    let agent_name: &str = agent_name;
    if let Some(code) = relay_client_spawn(home, args, params, agent_name, substrate, lease) {
        return Some(code);
    }
    // A DEFAULT never opens a view: a bare spawn is a paneless thread,
    // inside a mux or out. Only an explicit placement flag opens
    // anything, and it opens through place_thread_portal_after_spawn.
    if substrate == "pane" {
        // 4a-G2: the `pane` substrate (the default) is mux-hosted, and
        // the pane spine answers every harness arm itself - the Python
        // bounce this branch kept for unseated shapes is deleted.
        let promotion = lease.promotion.as_ref();
        let caller_session = fno_agent::spawn_promote::gate_input(promotion).caller_session;
        return Some(fno_agent::spawn_guard::run_guarded_pane(
            params,
            agent_name,
            caller_session.as_deref(),
            || {
                let code = fno_agent::pane_spawn::run_pane_spawn(home, params, agent_name, args);
                fno_agent::spawn_promote::settle(promotion, code, agent_name)
            },
        ));
    }
    // An --account spawn is decided inside maybe_run_spawn: the seam's
    // stamp names the account it applied; a spawn no arm takes falls
    // through to the daemon RPC.
    // The guard and the gate share one roster read; each gate closes it.
    lease.roster_scope = Some(fno_agent::backlog::worked::RosterScope::open());
    let code = maybe_run_spawn(
        home,
        params,
        agent_name,
        &mut lease.hold,
        lease.promotion.as_ref(),
        spawn_timing,
    )?;
    fno_agent::spawn_guard::settle(&mut lease.hold, code, agent_name);
    let code = fno_agent::spawn_promote::settle(lease.promotion.as_ref(), code, agent_name);
    if code == 0 {
        // A placement failure never recolors the spawn verdict: the
        // receipt is the truth, the worker is live, and a nonzero here
        // reads as spawn failure to a retrying caller - a duplicate.
        if let Err(detail) = place_thread_portal_after_spawn(params, agent_name) {
            eprintln!("{detail}");
        }
    }
    Some(code)
}

/// Admit a client-side spawn: the bare-spawn admission, the placement and
/// trust preflights, and the promote plan. Answers the dispatch substrate
/// (`thread` reads as its historical `bg` selector), or the refusal's exit.
fn admit_client_spawn(
    params: &mut Value,
    agent_name: &mut String,
    lease: &mut SpawnLease,
) -> Result<String, i32> {
    if let Some(map) = params.as_object_mut() {
        if let Err((line, code)) = fno_agent::spawn_bare::admit(map) {
            eprintln!("{line}");
            // The caller holds this refusal, so the first-turn watch owes it
            // no "stuck at gate" notice for the same launch.
            let home = fno_agent::paths::AgentsHome::from_env();
            fno_agent::spawn_watch::settle_refused(
                &home,
                fno_agent::spawn_promote::caller_session_id(&home).as_deref(),
                agent_name,
                &line,
            );
            return Err(code);
        }
    }
    let mut substrate = params
        .get("substrate")
        .and_then(|v| v.as_str())
        .unwrap_or("thread")
        .to_string();
    // `thread` is the public substrate name. The lower-level dispatch arms
    // retain their historical `bg` selector until their wire contract moves.
    if substrate == "thread" {
        substrate = "bg".to_string();
    }
    if let Err(message) = validate_spawn_placement(params, &substrate) {
        eprintln!("{message}");
        return Err(2);
    }
    // Trust preflight: refuse an untrusted claude workspace before the
    // gate queue, loudly, instead of a paid queue plus a silent exit 0.
    if fno_agent::claude_workspace_trust::spawn_preflight_refuses(params, &substrate) {
        return Err(2);
    }
    lease.promotion = fno_agent::spawn_promote::plan(params, agent_name, &substrate)?;
    if params["dry_run"] == true {
        println!(
            "{}",
            fno_agent::spawn_promote::dry_run_report(params, agent_name, lease.promotion.as_ref())
        );
        return Err(0);
    }
    // The raw --role-level/--role-scope pair rides the promote door's
    // grant check when no promote plan did: the pair widens the pane
    // posture, the codex sandbox and the minted thread row, and the
    // mint trusts this seam to own the territory policy.
    if lease.promotion.is_none() {
        fno_agent::spawn_promote::raw_role_pair_gate(params)?;
    }
    Ok(substrate)
}

/// Relay a spawn to another host: `Some(exit)` when a `--machine` pin or the
/// fleet map's least-loaded host took the worker, or the pin refused.
fn relay_client_spawn(
    home: &AgentsHome,
    args: &[String],
    params: &Value,
    agent_name: &str,
    substrate: &str,
    lease: &SpawnLease,
) -> Option<i32> {
    // The pin is read for every spawn, pinned or not: a pin that cannot
    // take the worker must refuse naming why, never drop silently. The
    // node-bound guard used to swallow an --machine pin and launch the
    // worker locally with no error.
    let spawn_tail = args.get(1..).unwrap_or(&[]);
    let pinned = match fno_agent::spawn_place::pinned_host(spawn_tail) {
        Ok(p) => p,
        Err(line) => {
            eprintln!("{line}");
            return Some(2);
        }
    };
    // A relayed leg never re-places: the ssh tail lost its pin at the
    // relay strip and FNO_MAIL_LOCAL_FORWARD marks a same-host forward,
    // so a foreign pin here contradicts the leg. The ambient parent
    // capture stamps spawned_by_session on every in-session spawn; that
    // is provenance, never a leg, so the guard reads only the explicit
    // marker.
    let leg = std::env::var_os("FNO_MAIL_LOCAL_FORWARD").is_some()
        || fno_agent::spawn_place::relay_leg(params);
    if fno_agent::spawn_place::leg_refuses_pin(pinned.as_deref(), leg) {
        let host = pinned.as_deref().unwrap_or_default();
        eprintln!(
            "fno agent spawn: --machine {host} cannot re-place a relayed leg; drop the pin or run the spawn on that host"
        );
        return Some(2);
    }
    // Cross-host placement, skipped on a relay leg. A --machine pin
    // relays to that host: the operator chose the machine, and a
    // node-bound spawn's far-side node-seed pass refuses loudly when it
    // cannot resolve the node, so placement never fails silent there
    // either. With no pin, the fleet map's least-loaded host takes the
    // worker when it beats this machine's free slots, unless the spawn
    // carries a machine-local credential or reads machine-local state:
    // the operator turn queue and the claims root live in this machine's
    // fno home, so those payloads pin to the spawner by default.
    let node_bound = params.get("node").is_some()
        || params.get("slug").is_some()
        || params.get("plan").is_some();
    let relayed = leg || (node_bound && pinned.is_none());
    if relayed {
        return None;
    }
    let target = match pinned.as_deref() {
        // A same-host pin never relays: the local gate takes it.
        Some(host) if fno_agent::spawn_place::same_host_pin(host) => None,
        Some(host) => Some(fno_agent::spawn_place::pinned_pick(host.to_string())),
        None => {
            // An account- or cutover-bearing spawn never auto-relays:
            // the remote resolves the machine-local id against its own
            // store and refuses there (mac2 refused zai on
            // 2026-11-07). An explicit --machine pin still relays.
            if fno_agent::spawn_place::stays_local(params)
                || fno_agent::spawn_place::reads_local_state(spawn_tail)
            {
                None
            } else {
                fno_agent::spawn_place::best_remote_host(home, chrono::Utc::now().timestamp())
                    .unwrap_or(None)
            }
        }
    };
    let t = target?;
    let strip = if substrate == "pane" {
        fno_agent::spawn_place::PANE_STRIP
    } else {
        fno_agent::spawn_place::THREAD_STRIP
    };
    let was_pinned = pinned.is_some();
    let how = if was_pinned { "pinned" } else { "least-loaded" };
    let tail = fno_agent::spawn_place::relay_args(spawn_tail, &t.host, agent_name, strip);
    eprintln!(
        "spawn relays to {}{}",
        t.host,
        if was_pinned { "" } else { " (least-loaded)" }
    );
    let code = fno_agent::spawn_remote::run(&tail);
    if code == 0 && substrate == "bg" {
        if let Err(detail) = place_thread_portal_after_spawn(params, agent_name) {
            eprintln!("{detail}");
        }
    }
    // The receipt's last line names the host: a lead reading the
    // tail sees where the spawn went without scrolling.
    eprintln!(
        "{}",
        fno_agent::spawn_place::receipt_line(&t.host, how, code)
    );
    fno_agent::spawn_promote::settle(lease.promotion.as_ref(), code, agent_name);
    Some(code)
}

/// Forward the caller's cwd so a spawned worker launches in the user's
/// project, not the daemon's frozen home dir (fix/agents-host-cwd). Only
/// daemon-bound requests reach here; claude/codex `ask` already returned. On
/// the rare current_dir() failure params stay as-is with a warning: the
/// daemon then uses its hardened temp-dir fallback (an obviously-wrong /tmp
/// launch) rather than silently adopting its own start dir.
fn stamp_request_cwd(method: &str, params: &mut Value) -> Result<(), i32> {
    let caller = match std::env::current_dir() {
        Ok(caller) => caller,
        Err(e) => {
            eprintln!(
                "fno-agent: could not resolve current dir ({e}); daemon will pick a fallback cwd"
            );
            return Ok(());
        }
    };
    // the default (no explicit --cwd, no --here) stamps the
    // canonical repo root instead of the caller cwd for daemon-bound
    // codex/gemini spawn -- the same inversion as the client-side path.
    // An explicit --cwd wins, so when params already carries one we
    // resolve nothing and emit no redirect note (it would falsely claim a
    // redirect that ensure_request_cwd's keep-explicit guard never
    // performs -- review MEDIUM 4); --here keeps the caller cwd. --fresh
    // is an accepted no-op alias. ensure_request_cwd then leaves the
    // explicit --cwd intact.
    let (_fresh, here) = fno_agent::spawn_cwd::fresh_here_flags(params);
    let explicit_cwd = params.get("cwd").is_some();
    // Only spawn consumes the launch dir: an `agent.ask` follows its
    // registered session and takes cwd as `_cwd`, so it never takes the
    // canonical default nor the redirect note (a false diagnostic for a
    // non-consuming op -- review). spawn keeps the inverted default.
    let stamp = if !explicit_cwd && !here && method == "agent.spawn" {
        match fno_agent::spawn_cwd::node_cwd_or_refuse(params, &caller) {
            Err(msg) => {
                eprintln!("fno-agent: refusing to dispatch: {msg}");
                return Err(2);
            }
            Ok(Some(project)) => project,
            Ok(None) => match fno_agent::paths::canonical_repo_root(&caller) {
                Some(canon) => {
                    fno_agent::spawn_cwd::note_fresh_redirect(&caller, &canon);
                    canon
                }
                None => caller,
            },
        }
    } else {
        caller
    };
    ensure_request_cwd(method, params, &stamp);
    Ok(())
}

/// Whether a spawn rides the daemon-bound thread lane, whose RPC creates the
/// registry row. Daemon-bound is DERIVED from the same capability contract the
/// daemon routes on (attach lane + a harness-owned server). Both binaries
/// embed the same packaged table, so this predicate and the daemon's route
/// cannot disagree the way a name test here and a derived route there could:
/// the next attach-with-server harness arrives with its state_dirs attached and
/// its gate run without this line learning its name.
fn is_daemon_bound_thread_spawn(method: &str, params: &Value) -> bool {
    // The provider default MUST match the daemon's, which is `codex` when the
    // param is absent (`handle_spawn`). A predicate requiring an explicit
    // "codex" here reads false for a spawn with no `-H`, while the daemon still
    // routes it to the codex thread lane - so the grant was never attached and
    // the gate below never ran, on the exact lane this node exists to fix. An
    // unreadable contract answers false here: the daemon refuses the same spawn,
    // so no grant or gate is skipped for a spawn the daemon would have served.
    let spawn_provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("codex");
    method == "agent.spawn"
        && params.get("substrate").and_then(|v| v.as_str()) == Some("thread")
        && fno_agent::harness_capabilities::HarnessContract::packaged()
            .and_then(|contract| {
                Ok(contract.thread_lane(spawn_provider)? == "attach"
                    && contract.attach_needs_server(spawn_provider)?)
            })
            .unwrap_or(false)
}

/// Hold the spawn gate for a daemon-bound thread spawn, waiting up to the
/// spawn's `wait_secs`. The guard releases the gate when the caller drops it
/// after the response read. A refusal prints its receipt and answers its exit.
fn hold_daemon_spawn_gate(
    home: &AgentsHome,
    params: &Value,
    agent_name: &str,
    lease: &mut SpawnLease,
    spawn_timing: Option<&mut fno_agent::spawn_seam::SpawnTiming>,
) -> Result<fno_agent::spawn_gate::GateGuard, i32> {
    // The relocated gate reads the same spawn-control flags the shared
    // construction reads.
    let daemon_gate_flags = fno_agent::spawn_gate::GateFlags::from_payload(params);
    let daemon_gate_seed = fno_agent::spawn_phase::params_seed(params);
    let daemon_gate_route_provider = fno_agent::claude_adopt::gate_route_provider(params);
    let daemon_wait = params.get("wait_secs").and_then(Value::as_f64);
    let config_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let gate_started = std::time::Instant::now();
    let mut gate_work = std::time::Duration::ZERO;
    let gate_result = fno_agent::spawn_bare::gate_with_wait(daemon_wait, || {
        let run_started = std::time::Instant::now();
        let result = fno_agent::spawn_gate::run_gate(
            &config_cwd,
            &home.registry_json(),
            fno_agent::spawn_gate::GateInput {
                name: agent_name.to_string(),
                substrate: "bg".into(),
                flags: daemon_gate_flags,
                seed: daemon_gate_seed.clone(),
                route_provider: daemon_gate_route_provider.clone(),
                node: fno_agent::spawn_gate::gate_node_from_params(params),
                ..fno_agent::spawn_promote::gate_input(lease.promotion.as_ref())
            },
        );
        gate_work += run_started.elapsed();
        result
    });
    if let Some(timing) = spawn_timing {
        timing.record_gate(gate_work, gate_started.elapsed().saturating_sub(gate_work));
    }
    drop(lease.roster_scope.take());
    gate_result.map_err(|refusal| {
        if let Some(receipt) = &refusal.receipt {
            println!("{receipt}");
        }
        refusal.exit_code
    })
}

/// Carry the client's stage timings to the daemon on a node-bound spawn.
fn stamp_stage_ms(req: &mut Request, spawn_timing: Option<&fno_agent::spawn_seam::SpawnTiming>) {
    let node_bound = req
        .params
        .get("node")
        .and_then(Value::as_str)
        .is_some_and(|node| !node.trim().is_empty());
    if !node_bound {
        return;
    }
    if let Some(timing) = spawn_timing {
        if let Some(params) = req.params.as_object_mut() {
            params.insert("stage_ms".into(), timing.client_stage_ms());
        }
    }
}

/// What a daemon `Ok` reply renders against: the command, the row it named,
/// and the client-side flags and snapshots read before the request moved.
struct DaemonReply<'run> {
    command: &'run str,
    agent_name: &'run str,
    home: &'run AgentsHome,
    flags: &'run RenderFlags,
    thread_portal_params: Option<&'run Value>,
}

impl DaemonReply<'_> {
    /// Print the reply and answer the exit code it earns.
    async fn render(&self, result: Value) -> i32 {
        let command = self.command;
        // The loaded-thread answer rides `list --harness codex` as a
        // field instead of a hidden command root. Gated on the codex
        // filter so no other list pays the app-server round trip; an
        // unreachable daemon becomes `available: false`, never a list
        // failure. The await must happen here - format_success is sync.
        let mut result = if command == "list"
            && result["filters_applied"]["provider"]
                .as_str()
                .map(Harness::parse)
                == Some(Harness::Codex)
        {
            let mut with_loaded = result;
            with_loaded["codex_loaded"] = fno_agent::codex_inject::loaded_threads_block(
                fno_agent::codex_inject::discover_loaded_threads().await,
            );
            with_loaded
        } else {
            result
        };
        // A registry rename moves fno's label only; the harness
        // session keeps its old title until the native rename
        // reaches it. Best-effort: the receipt names a failed push,
        // the rename still exits 0.
        if command == "rename" {
            fno_agent::rename::stamp_title_push(self.home, &mut result);
        }
        self.print(&result);
        // Keep drift warnings on read/removal commands stderr-only; `call`
        // already ensured a daemon, so only these commands pay the probe.
        if warns_on_daemon_drift(command) {
            let state = check_daemon_drift(self.home).await;
            if let Some(w) = drift_warning(&state, None) {
                eprintln!("{w}");
            }
        }
        if let Some(code) = refused_receipt_exit(command, &result) {
            return code;
        }
        // The daemon-bound thread lane (codex and every other
        // attach-with-server harness): the RPC created the row, so the
        // portal places here, after the receipt, exactly as the
        // client-side lanes do.
        if let Some(place_params) = self.thread_portal_params {
            if let Err(detail) = place_thread_portal_after_spawn(place_params, self.agent_name) {
                eprintln!("{detail}");
            }
        }
        0
    }

    /// Print the formatted receipt, or the pretty JSON when no formatter owns
    /// the command.
    fn print(&self, result: &Value) {
        let flags = self.flags;
        let Some(line) = format_success(
            self.command,
            self.agent_name,
            result,
            flags.json,
            flags.is_tty,
            flags.discover,
        ) else {
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
            return;
        };
        // ask FOLLOW-UP prints the reply verbatim with no added
        // newline, matching Python `sys.stdout.write(result.reply or "")`.
        // Every other formatted output (ask create short_id, stop/rm/list/reconcile)
        // keeps the trailing newline.
        let ask_followup = self.command == "ask"
            && !result
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        if ask_followup {
            print!("{line}");
        } else {
            println!("{line}");
        }
        // The unfiltered channel: the plain twin rides stderr even when a
        // filter rewrites or truncates stdout, so the spawn name survives.
        if self.command == "spawn" {
            eprintln!("{}", spawn_receipt_twin(self.agent_name, result));
        }
    }
}

/// The nonzero exit a daemon `Ok` receipt still earns, or None for success.
fn refused_receipt_exit(command: &str, result: &Value) -> Option<i32> {
    // A refused stop is not a success. The daemon answers
    // `stopped: false` over a turn whose interrupt never settled,
    // and the mux viewport's `run_agent_action` reads ONLY this
    // exit code. A 0 there prints "stopped <name>" over a worker
    // that is still running, which is the report the daemon arm
    // refuses to make. `Busy` is the shape: the turn holds the
    // thread, and a retry can still reach it.
    if command == "stop" && result.get("stopped").and_then(Value::as_bool) == Some(false) {
        return Some(18);
    }
    // change 3: a receipt over a surviving harness row is
    // the "reports success while removing nothing" shape that
    // stamped four sessions origin=adopted. The renderer already
    // prints the survival in its notes; the exit code now says it
    // too. An already-absent harness row is a COMPLETED removal,
    // not a survivor - the removal asked for is total, so it
    // keeps exit 0 and the sideline never renders a false
    // failure for it.
    let harness_survives = result.get("harness_removed").and_then(Value::as_bool) == Some(false)
        && !result
            .get("harness_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("already absent");
    if command == "rm" && harness_survives {
        return Some(21);
    }
    None
}

/// Report a failed daemon exchange; exit 1.
fn report_call_failure(command: &str, agent_name: &str, home: &AgentsHome, e: &ClientError) -> i32 {
    if command == "rm" && !agent_name.is_empty() {
        // A dead connection does not prove the daemon skipped the
        // removal; read the store rather than assert the outcome. An
        // unread store reports unknown instead of absent.
        let still_registered = match fno_agent::state::load_registry(&home.registry_json()) {
            Ok(reg) => Some(reg.find_name_or_full_session_id(agent_name).is_some()),
            Err(_) => None,
        };
        eprintln!(
            "{}",
            rm_failure_line(agent_name, &e.to_string(), still_registered)
        );
    } else {
        eprintln!("fno-agent: {e}");
    }
    1
}

fn validate_spawn_placement(params: &Value, substrate: &str) -> Result<(), String> {
    // R1: one typed read of the placement block at this boundary. A
    // field of an unexpected JSON type lands on None, the same answer a
    // misspelled or absent key gave the string-key reads this replaces.
    #[derive(serde::Deserialize, Default)]
    struct PlacementParams {
        squad: Option<String>,
        split: Option<String>,
        at: Option<String>,
        tab: Option<String>,
        portal: Option<u8>,
        portal_new: Option<bool>,
    }
    let p: PlacementParams = serde_json::from_value(params.clone()).unwrap_or_default();
    let squad = p.squad.as_deref();
    let split = p.split.as_deref();
    let at = p.at.as_deref();
    let tab = p.tab.as_deref();
    let portal = p.portal;
    let portal_new = p.portal_new == Some(true);

    if squad.is_some_and(|name| name.trim().is_empty()) {
        return Err("--workspace/-s needs a nonblank workspace name".into());
    }
    // A portal is the pane a thread hosts: the placement flags are
    // legal for a thread WHEN --portal names it (an index, or `new` where
    // the server picks the index), refused for a bare thread where they
    // mean nothing. Mirrors Python's placement_refusal, the one contract
    // both runtimes read; the portal's 0-255 range is enforced at the
    // parse arm, so a bad index never reaches this check.
    if (portal.is_some() || portal_new) && substrate != "bg" {
        return Err(
            "--portal applies only to the thread lane; a pane hosts its \
             own geometry and headless hosts no session at all"
                .into(),
        );
    }
    if at.is_some() && substrate == "bg" {
        return Err("--at needs --as pane (a thread has no calling pane)".into());
    }
    let placement_requested = squad.is_some() || split.is_some() || at.is_some() || tab.is_some();
    if placement_requested && substrate == "bg" && portal.is_none() && !portal_new {
        return Err("--workspace/-s, --split/-x, and --tab on the \
             thread lane need --portal N|new: a thread hosts no pane until a portal \
             opens one, so the placement has nothing to place"
            .into());
    }
    if placement_requested && portal.is_none() && !portal_new && substrate != "pane" {
        return Err(
            "--workspace/-s, --split/-x, --at, and --tab apply only to the pane lane \
             (thread/headless have no pane geometry)"
                .into(),
        );
    }
    if split.is_some_and(|direction| !matches!(direction, "left" | "right" | "up" | "down")) {
        return Err(format!(
            "--split/-x must be left, right, up, or down (got {:?})",
            split.unwrap_or_default()
        ));
    }
    if tab.is_some_and(|selector| selector.trim().is_empty()) {
        return Err("--tab needs a nonblank selector or pane-group name".into());
    }
    Ok(())
}

fn effective_spawn_message(message: &str, substrate: &str) -> String {
    if substrate == "pane" {
        message.to_owned()
    } else {
        fno_agent::spawn_payload::enrich_spawn_payload(message)
    }
}

/// Route a `spawn` (NOT host/promote) to the appropriate client-side path.
///
/// names the session substrate as one axis with three values; this arm
/// routes the two non-default ones client-side and falls through for `pane`.
/// - `pane` (default): owned interactive daemon pane -> None (fall through).
/// - claude + `bg`: dispatch_claude_spawn (the detached `claude --bg` thread).
/// - claude + `headless`: dispatch_claude_headless (the `claude -p` one-shot).
/// - codex/gemini/agy/opencode + `headless`: dispatch_*_once (one-shot, client-side).
/// - opencode + `bg`: dispatch_opencode_serve (persistent session on a shared
///   `opencode serve`,; detached `run --attach` writer streams events).
/// - codex + `bg`: daemon-hosted app-server thread; gemini/agy + `bg`: hard error.
/// - no resolvable / unknown provider: stderr usage error + exit 2.
///
/// Returns `Some(exit_code)` when handled client-side, `None` to fall through.
/// One-call portal placement on the Rust lanes, the twin of the
/// Python `thread_portal.place_thread_portal`: a thread spawn with
/// `--portal` ends with the portal open, through the same `fno mux thread`
/// reach a manual second command would type. The worker is already live, so
/// a placement failure is reported AFTER the spawn receipt and never
/// un-spawns anyone; the exit code stays the spawn's, because a nonzero
/// here reads as spawn failure to a retrying caller - a duplicate worker.
fn place_thread_portal_after_spawn(params: &Value, name: &str) -> Result<(), String> {
    // R1: the portal pick rides one typed read, the twin of the
    // validator's placement struct.
    #[derive(serde::Deserialize, Default)]
    struct PortalParams {
        portal: Option<u8>,
        portal_new: Option<bool>,
    }
    let p: PortalParams = serde_json::from_value(params.clone()).unwrap_or_default();
    let portal = p.portal;
    let portal_new = p.portal_new == Some(true);
    if portal.is_none() && !portal_new {
        return Ok(());
    }
    // The word --portal named: an index, or `new` for the server-picked
    // next free one. The literal forwards; this caller never computes an
    // index from a render (portals.md's repoint race).
    let portal_word = portal
        .map(|p| p.to_string())
        .unwrap_or_else(|| "new".into());
    // `--from current` names the calling pane: resolve it HERE, where
    // FNO_PANE still names the pane the spawn ran inside, and ride the
    // plain `--at` anchor the server splits beside. The literal never
    // forwards - the thread door has no calling pane to resolve it with.
    let mut from_current = false;
    if params.get("from").and_then(Value::as_str).map(str::trim) == Some("current") {
        from_current = true;
    }
    let mut args = vec![
        "mux".to_string(),
        "thread".to_string(),
        name.to_string(),
        "--portal".to_string(),
        portal_word.clone(),
    ];
    for (flag, key) in [
        ("--workspace", "squad"),
        ("--split", "split"),
        ("--at", "at"),
        ("--tab", "tab"),
        ("--from", "from"),
    ] {
        if from_current && key == "from" {
            continue;
        }
        if let Some(v) = params.get(key).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                args.push(flag.to_string());
                args.push(v.to_string());
            }
        }
    }
    if from_current {
        let fno_pane = std::env::var("FNO_PANE")
            .ok()
            .and_then(|s| s.trim().trim_start_matches('%').parse::<u64>().ok());
        match fno_pane {
            Some(pane) => {
                args.push("--at".to_string());
                args.push(pane.to_string());
            }
            None => {
                return Err(format!(
                    "--from current needs a calling pane; run the spawn \
                     inside a mux pane (FNO_PANE unset) or name the cell: \
                     `--from portal N` / `--from <worker>`"
                ));
            }
        }
    }
    let out = std::process::Command::new(fno_agent::scrape::fno_bin())
        .args(&args)
        .output()
        .map_err(|e| {
            format!(
                "portal {portal_word} placement failed: {e} (the worker is live; \
                 place it with 'fno mux thread {name} --portal {portal_word}')"
            )
        })?;
    if !out.status.success() {
        let raw = if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        let detail = String::from_utf8_lossy(raw).trim().to_string();
        let detail = if detail.is_empty() {
            "no output".to_string()
        } else {
            detail
        };
        return Err(format!(
            "portal {portal_word} placement failed: {detail} (the worker is live; \
             place it with 'fno mux thread {name} --portal {portal_word}')"
        ));
    }
    if !out.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&out.stdout));
    }
    Ok(())
}

fn maybe_run_spawn(
    home: &AgentsHome,
    params: &Value,
    name: &str,
    hold: &mut Option<fno_agent::spawn_guard::SpawnHold>,
    promotion: Option<&fno_agent::spawn_promote::Promotion>,
    spawn_timing: Option<&mut fno_agent::spawn_seam::SpawnTiming>,
) -> Option<i32> {
    client_spawn(home, params, name, hold, promotion, spawn_timing).unwrap_or_else(Some)
}

/// The client-side spawn, step by step. `Ok(None)` hands the spawn to the
/// daemon RPC; `Err(code)` is a refusal's exit before any lane launched.
fn client_spawn(
    home: &AgentsHome,
    params: &Value,
    name: &str,
    hold: &mut Option<fno_agent::spawn_guard::SpawnHold>,
    promotion: Option<&fno_agent::spawn_promote::Promotion>,
    spawn_timing: Option<&mut fno_agent::spawn_seam::SpawnTiming>,
) -> Result<Option<i32>, i32> {
    use fno_agent::claude_ask::py_repr;

    let lane = spawn_lane(home, params, name)?;
    let (output_format, mut route) = spawn_flag_seams(params, &lane)?;
    state_root_grant(params, &lane)?;
    let seed = spawn_seed(params, &lane)?;
    let provider = lane.provider;

    // Validate the provider FIRST so an unknown provider is a client-side
    // error (exit 2) for every substrate, never a fall-through to the daemon.
    if !KNOWN_PROVIDERS.contains(&provider) {
        eprintln!(
            "unknown provider {}; supported: {}",
            py_repr(provider),
            known_providers_csv()
        );
        return Err(2);
    }

    // `--print` answers here; a sandbox block writes its settings file.
    let sandbox_settings =
        fno_agent::spawn_bare::pre_gate(params, provider, name, route.as_mut(), &seed.cwd)?;
    // fno's computed writable-dir set, published by the Python spawn seam. A
    // worker that cannot write ~/.fno takes no node claim, and the graph then
    // reads that node free while it works. Read once here so every lane below
    // carries it.
    let state_dirs = fno_agent::claude_ask::state_dirs_from_env();
    let harness_args: Vec<String> = params
        .get("harness_args")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let route_env: Vec<(&str, &str)> = route
        .iter()
        .flat_map(|r| r.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .collect();

    let mut gate_guard = admit_spawn(
        home,
        params,
        name,
        &lane,
        &seed.cwd,
        hold,
        promotion,
        spawn_timing,
    )?;
    if seed.sandboxed {
        if let Err((code, reason)) = fno_agent::sandbox_launch::arm(home, &seed.cwd) {
            eprintln!("fno-agent spawn: [sandbox] refused: {reason}");
            if let Some(g) = gate_guard.as_mut() {
                g.release();
            }
            return Err(code);
        }
    }

    let call = SpawnCall {
        home,
        params,
        name,
        message: &seed.message,
        from_name: &seed.from_name,
        cwd: &seed.cwd,
        surface_cwd: seed.surface_cwd,
        timeout: seed.timeout,
        harness_args: &harness_args,
        route: route.as_ref(),
        route_env: &route_env,
        claude_flags: spawn_claude_flags(
            &state_dirs,
            &harness_args,
            route.as_ref(),
            sandbox_settings.as_deref(),
        ),
        output_format: output_format.as_deref(),
    };
    Ok(launch_spawn_lane(
        &call,
        provider,
        lane.substrate,
        &mut gate_guard,
    ))
}

/// Which lane a spawn names: its harness and its substrate.
struct SpawnLane<'p> {
    provider: &'p str,
    /// The historical selector: `bg` for a thread.
    substrate: &'p str,
    /// The name the capability tables key on: `thread` for `bg`.
    public_substrate: &'p str,
    /// The caller's cwd, which the route and role seams read.
    caller_cwd: std::path::PathBuf,
}

/// The spawn's lane, read once the message's own env side effects ran and
/// the name proved free.
fn spawn_lane<'p>(home: &AgentsHome, params: &'p Value, name: &str) -> Result<SpawnLane<'p>, i32> {
    use fno_agent::state::load_registry;

    // refusal carrier, Rust lane: the verdict is computed HERE, in the
    // one owner of the grammar (`merge_posture`), so a direct `fno-agent
    // spawn` gets the same posture as a Python-fronted one. It is applied to
    // this process's env and every child env below inherits it (model_env_scrub
    // keeps TARGET_NO_MERGE as protected bookkeeping). Idempotent with the
    // Python lane's application (`harness_map.apply_merge_posture_env` in
    // `cmd_spawn`): both answer from the same table, so running twice
    // converges.
    if let Some(message) = params.get("message").and_then(|v| v.as_str()) {
        fno_agent::merge_posture::apply_env_from_message(message);
    }

    // A payload command the node does not derive runs as named, with a notice.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some(notice) = fno_agent::node_seed::spawn_payload_command_notice(params, &cwd) {
        eprintln!("fno-agent spawn: notice: {notice}");
    }

    let provider_param = params.get("provider").and_then(|v| v.as_str());
    // `substrate` is a CLIENT-ONLY routing key: build_request validates and
    // inserts it (default `pane`) for the spawn command and this is its sole
    // consumer. It is never forwarded in a daemon-bound request (the `pane`
    // fall-through below sends params WITHOUT it mattering; the daemon ignores
    // unknown params).
    let substrate = params
        .get("substrate")
        .and_then(|v| v.as_str())
        .unwrap_or("thread");
    let substrate = if substrate == "thread" {
        "bg"
    } else {
        substrate
    };

    // unwrap_or_default is acceptable HERE (unlike the ask pre-check, which
    // must exit 12 on a corrupt registry): this collision check is advisory;
    // the authoritative read happens again under the per-agent lock inside
    // dispatch_claude_spawn / dispatch_*_once, which surface a corrupt
    // registry as exit 12.
    let registry = load_registry(&home.registry_json()).unwrap_or_default();

    // Collision check: name already exists -> error. An idle spare behind the
    // row does not refuse here, and neither does a proved-dead corpse (a
    // daemon-stamped Exited, Failed or PermanentDead row): the dispatch lane
    // reclaims the name under its own lock, and a refusal in the client would
    // preempt it. This pre-check is advisory; the dispatch lane re-checks
    // under the per-agent lock.
    if let Some(row) = registry.find(name) {
        if !fno_agent::spawn::name_collision::holder_name_free(row) {
            eprintln!(
                "{}",
                fno_agent::spawn::name_collision::refusal_line(name, row)
            );
            return Err(2);
        }
    }

    // Resolve provider: explicit --provider > invoking-harness inference >
    // builtin `claude` (mirrors Python's resolve_dispatch_harness). A missing
    // flag no longer exits 2 -- that was the bg/headless split-brain vs pane,
    // which already infers via the Python re-exec.
    let provider = match provider_param {
        Some(p) => p,
        None => fno_agent::provider::infer_dispatch_provider(|k| std::env::var(k).ok()),
    };
    let public_substrate = if substrate == "bg" {
        "thread"
    } else {
        substrate
    };
    Ok(SpawnLane {
        provider,
        substrate,
        public_substrate,
        caller_cwd: cwd,
    })
}

/// The flag seams the binary owns, before the gate takes a slot: monitor,
/// output format, route or role, then account. Returns the output format
/// and the resolved route.
fn spawn_flag_seams(
    params: &Value,
    lane: &SpawnLane<'_>,
) -> Result<(Option<String>, Option<fno_agent::spawn_route::Route>), i32> {
    let provider = lane.provider;
    let public_substrate = lane.public_substrate;
    // The row-9 flag seams: the refusals the binary owns print the Python
    // door's line bare and exit 2 before the gate takes a slot (the account
    // seam's ordering rationale, and the doors' shared Python order:
    // monitor gates, then output-format, then account, then route). A valid
    // pane monitor spawn never reaches here via the seam (the pane predicate
    // keeps it Python), so Launch falls through to the normal flow.
    match fno_agent::spawn_axes::monitor_seam(
        params.get("monitor").and_then(Value::as_str),
        provider,
        public_substrate,
    ) {
        fno_agent::spawn_axes::MonitorSeam::None | fno_agent::spawn_axes::MonitorSeam::Launch => {}
        fno_agent::spawn_axes::MonitorSeam::Refuse(line) => {
            eprintln!("{line}");
            return Err(2);
        }
    }
    let output_format = match fno_agent::spawn_axes::output_format_seam(
        params.get("output_format").and_then(Value::as_str),
        provider,
        public_substrate,
    ) {
        Ok(v) => v,
        Err(line) => {
            eprintln!("{line}");
            return Err(2);
        }
    };

    // The route arm, then the role arm (an explicit route wins): resolved
    // before the gate, so a refusal takes no slot, and before the account
    // scrub, which can remove the var a vendor key lives in.
    let cwd = &lane.caller_cwd;
    let env_get = |k: &str| std::env::var(k).ok();
    let text = |k: &str| params.get(k).and_then(Value::as_str);
    let route = match fno_agent::spawn_route::route_seam(
        text("route"),
        provider,
        public_substrate,
        cwd,
        &env_get,
    ) {
        fno_agent::spawn_route::RouteSeam::None => {
            fno_agent::spawn_route::role_seam(text("role"), provider, cwd, &env_get)
        }
        seam => seam,
    };
    let route = match route {
        fno_agent::spawn_route::RouteSeam::None => None,
        fno_agent::spawn_route::RouteSeam::Refuse(line) => {
            eprintln!("{line}");
            return Err(2);
        }
        fno_agent::spawn_route::RouteSeam::Routed(route) => {
            std::env::set_var(fno_agent::spawn_route::ROUTE_PROVIDER_ENV, &route.provider);
            Some(route)
        }
    };
    // The account arm: both account flags resolve here, before the gate.
    let route_env = route.as_ref().map(|r| &r.env);
    if let Some(code) = fno_agent::accounts::launch::seam_exit(params, provider, route_env) {
        return Err(code);
    }
    Ok((output_format, route))
}

/// R3, the state-root grant gate. Placed HERE because every non-pane spawn
/// passes through the client spawn exactly once, including the codex thread
/// spawn that completes over the daemon RPC: one gate evaluation per spawn,
/// on both routes. `pane` is excluded because it re-execs the Python CLI,
/// whose own seam spends the grant.
fn state_root_grant(params: &Value, lane: &SpawnLane<'_>) -> Result<(), i32> {
    // The capability table is keyed on the public substrate name, never the
    // historical `bg` selector.
    // Skipped for a provider this binary does not know: the dispatch arms below
    // already refuse an unknown provider with exit 2 and the supported list,
    // which is the message that reader needs. Refusing first with grant advice
    // would shadow it with a diagnosis of the wrong problem.
    let provider = lane.provider;
    if lane.substrate != "pane" && KNOWN_PROVIDERS.contains(&provider) {
        let declared_substrate = lane.public_substrate;
        let roots = fno_agent::claude_ask::state_dirs_from_env();
        if let Err(refusal) =
            fno_agent::spawn_gate::state_root_grant_gate(provider, declared_substrate, &roots)
        {
            fno_agent::spawn_gate::emit_refusal_event(
                &refusal,
                // R1: the shrink-only read counter bars a new .get on Value;
                // a JSON pointer is the total read (None on non-object too).
                params
                    .pointer("/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                declared_substrate,
                None,
            );
            return Err(refusal.exit_code);
        }
    }
    Ok(())
}

/// What the worker launches with, resolved once the lane passed its seams.
struct SpawnSeed {
    message: String,
    from_name: String,
    cwd: std::path::PathBuf,
    /// The receipt names the cwd: the dispatch moved the worker.
    surface_cwd: bool,
    sandboxed: bool,
    timeout: Option<std::time::Duration>,
}

/// The worker's seed message, sender, cwd and sandbox verdict.
fn spawn_seed(params: &Value, lane: &SpawnLane<'_>) -> Result<SpawnSeed, i32> {
    let substrate = lane.substrate;
    let enriched_message = effective_spawn_message(
        params.get("message").and_then(|v| v.as_str()).unwrap_or(""),
        substrate,
    );
    let from_name =
        fno_agent::mail_cli::send::stamp_from(params.get("from_name").and_then(|v| v.as_str()));
    let (cwd, surface_cwd) = spawn_worker_cwd(params, substrate)?;
    // `[sandbox] provider`: refuse a spawn the sandbox cannot carry before the
    // guard and the gate; the sandbox itself comes up after the gate admits.
    let sandboxed =
        match fno_agent::sandbox_launch::check(&cwd, lane.provider, lane.public_substrate) {
            Ok(sandboxed) => sandboxed,
            Err((code, reason)) => {
                eprintln!("fno-agent spawn: {reason}");
                return Err(code);
            }
        };

    // The fleet-law block composes for the WORKER's cwd (a --cwd foreign-repo
    // spawn must not inherit the spawner's repo freezes), after the dispatch
    // cwd resolves. A pane composes nowhere here: its Python leg or the
    // server arm owns it with the true seat cwd.
    let (message, fleet_laws) = if substrate == "pane" {
        (enriched_message, Vec::new())
    } else {
        fno_agent::spawn_payload::finalize_spawn_payload_at(&enriched_message, &cwd)
    };
    if !fleet_laws.is_empty() {
        // The receipt names the laws the seed carries: stderr keeps
        // stdout/JSON clean for the line-by-line receipt consumers.
        eprintln!(
            "fno-agent spawn: seed carries {} active fleet decision(s): {}",
            fleet_laws.len(),
            fleet_laws.join("; ")
        );
    }
    Ok(SpawnSeed {
        message,
        from_name,
        cwd,
        surface_cwd,
        sandboxed,
        timeout: opt_timeout(params),
    })
}

/// The worker's cwd and whether the receipt surfaces it.
fn spawn_worker_cwd(params: &Value, substrate: &str) -> Result<(std::path::PathBuf, bool), i32> {
    // --cwd > --here (caller) > default canonical; resolve_dispatch_cwd
    // canonicalizes an explicit --cwd and shells to git on the default path
    // (no --cwd, no --here). Resolve only for CLIENT-SIDE spawns, which are the
    // non-`pane` substrates (bg + headless).
    // The `pane` substrate falls through to the daemon RPC, which resolves
    // canonical itself; resolving here too would double the git call and the
    // redirect note (review MEDIUM 3).
    // `surface_cwd` is the move decision resolve_dispatch_cwd already
    // made (the note condition), so the receipt's cwd field couples to the note
    // with no second comparison. pane re-execs Python and resolves canonical
    // itself, so it neither resolves cwd here nor surfaces it.
    if substrate == "pane" {
        return Ok((std::path::PathBuf::new(), false));
    }
    let caller = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // An explicit --cwd (or the node's worktree, spawn_bare's render)
    // wins over the node row's recorded checkout, as on the daemon path.
    let node_cwd = match params.get("cwd") {
        Some(_) => Ok(None),
        None => fno_agent::spawn_cwd::node_cwd_or_refuse(params, &caller),
    };
    match node_cwd {
        Err(msg) => {
            eprintln!("fno-agent: refusing to dispatch: {msg}");
            Err(2)
        }
        Ok(Some(project)) => {
            let moved = project != caller;
            Ok((project, moved))
        }
        Ok(None) => Ok(fno_agent::spawn_cwd::resolve_dispatch_cwd(params)),
    }
}

/// Admission: the family-2 node guard, then the spawn gate. Returns the gate
/// guard a client-side launch holds; a pane or a codex thread holds none.
#[allow(clippy::too_many_arguments)]
fn admit_spawn(
    home: &AgentsHome,
    params: &Value,
    name: &str,
    lane: &SpawnLane<'_>,
    cwd: &std::path::Path,
    hold: &mut Option<fno_agent::spawn_guard::SpawnHold>,
    promotion: Option<&fno_agent::spawn_promote::Promotion>,
    mut spawn_timing: Option<&mut fno_agent::spawn_seam::SpawnTiming>,
) -> Result<Option<fno_agent::spawn_gate::GateGuard>, i32> {
    let substrate = lane.substrate;
    // Client-side dispatch holds admission until its row or reservation is visible.
    // (AC19): the codex THREAD spawn's registry row is created by the
    // DAEMON spawn RPC (the codex thread lane returns None), so a
    // guard held here drops before any row exists and two rapid thread spawns
    // both pass the cap. The gate moves to run(), wrapped around that RPC.
    let codex_thread_fallthrough =
        lane.provider.as_harness() == Harness::Codex && substrate == "bg";
    // The family-2 node guard runs after the cwd it judges and before the
    // gate. The caller owns the hold, so a codex thread keeps its keys across
    // the daemon RPC, and every exit before a proven launch releases them.
    let caller_session = fno_agent::spawn_promote::gate_input(promotion).caller_session;
    let guard_started = std::time::Instant::now();
    *hold =
        fno_agent::spawn_guard::guard_client_spawn(params, name, cwd, caller_session.as_deref())?;
    if let Some(timing) = spawn_timing.as_deref_mut() {
        timing.record_guard(guard_started.elapsed());
    }
    let gate_guard = if substrate == "pane" || codex_thread_fallthrough {
        None
    } else {
        Some(run_spawn_gate(home, params, name, substrate, promotion)?)
    };
    // A codex thread's gate runs later, around the daemon RPC, and closes
    // the roster scope there.
    if !codex_thread_fallthrough {
        fno_agent::backlog::worked::close_roster_scope();
    }
    Ok(gate_guard)
}

/// The spawn gate, waiting up to `--wait` for a slot.
fn run_spawn_gate(
    home: &AgentsHome,
    params: &Value,
    name: &str,
    substrate: &str,
    promotion: Option<&fno_agent::spawn_promote::Promotion>,
) -> Result<fno_agent::spawn_gate::GateGuard, i32> {
    let flags = fno_agent::spawn_gate::GateFlags::from_payload(&params);
    let config_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let wait_secs = params.get("wait_secs").and_then(Value::as_f64);
    match fno_agent::spawn_bare::gate_with_wait(wait_secs, || {
        fno_agent::spawn_gate::run_gate(
            &config_cwd,
            &home.registry_json(),
            fno_agent::spawn_gate::GateInput {
                name: name.to_string(),
                substrate: substrate.to_string(),
                flags,
                seed: fno_agent::spawn_phase::params_seed(&params),
                route_provider: fno_agent::claude_adopt::gate_route_provider(&params),
                node: fno_agent::spawn_gate::gate_node_from_params(&params),
                account: params
                    .get("account")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                session_phase: params
                    .get("session_phase")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                background_job: fno_agent::spawn_contract::background_job(&params, substrate),
                ..fno_agent::spawn_promote::gate_input(promotion)
            },
        )
    }) {
        Ok(g) => Ok(g),
        Err(refusal) => {
            // One line by default; the receipt rides --json.
            if params.get("json_out").and_then(Value::as_bool) == Some(true) {
                if let Some(receipt) = &refusal.receipt {
                    println!("{receipt}");
                }
            }
            Err(refusal.exit_code)
        }
    }
}

/// The claude-only bundle, resolved once for both claude lanes.
/// The old tier-3 bundle (add-dir/agent/tools/deny-tools) no longer
/// arrives as typed fields: --add-dir/--agent/--tools ride the fenced
/// passthrough with every other harness flag, and --deny-tools refuses at
/// the door. The builders keep their parameters; the call sites pass
/// neutrals.
fn spawn_claude_flags<'a>(
    state_dirs: &'a [String],
    harness_args: &'a [String],
    route: Option<&'a fno_agent::spawn_route::Route>,
    settings: Option<&'a str>,
) -> fno_agent::claude_ask::HarnessFlags<'a> {
    fno_agent::claude_ask::HarnessFlags {
        add_dir: None,
        state_dirs,
        agent: None,
        allowed_tools: None,
        disallowed_tools: None,
        passthrough: harness_args,
        route: route.map(|r| fno_agent::claude_ask::RouteStamp {
            provider: &r.provider,
            model: &r.model,
            settings: &r.settings,
        }),
        settings,
    }
}

/// One admitted spawn, as each launch lane reads it.
struct SpawnCall<'a> {
    home: &'a AgentsHome,
    params: &'a Value,
    name: &'a str,
    message: &'a str,
    from_name: &'a str,
    cwd: &'a std::path::Path,
    surface_cwd: bool,
    timeout: Option<std::time::Duration>,
    harness_args: &'a [String],
    route: Option<&'a fno_agent::spawn_route::Route>,
    route_env: &'a [(&'a str, &'a str)],
    claude_flags: fno_agent::claude_ask::HarnessFlags<'a>,
    output_format: Option<&'a str>,
}

// Each provider module defines its OWN AskOutcome struct (nominally
// distinct types), so `emit!` prints+returns inline per arm rather than via
// one shared closure that could not name all four types.
macro_rules! emit {
    ($outcome:expr) => {{
        let outcome = $outcome;
        if !outcome.stderr.is_empty() {
            eprint!("{}", outcome.stderr);
        }
        if !outcome.stdout.is_empty() {
            print!("{}", outcome.stdout);
        }
        Some(outcome.exit_code)
    }};
}

/// Launch an admitted spawn on its substrate's lane. `None` hands the spawn
/// to the daemon RPC.
fn launch_spawn_lane(
    call: &SpawnCall<'_>,
    provider: &str,
    substrate: &str,
    gate_guard: &mut Option<fno_agent::spawn_gate::GateGuard>,
) -> Option<i32> {
    match substrate {
        // pane: the caller sends it to the pane spine before this fn.
        "pane" => None,
        "bg" => launch_thread_lane(call, provider, gate_guard),
        "headless" => launch_headless_lane(call, provider),
        // claude cloud: the lane module owns the arm (axis refusals, the
        // pty-held create, refusal by name). The gate guard drops as the
        // client spawn ends.
        "cloud" => fno_agent::claude_cloud::client_spawn_arm(
            call.home,
            call.name,
            call.message,
            call.from_name,
            call.cwd,
            call.surface_cwd,
            provider,
            call.params,
        ),
        // Unreachable: provider is validated known above and substrate is
        // validated to pane|bg|headless in build_request.
        _ => None,
    }
}

/// The thread lanes: a persistent worker that outlives this process.
fn launch_thread_lane(
    call: &SpawnCall<'_>,
    provider: &str,
    gate_guard: &mut Option<fno_agent::spawn_gate::GateGuard>,
) -> Option<i32> {
    let SpawnCall {
        home,
        params,
        name,
        message,
        from_name,
        cwd,
        timeout,
        harness_args,
        ..
    } = *call;
    match provider {
        "claude" => launch_claude_thread(call, gate_guard),
        "opencode" => launch_opencode_thread(call, gate_guard),
        // footnote (ACP) and muse (MSP): an fno-owned host holds the session.
        h @ ("footnote" | "muse") => emit!(fno_agent::thread_host::dispatch(
            home, h, "bg", name, message, from_name, cwd, timeout, params
        )),
        // Codex thread is supervisor-hosted by the daemon. Returning `None`
        // preserves the request's `substrate=thread` so the daemon can own the
        // held app-server process and register its full thread identity.
        "codex" => None,
        // The keeper-lane thread spawns (pi, cursor-agent, grok, agy): the
        // keeper_spawn arm reads the harness's capability row and launches
        // the keeper from the binary, with no Python crossing. A keeper row
        // names the harness and the row's refusals answer the unmeasured
        // stances, so the guard is the row itself.
        other if fno_agent::keeper_spawn::carries_keeper_row(other) => {
            let outcome = fno_agent::keeper_spawn::dispatch(
                home,
                other,
                name,
                message,
                from_name,
                cwd,
                harness_args,
                params,
            );
            if !outcome.stderr.is_empty() {
                eprint!("{}", outcome.stderr);
            }
            if !outcome.stdout.is_empty() {
                print!("{}", outcome.stdout);
            }
            // The row exists once dispatch returned: release the gate now,
            // like the claude arm, so a waiter never queues behind this
            // spawn's post-receipt work.
            if let Some(g) = gate_guard.as_mut() {
                g.release();
            }
            Some(outcome.exit_code)
        }
        // The three named lanes above stay NAMED rather than lane-routed:
        // claude's thread is hosted by the detached client, codex's by its own
        // app-server, opencode's by a serve-hosted HTTP session. `thread_lane`
        // classifies all three `attach`, and that one label cannot tell a
        // client-side re-attach from a daemon-owned serve process, so each
        // needs its own dispatch call.
        // The REFUSAL is derived, because a provider name list goes stale the
        // moment a lane is built. Hard error pointing to headless; never a
        // silent substrate swap.
        other => {
            eprintln!(
                "{}",
                fno_agent::harness_capabilities::thread_substrate_refusal(other)
            );
            Some(2)
        }
    }
}

/// claude bg: the detached `claude --bg` thread (appears in `claude
/// agents`; attach/peek/reply; NOT a grid pane). claude-only by nature.
fn launch_claude_thread(
    call: &SpawnCall<'_>,
    gate_guard: &mut Option<fno_agent::spawn_gate::GateGuard>,
) -> Option<i32> {
    use fno_agent::claude_ask::{dispatch_claude_spawn, ClaudeHome};

    let SpawnCall {
        home,
        params,
        name,
        message,
        from_name,
        cwd,
        surface_cwd,
        timeout,
        route,
        route_env,
        claude_flags,
        ..
    } = *call;
    let native = fno_agent::spawn_bare::native_lane(params, name, route, cwd);
    if let Some(code) = native {
        return Some(code);
    }
    let claude_home = ClaudeHome::from_env();
    let daemon_receipt = match fno_agent::claude_ask::preflight_claude_daemon(&claude_home) {
        Ok(fno_agent::claude_ask::ClaudeDaemonPreflight::Ready(receipt)) => Some(receipt),
        Ok(fno_agent::claude_ask::ClaudeDaemonPreflight::NeedsBootstrap) => None,
        Err(error) => {
            eprintln!(
                "claude spawn refused: harness=claude observed=unreadable remedy=repair the Claude daemon roster: {error}"
            );
            if let Some(g) = gate_guard.as_mut() {
                g.release();
            }
            return Some(13);
        }
    };
    // The refusal carrier rides the inherited env (see the note in
    // spawn_lane), so extra_env carries only a route's env: a worker that
    // drops the flag post-compaction still folds the refusal at init.
    let mut outcome = dispatch_claude_spawn(
        home,
        &claude_home,
        name,
        message,
        from_name,
        cwd,
        false,
        timeout,
        route_env,
        None,
        None,
        route.and_then(|r| r.effort.as_deref()),
        claude_flags,
        surface_cwd,
    );
    if outcome.exit_code == 0 {
        let daemon_receipt = match daemon_receipt {
            Some(receipt) => Ok(receipt),
            None => fno_agent::claude_ask::ensure_claude_daemon(&claude_home),
        };
        match daemon_receipt {
            Ok(receipt) => fno_agent::claude_ask::stamp_daemon_receipt(&mut outcome, &receipt),
            Err(error) => {
                outcome.exit_code = 13;
                outcome.stdout.clear();
                outcome.stderr.push_str(&format!(
                    "claude spawn refused after create: harness=claude observed=unreadable remedy=inspect the Claude daemon roster: {error}\n"
                ));
            }
        }
    }
    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }
    // Flush the receipt BEFORE the bounded QoS roster poll so
    // line-parsing consumers never wait on the demotion.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // The roster row exists once dispatch returned: release the gate
    // NOW so the ~10s demotion poll never serializes other spawns
    // behind the spawn-gate mutex (codex P2).
    if let Some(g) = gate_guard.as_mut() {
        g.release();
    }
    if outcome.exit_code == 0 {
        demote_claude_thread(&outcome.stdout);
    }
    Some(outcome.exit_code)
}

/// The bg worker is claude's child (its exec can't be wrapped); demote it
/// post-hoc via the roster pid. short_id from the JSON receipt line (parsed,
/// not string-split - gemini HIGH).
fn demote_claude_thread(receipt: &str) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(receipt.trim()).ok();
    if let Some(sid) = parsed
        .as_ref()
        .and_then(|v| v.get("short_id"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
    {
        let config_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        fno_agent::spawn_gate::qos_demote_bg_worker(&config_cwd, sid);
    }
}

/// opencode bg: the serve-HTTP worker lane. A shared
/// `opencode serve` hosts a persistent session bound to the worker
/// cwd; a detached `opencode run --attach` writer streams the turn's
/// JSON events to the log. The spawn returns immediately - the session
/// on the serve IS the worker (steering/mail over the API is a filed
/// follow-up).
fn launch_opencode_thread(
    call: &SpawnCall<'_>,
    gate_guard: &mut Option<fno_agent::spawn_gate::GateGuard>,
) -> Option<i32> {
    let SpawnCall {
        home,
        params,
        name,
        message,
        from_name,
        cwd,
        harness_args,
        route,
        ..
    } = *call;
    // The serve lane carries one harness flag, the model. Any other
    // token refuses by name: a dropped flag runs a worker that is
    // not what the spawn asked for.
    let model = fno_agent::claude_ask::passthrough_model(harness_args);
    if let Some(extra) = fno_agent::opencode_serve::uncarried_flag(harness_args) {
        eprintln!(
            "refused: the opencode thread lane carries only --model; {extra} cannot ride it. \
             Spawn on the pane lane (--split) to pass other opencode flags."
        );
        if let Some(g) = gate_guard.as_mut() {
            g.release();
        }
        return Some(2);
    }
    emit!(fno_agent::opencode_serve::dispatch_opencode_serve(
        home,
        name,
        message,
        from_name,
        cwd,
        model,
        route.and_then(|r| r.effort.as_deref()),
        params.get("node").and_then(|v| v.as_str()),
    ))
}

/// The one-shot lanes of the vendor CLIs: each runs one turn to completion
/// and exits. Model selection rides each lane's own config.
fn launch_headless_lane(call: &SpawnCall<'_>, provider: &str) -> Option<i32> {
    use fno_agent::claude_ask::{dispatch_claude_headless, ClaudeHome};
    use fno_agent::gemini_ask::dispatch_gemini_once;
    use fno_agent::opencode_ask::dispatch_opencode_once;

    let SpawnCall {
        home,
        name,
        message,
        from_name,
        cwd,
        timeout,
        harness_args,
        route_env,
        claude_flags,
        output_format,
        ..
    } = *call;
    match provider {
        // claude headless: a truly headless `claude -p` one-shot (no thread, no
        // grid row; runs to completion and exits). The one place claude shells
        // `-p` (Locked Decision 4); ask/relay keep `--bg`.
        "claude" => {
            let claude_home = ClaudeHome::from_env();
            emit!(dispatch_claude_headless(
                home,
                &claude_home,
                name,
                message,
                from_name,
                cwd,
                false,
                timeout,
                None,
                None,
                None,
                output_format,
                claude_flags,
                route_env,
            ))
        }
        "codex" => launch_codex_headless(call),
        "gemini" => emit!(dispatch_gemini_once(
            home, name, message, from_name, cwd, false, timeout, None,
        )),
        // opencode headless: the client-side one-shot `opencode run
        // --dangerously-skip-permissions` (the bare TUI stays the `pane` form).
        // The flag is NOT `--auto`: that spelling is stale vendor docs.
        "opencode" => emit!(dispatch_opencode_once(
            home,
            name,
            message,
            from_name,
            cwd,
            false,
            timeout,
            None,
            None,
            harness_args,
        )),
        other => launch_other_headless_lane(call, other),
    }
}

/// codex headless: the client-side `codex --exec` one-shot.
fn launch_codex_headless(call: &SpawnCall<'_>) -> Option<i32> {
    // The daemon receipt is teardown telemetry, never a precondition: a
    // codex whose app-server cannot boot (stubbed CLI, partial install,
    // CI) still runs `codex --exec`, like the Python lane. Inject and
    // mail ensure their own daemon.
    let daemon_receipt = match fno_agent::codex_inject::ensure_codex_daemon() {
        Ok(result) => Some(result.receipt),
        Err(error) => {
            eprintln!("spawn: harness=codex daemon-ensure degraded: {error}");
            None
        }
    };
    let mut outcome = fno_agent::codex_ask::dispatch_codex_once(
        call.home,
        call.name,
        call.message,
        call.from_name,
        call.cwd,
        false,
        call.timeout,
        None,
        None,
        None,
        call.harness_args,
        call.route.and_then(|r| r.codex.as_ref()),
    );
    if let Some(receipt) = daemon_receipt.as_ref() {
        fno_agent::codex_ask::append_daemon_receipt(&mut outcome, receipt);
    }
    emit!(outcome)
}

/// The one-shot lanes of the newer harnesses and the fno-hosted ones.
fn launch_other_headless_lane(call: &SpawnCall<'_>, provider: &str) -> Option<i32> {
    use fno_agent::agy_ask::dispatch_agy_once_with_effort;

    let SpawnCall {
        home,
        params,
        name,
        message,
        from_name,
        cwd,
        timeout,
        harness_args,
        ..
    } = *call;
    match provider {
        // pi and omp headless: one `--print --mode json` turn (pi_ask.rs).
        harness @ ("pi" | "omp") => emit!(fno_agent::pi_ask::dispatch_pi_once(
            harness,
            home,
            name,
            message,
            from_name,
            cwd,
            timeout,
            None,
            None,
            harness_args,
        )),
        // agy is stateless (plain text, no session id): a one-shot `agy -p`
        // under its lane-default bypass posture.
        "agy" => emit!(dispatch_agy_once_with_effort(
            home,
            name,
            message,
            from_name,
            cwd,
            None,
            None,
            timeout,
            None,
            harness_args,
        )),
        "grok" => emit!(fno_agent::grok_ask::dispatch_grok_once(
            home,
            name,
            message,
            from_name,
            cwd,
            None,
            None,
            false,
            None,
            timeout,
            harness_args,
        )),
        // zcode headless: the one-shot `-p` lane with identity read-back. No
        // model/effort axis (the row's model_switch_strategy is unsupported);
        // the row's -m refusal fires upstream in the gate.
        "zcode" => emit!(fno_agent::zcode_ask::dispatch_zcode_once(
            home,
            name,
            message,
            from_name,
            cwd,
            false,
            None,
            timeout,
            harness_args,
            params.get("node").and_then(|v| v.as_str()),
        )),
        // footnote (ACP) and muse (MSP): an fno-owned host holds the session.
        h @ ("footnote" | "muse") => emit!(fno_agent::thread_host::dispatch(
            home, h, "headless", name, message, from_name, cwd, timeout, params
        )),
        _ => None,
    }
}

/// Surface for an `ask` that resolved to no known provider: a create with no
/// `--provider` (or an unknown one). Reproduces Python's `select_provider`
/// exit-2 error text byte-for-byte (`dispatch.py` wraps both the
/// `_check_known_provider` ValueError and the "provider is required for new
/// agent" ValueError as `DispatchAskError(..., exit_code=2)`, which `cmd_ask`
/// prints to stderr verbatim). Never routes to the daemon (Locked Decision 3).
fn unresolvable_ask_exit(params: &Value, name: &str) -> i32 {
    use fno_agent::claude_ask::py_repr;
    let provider_param = params.get("provider").and_then(|v| v.as_str());
    let msg = match provider_param {
        // `select_provider` validates the requested provider FIRST, so an
        // unknown `--provider` surfaces the "unknown provider" error.
        Some(p) if !KNOWN_PROVIDERS.contains(&p) => format!(
            "unknown provider {}; supported: {}",
            py_repr(p),
            known_providers_csv()
        ),
        // New agent with no resolvable provider.
        _ => format!(
            "provider is required for new agent {}; pass --provider one of: {}",
            py_repr(name),
            known_providers_csv()
        ),
    };
    eprintln!("{}", msg);
    2
}

/// One-line pointers for the commands retired at G4: the grid, the
/// WebSocket drive surface, and the interactive daemon PTY hosting behind
/// `host`/`promote` moved to the mux. Returns `None` for a live command. Callers
/// print the pointer and exit non-zero so a script never reads a retired command
/// as a silent success (AC5-EDGE).
fn retired_command_pointer(command: &str) -> Option<&'static str> {
    match command {
        "grid" => Some(
            "fno agent grid was retired at G4: agent panes now live in the mux. \
             Open `fno mux`, or script panes with `fno mux pane ls|read|run|send|wait|kill`.",
        ),
        "drive" => Some(
            "fno agent drive was retired at G4: drive an agent pane in the mux. \
             Use `fno mux pane send <pane> --raw ...` for keystrokes (without --raw the payload is wrapped in an <fno_mail> envelope), or open `fno mux` and type into the pane.",
        ),
        "host" => Some(
            "fno agent host was retired at G4: open `fno mux` (the spawn command \
             launches in the mux itself; the lane template owns the placement).",
        ),
        "promote" => Some(
            "fno agent promote is now fno agent hq promote <session> --scope <scope>; \
             the old pane command was retired at G4 (a spawn lands in the mux by default).",
        ),
        _ => None,
    }
}

/// Dispatch `fno-agent status`: probe an already-running daemon and print its
/// `status-v1.json` (with `--json`), or the human arms table plus daemon lines
/// (default). The control-plane arms readout is read straight from the event
/// journals, so it survives a down daemon: exit 13 still signals the daemon,
/// but the arms rows print either way.
async fn run_status(json_out: bool) -> i32 {
    let home = AgentsHome::from_env();
    let req = Request::new(1, "agent.status", Value::Object(Map::new()));
    match call_if_running(&home, &req).await {
        Ok(resp) => match resp.payload {
            ResponsePayload::Err(err) => {
                eprintln!("fno-agent: {}", err.message);
                exit_code_for(err.code)
            }
            ResponsePayload::Ok(result) => print_live_status(&home, result, json_out),
        },
        Err(ClientError::DaemonNotRunning) => {
            // The arms table is exactly what a dead control plane needs to
            // show; print it beside the down-daemon signal rather than nothing.
            print_degraded_status(&home, fno_agent::tick_ledger::DaemonFacts::Down, json_out);
            eprintln!("fno-agent: daemon not running");
            13
        }
        Err(e) => {
            // Unreachable daemon (socket error, timeout, ...): same degraded
            // shape as DaemonNotRunning - the arms readout stands on its own.
            // Daemon rules do not fire on Unknown, so stale rows read
            // `unexplained` rather than blaming a daemon of unknown health.
            print_degraded_status(
                &home,
                fno_agent::tick_ledger::DaemonFacts::Unknown,
                json_out,
            );
            eprintln!("fno-agent: {e}");
            1
        }
    }
}

/// The live daemon's status: its payload with the arms, the attention set,
/// the stuck work and the drift verdict folded in, then the drift warning.
fn print_live_status(home: &AgentsHome, mut result: Value, json_out: bool) -> i32 {
    // One drift read feeds both the stderr warning below and the
    // facts the arms rows are explained against.
    let drift = drift_from_status(&result);
    let uptime_s = result
        .pointer("/daemon/uptime_secs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let drifted = matches!(drift, DriftState::Drifted { .. });
    // The served fold when the daemon carries one for these
    // journals, the local journal fold otherwise (an old daemon
    // answers without `arms_fold`).
    let (mut arms, trace) = fno_agent::status_render::arms_for_status(home, &result);
    fno_agent::arm_repair::explain(
        &mut arms,
        &fno_agent::tick_ledger::DaemonFacts::Up { uptime_s, drifted },
        &trace,
    );
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "arms".into(),
            serde_json::to_value(&arms).unwrap_or(Value::Null),
        );
        // The Rust-owned attention set beside the full list, so
        // every consumer reads the same verdict.
        obj.insert(
            "arms_attention".into(),
            serde_json::to_value(fno_agent::status_render::arms_attention(&arms))
                .unwrap_or(Value::Null),
        );
        // What work is stuck right now: hung commands and dead
        // flight holders, from the one stuck_work read.
        obj.insert(
            "stuck_work".into(),
            fno_agent::stuck_work::status_value(
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
        );
        // change 4: the drift verdict as a field, so the
        // census reads it from JSON instead of regex-parsing the
        // stderr sentence.
        obj.insert("drift".into(), json!(fno_agent::drift::drift_label(&drift)));
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    } else {
        fno_agent::status_render::print_status_human(&result, &arms);
    }
    // Drift warning, stderr-only so --json/automation
    // consumers of stdout are never contaminated. We already hold the
    // status payload, so classify from it without a second RPC.
    let pid = result
        .get("daemon")
        .and_then(|d| d.get("pid"))
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    if let Some(w) = drift_warning(&drift, pid) {
        eprintln!("{w}");
    }
    0
}

/// The status readout with no daemon answer: the journal arms table in the
/// degraded payload shape.
fn print_degraded_status(
    home: &AgentsHome,
    facts: fno_agent::tick_ledger::DaemonFacts,
    json_out: bool,
) {
    let (mut arms, trace) = fno_agent::status_render::arms_readout(home);
    fno_agent::arm_repair::explain(&mut arms, &facts, &trace);
    let payload = fno_agent::status_render::degraded_status_payload(&arms);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        fno_agent::status_render::print_status_human(&payload, &arms);
    }
}

/// `fno agent reap`: manual row retirement. Runs the same
/// `gc_sweep` the daemon runs on its idle tick, operating on the registry
/// directly under the shared flock (no daemon required), and reports what it
/// did: every row retired with its basis, and for each row KEPT, the named
/// gate holding it, so a stuck row is never silent and invisible, and a
/// zero-reap pass over a live fleet is never silent about the rows it kept.
/// The grace window is resolved from `config.agents.retire_grace_s` exactly
/// as the daemon does.
///
/// `--dry-run` runs the identical classification with no registry write and no
/// `agent_row_reaped` event - a reaper an operator cannot rehearse is one they
/// will not run.
fn run_reap(rest: &[String]) -> i32 {
    let json_out = rest.iter().any(|a| a == "--json" || a == "-J");

    if rest.iter().any(|arg| arg == "--state-files-only") {
        return reap_state_files(rest, json_out);
    }

    // The verify probe (task 5): read-only audit of the receipts
    // store over `--since`, pinned to THIS build. Nonzero exit on any
    // unmet condition - empty window, stale build, partial effects - so the
    // plan's done probe cannot pass on CI-green alone.
    if rest.iter().any(|a| a == "--verify") {
        return reap_verify(rest, json_out);
    }

    // The release command: apply a ruling to one escalated hold through the
    // sweep's own door. Classify, refuse fresh holds and open work by name,
    // apply, print the whole-sweep receipt. The keep mark: set, clear, or
    // list the user's standing exception. Both parse beside the sweep; the
    // binary shrinks by the moved parse.
    if let Some(code) = fno_agent::reap_release::from_args(rest) {
        return code;
    }
    if let Some(code) = fno_agent::keep_mark::from_args(rest) {
        return code;
    }
    reap_rows(rest, json_out)
}

/// `reap --state-files-only`: the state-file pass for this cwd, rehearsed
/// unless `--apply`.
fn reap_state_files(rest: &[String], json_out: bool) -> i32 {
    let apply = rest.iter().any(|arg| arg == "--apply");
    let explicit_dry_run = rest.iter().any(|arg| arg == "--dry-run");
    if apply && explicit_dry_run {
        eprintln!("fno-agent: reap --state-files-only cannot combine --apply and --dry-run");
        return 2;
    }
    let extras: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|arg| {
            !matches!(
                *arg,
                "--state-files-only" | "--apply" | "--dry-run" | "--json" | "-J"
            )
        })
        .collect();
    if !extras.is_empty() {
        eprintln!(
            "fno-agent: reap --state-files-only takes only --apply/--dry-run/--json (got: {})",
            extras.join(" ")
        );
        return 2;
    }
    let home = AgentsHome::from_env();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let summary = fno_agent::gc_sweep::reap_state_files_for_cwd(
        &home,
        &cwd,
        fno_agent::agents_config::state_reap_config(&cwd),
        apply,
    );
    print!(
        "{}",
        fno_agent::reap_render::render_state_files_reap(&summary, json_out)
    );
    return i32::from(fno_agent::gc_sweep::state_reap_has_failures(&summary));
}

/// `reap --verify`: the receipts audit over `--since`, pinned to this build.
fn reap_verify(rest: &[String], json_out: bool) -> i32 {
    let (since, expected) = match verify_window(rest) {
        Ok(window) => window,
        Err(code) => return code,
    };
    let home = AgentsHome::from_env();
    let report = fno_agent::gc_verify::verify(&home, since, &expected);
    if json_out {
        println!("{}", report.to_json());
    } else {
        println!(
            "verified {} of {} receipt(s) in the last {}s (build {})",
            report.verified.len(),
            report.checked,
            since,
            fno_agent::gc_verify::current_build()
        );
        for v in &report.verified {
            println!(
                "  ok {} {} ({}) at {}",
                v.harness, v.session_id, v.row_name, v.reaped_at
            );
            for effect in &v.effects {
                println!("    {effect}");
            }
        }
        for p in &report.problems {
            println!("  REFUSED {}: {}", p.receipt, p.reason);
        }
        for s in &report.skipped {
            println!("  SKIP {}: {}", s.receipt, s.reason);
        }
    }
    return if report.passes() { 0 } else { 1 };
}

/// The `--verify` window and cohort: `--since <dur>` (default 24h) and
/// `--expect-sessions <a,b>`. `Err` carries the usage exit.
fn verify_window(rest: &[String]) -> Result<(u64, Vec<String>), i32> {
    let since = match rest.iter().position(|a| a == "--since") {
        Some(i) => match rest
            .get(i + 1)
            .and_then(|v| fno_agent::duration::parse_duration_secs(v))
        {
            Some(secs) => secs,
            None => {
                eprintln!(
                    "fno-agent: --since needs a duration like 24h (got: {:?})",
                    rest.get(i + 1).map(String::as_str).unwrap_or("")
                );
                return Err(2);
            }
        },
        None => 24 * 3600,
    };
    // Exactly --verify, --since <dur>, --expect-sessions <a,b>,
    // --json/-J: anything else is a usage error, checked with a plain
    // skip-list walk.
    let mut extras: Vec<&str> = Vec::new();
    let mut expected: Vec<String> = Vec::new();
    let mut cohort_given = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--verify" | "--json" | "-J" => {}
            "--since" => {
                i += 1; // the duration value rides with --since
            }
            "--expect-sessions" => {
                cohort_given = true;
                let value = rest.get(i + 1).map(String::as_str).unwrap_or("");
                if value.is_empty() {
                    eprintln!(
                            "fno-agent: --expect-sessions needs a comma-separated session list (got: {:?})",
                            rest.get(i + 1).map(String::as_str).unwrap_or("")
                        );
                    return Err(2);
                }
                expected.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                );
                i += 1; // the cohort value rides with --expect-sessions
            }
            other => extras.push(other),
        }
        i += 1;
    }
    // A cohort given but normalizing to nothing ("," or whitespace) must
    // be a usage error, never a silently disabled check (codex P2).
    if cohort_given && expected.is_empty() {
        eprintln!(
            "fno-agent: --expect-sessions normalized to an empty cohort; name at least one session"
        );
        return Err(2);
    }
    if !extras.is_empty() {
        eprintln!(
            "fno-agent: reap --verify takes only --since/--expect-sessions/--json (got: {})",
            extras.join(" ")
        );
        return Err(2);
    }
    Ok((since, expected))
}

/// The row sweep's switches.
struct ReapFlags {
    dry_run: bool,
    no_mux: bool,
    only_done: bool,
    apply: bool,
}

/// Parse the row sweep's switches; `Err` carries the usage exit.
fn reap_flags(rest: &[String]) -> Result<ReapFlags, i32> {
    let dry_run = rest.iter().any(|a| a == "--dry-run");
    let no_mux = rest.iter().any(|a| a == "--no-mux");
    let only_done = rest.iter().any(|a| a == "--done");
    let apply = rest.iter().any(|a| a == "--apply");
    let extras: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|a| {
            *a != "--json"
                && *a != "-J"
                && *a != "--dry-run"
                && *a != "--no-mux"
                && *a != "--done"
                && *a != "--apply"
        })
        .collect();
    if !extras.is_empty() {
        eprintln!(
            "fno-agent: reap takes no arguments other than --json/--dry-run/--no-mux/--done/--apply (got: {})",
            extras.join(" ")
        );
        return Err(2);
    }
    if apply && !only_done {
        eprintln!(
            "fno-agent: reap --apply only rides --done (the bulk all-clear): `reap --done --apply`"
        );
        return Err(2);
    }
    Ok(ReapFlags {
        dry_run,
        no_mux,
        only_done,
        apply,
    })
}

/// The row sweep: the daemon's gc pass (or the scoped `--done` pass), the
/// team sweep and the mux tab sweep, then the receipt.
fn reap_rows(rest: &[String], json_out: bool) -> i32 {
    let ReapFlags {
        dry_run,
        no_mux,
        only_done,
        apply,
    } = match reap_flags(rest) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    let home = AgentsHome::from_env();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let grace_secs = fno_agent::agents_config::retire_grace_secs(&cwd) as i64;
    // The bulk all-clear: --apply executes the scoped pass; without it the
    // pass rehearses, whatever --dry-run says.
    if only_done && apply && dry_run {
        eprintln!("fno-agent: reap --done takes --apply or --dry-run, not both");
        return 2;
    }
    let effective_dry = if only_done { !apply } else { dry_run };
    // What this invocation runs: `--done` scopes the whole command to the
    // done/eval pass. The team sweep and the mux tab sweep touch rows the
    // scoped pass never judges, so a scoped run runs neither, apply or dry.
    let plan = reap_plan(only_done);
    let teams = if plan.teams {
        // Dead teams first, matching the arm's in-arm order.
        Some(fno_agent::team_reap::production_sweep(
            &home,
            &cwd,
            !effective_dry,
        ))
    } else {
        None
    };
    let mut summary = if only_done {
        if apply {
            fno_agent::gc::gc_sweep_done_only(
                &home,
                &fno_agent::events::EventEmitter::new(home.events_jsonl(), "daemon"),
                grace_secs,
                fno_agent::agents_config::reap_receipt_retain_days(&cwd),
            )
        } else {
            fno_agent::gc::gc_sweep_done_only_dry_run(&home, grace_secs)
        }
    } else if dry_run {
        fno_agent::daemon::gc_sweep_dry_run(&home, grace_secs)
    } else {
        // Source "daemon": the manual command is the same operation as the tick.
        let emitter = fno_agent::events::EventEmitter::new(home.events_jsonl(), "daemon");
        fno_agent::daemon::gc_sweep(
            &home,
            &emitter,
            grace_secs,
            fno_agent::agents_config::reap_receipt_retain_days(&cwd),
        )
    };
    summary.mark_escalated(fno_agent::agents_config::hold_escalate_after(&cwd));
    summary.teams = teams;

    // The dry-run JSON read also carries the census (task 4): who would
    // retire and what was seen, one read. Text dry-runs carry it too now:
    // the text receipt prints the counts beside the judged rows instead of
    // the census living only in the JSON read.
    let inventory = if effective_dry {
        Some(fno_agent::gc_inventory::census(&home))
    } else {
        None
    };
    // The mux sideline sweep: the registry pass above reaps rows,
    // but ghost panes are the surface an operator SEES. The sweep body stays
    // the one prune command (reused, not reimplemented); `--no-mux` skips it.
    // The manual command keeps `--include-used-shells`: closing a human's spent
    // shells is an attended choice, never the daemon's default. A scoped
    // `--done` run skips it entirely: the tabs belong to rows the scoped
    // pass never judged.
    let mux = if no_mux || !plan.mux_tabs {
        fno_agent::reap_render::MuxSweep::Skipped
    } else {
        fno_agent::gc::mux_tab_sweep(None, &cwd, effective_dry, true)
    };
    print!(
        "{}",
        fno_agent::reap_render::render_reap_with_inventory(
            &summary,
            inventory.as_ref(),
            Some(&mux),
            json_out,
            effective_dry
        )
    );
    0
}

/// What one `reap` invocation runs. `--done` scopes the whole command to the
/// done/eval pass: the team sweep and the mux tab sweep touch rows the
/// scoped pass never judges, so a scoped run runs neither, apply or dry.
struct ReapPlan {
    teams: bool,
    mux_tabs: bool,
}

fn reap_plan(only_done: bool) -> ReapPlan {
    ReapPlan {
        teams: !only_done,
        mux_tabs: !only_done,
    }
}

#[cfg(test)]
mod reap_tests {
    use super::*;

    /// The scope contract the merge hold names: a scoped `--done` run runs
    /// the done/eval pass alone. The unscoped run keeps both sidelines.
    #[test]
    fn a_done_scoped_reap_runs_neither_sideline_sweep() {
        let plan = reap_plan(true);
        assert!(!plan.teams, "a scoped run never sweeps teams");
        assert!(!plan.mux_tabs, "a scoped run never sweeps mux tabs");
        let plain = reap_plan(false);
        assert!(plain.teams && plain.mux_tabs, "the unscoped run keeps both");
    }
}

#[cfg(test)]
mod spawn_parse_tests {
    use super::*;

    /// The bare hand-off (`spawn --promote <scope> --hand-off`, no `--name`)
    /// parses without a name: the seam skips the mint for a hand-off on
    /// purpose, and the settle draw fills the name at plan. Every other
    /// nameless spawn still demands a name at the parse.
    #[test]
    fn a_bare_hand_off_parses_without_a_name() {
        let rest: Vec<String> = ["--promote", "alpha", "--hand-off", "--print"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (method, params) = build_request("spawn", &rest).unwrap();
        assert_eq!(method, "agent.spawn");
        assert!(params.get("name").is_none(), "the draw owns the name");
        assert_eq!(params["promote"], serde_json::json!(["alpha"]));
        assert_eq!(params["hand_off"], serde_json::json!(true));
    }

    #[test]
    fn a_dry_run_flag_parses_into_the_spawn_params() {
        let rest: Vec<String> = ["--promote", "alpha", "--hand-off", "--dry-run"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert!(params
            .as_object()
            .is_some_and(|m| m.contains_key("dry_run")));
    }

    /// The equals-form incident: an owned scalar flag reached
    /// the unowned-passthrough arm (it contains `=`), so `--dry-run=true`
    /// rode the harness argv and the spawn LAUNCHED - the exact opposite of
    /// what the caller asked. The equals form must parse like the space form.
    #[test]
    fn an_equals_form_dry_run_parses_into_the_spawn_params() {
        let rest: Vec<String> = ["--promote", "alpha", "--hand-off", "--dry-run=true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        // The parse only ever inserts true, so presence pins the value.
        assert!(params
            .as_object()
            .is_some_and(|m| m.contains_key("dry_run")));
        assert!(
            !params
                .as_object()
                .is_some_and(|m| m.contains_key("harness_args")),
            "the flag must never ride the harness argv"
        );
    }

    #[test]
    fn an_equals_form_false_scalar_parses_as_absent() {
        let rest: Vec<String> = ["--name", "w", "--dry-run=false", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert!(!params
            .as_object()
            .is_some_and(|m| m.contains_key("dry_run")));
        assert!(!params
            .as_object()
            .is_some_and(|m| m.contains_key("harness_args")));
    }

    #[test]
    fn an_equals_form_scalar_with_a_non_bool_refuses() {
        let rest: Vec<String> = ["--name", "w", "--dry-run=maybe", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = build_request("spawn", &rest).unwrap_err();
        assert!(err.contains("true or false"), "got: {err}");
    }

    #[test]
    fn an_equals_form_hand_off_parses_into_the_spawn_params() {
        let rest: Vec<String> = ["--promote", "alpha", "--hand-off=true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert_eq!(params["hand_off"], serde_json::json!(true));
        assert!(!params
            .as_object()
            .is_some_and(|m| m.contains_key("harness_args")));
    }

    /// The unowned-flag law keeps its edge: a foreign equals-form flag still
    /// rides the harness argv verbatim, and the harness's own scalar pairs
    /// forward through the harness-args arm.
    #[test]
    fn an_unowned_equals_form_flag_still_rides_the_harness_argv() {
        let rest: Vec<String> = ["--name", "w", "--dangerously-skip-permissions=true", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert_eq!(
            params.pointer("/harness_args"),
            Some(&serde_json::json!(["--dangerously-skip-permissions=true"]))
        );
    }

    #[test]
    fn an_explicit_spawned_by_marks_the_relay_leg() {
        // The ssh relay's carried parent edge stamps the explicit marker the
        // placement guard reads; ambient provenance never claims it.
        let rest: Vec<String> = ["--name", "quinlan@mbp3", "--spawned-by", "s-parent", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert_eq!(
            params.get("spawned_by_session"),
            Some(&serde_json::json!("s-parent"))
        );
        assert_eq!(
            params.get("spawned_by_explicit"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn a_nameless_spawn_without_a_hand_off_still_refuses() {
        let err = build_request("spawn", &[]).unwrap_err();
        assert_eq!(err, "spawn needs a <name> or --name");
        let bare_hand_off: Vec<String> = ["--hand-off"].iter().map(|s| s.to_string()).collect();
        let err = build_request("spawn", &bare_hand_off).unwrap_err();
        assert_eq!(err, "spawn needs a <name> or --name");
    }

    #[test]
    fn unowned_space_form_before_the_prompt_refuses() {
        // `spawn w --resume abc msg`: --resume in space form, before the
        // prompt, is the retired passthrough spelling (amends the passthrough
        // law): it refuses and points at the two legal forms.
        let rest: Vec<String> = ["w", "--resume", "abc", "msg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = build_request("spawn", &rest).unwrap_err();
        assert!(
            err.contains("space form"),
            "the refusal names the retired spelling: {err}"
        );
    }

    #[test]
    fn a_portal_new_spawn_parses_the_wire_key() {
        // `--portal new` asks the server for the next free index: the
        // literal rides `portal_new` so no caller ever computes an index
        // from a stale render (the portals.md repoint race). An index
        // still rides `portal`, and a word that is neither refuses with
        // the thread door's wording. The asserts walk the object's
        // entries: no string-key read, no slice indexing.
        let find = |params: &Value, key: &str| {
            params
                .as_object()
                .expect("spawn params are an object")
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        };
        let rest: Vec<String> = ["--name", "kite", "--portal", "new", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert_eq!(find(&params, "portal_new"), Some(serde_json::json!(true)));
        assert_eq!(
            find(&params, "portal"),
            None,
            "no index is computed client-side"
        );
        let rest: Vec<String> = ["--name", "kite", "--portal", "2", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, params) = build_request("spawn", &rest).unwrap();
        assert_eq!(find(&params, "portal"), Some(serde_json::json!(2)));
        let rest: Vec<String> = ["--name", "kite", "--portal", "left", "fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = build_request("spawn", &rest).unwrap_err();
        assert_eq!(err, "--portal takes an index 0-255 or new");
    }

    #[test]
    fn a_portal_new_thread_counts_as_a_named_portal_for_the_gates() {
        // `--portal new` satisfies the thread lane's portal gates exactly
        // like an explicit index: legal on the thread lane, a named portal
        // for the placement flags, refused on the pane lane.
        let params = serde_json::json!({"portal_new": true});
        validate_spawn_placement(&params, "bg").expect("new places on the thread lane");
        validate_spawn_placement(&params, "pane")
            .expect_err("a portal belongs to the thread lane only");
        let placed = serde_json::json!({"portal_new": true, "split": "left"});
        validate_spawn_placement(&placed, "bg")
            .expect("new names the portal the placement flags need");
        let bare: serde_json::Value = serde_json::json!({"split": "left"});
        let err = validate_spawn_placement(&bare, "bg").unwrap_err();
        assert!(
            err.contains("--portal"),
            "a bare thread still refuses without a portal: {err}"
        );
    }
}

/// `fno-agent roster-reap`: the roster-side sweep (gap one). Dry-run
/// by default; `--apply` executes. Takes only --json/--apply.
fn run_roster_reap(rest: &[String]) -> i32 {
    let json_out = rest.iter().any(|a| a == "--json" || a == "-J");
    let dry_run = !rest.iter().any(|a| a == "--apply");
    let extras: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json" && *a != "-J" && *a != "--apply")
        .collect();
    if !extras.is_empty() {
        eprintln!(
            "fno-agent: roster-reap takes no arguments other than --json/--apply (got: {})",
            extras.join(" ")
        );
        return 2;
    }
    let home = AgentsHome::from_env();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let grace_secs = fno_agent::agents_config::retire_grace_secs(&cwd) as i64;
    let scope = fno_agent::agents_config::roster_scope(&cwd);
    let summary = fno_agent::roster_reap::roster_reap(&home, &cwd, grace_secs, scope, dry_run);
    print!(
        "{}",
        fno_agent::roster_reap::render(&summary, json_out, dry_run)
    );
    if summary.nothing_resolved() {
        eprintln!(
            "fno-agent: roster-reap refused: probed {} row(s), the truth probe resolved none",
            summary.probed
        );
        return 1;
    }
    0
}

/// `fno-agent node-route --names a,b --json`: the provenance cascade
/// verdict per NAME. One graph read, one JSON answer per name:
/// `retire-eligible` when the cascade resolves a done node and the PR
/// confirm passes, `held` / `open` / `unresolved` otherwise. A name is
/// never resolved from anything but the declared sources.
fn run_node_route(rest: &[String]) -> i32 {
    let (names, pairs) = match node_route_args(rest) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let home = AgentsHome::from_env();
    let graph = fno_agent::gc_sweep::read_graph_entries(&home);
    let mut answers = serde_json::Map::new();
    for name in &names {
        answers.insert(name.clone(), name_route_answer(name, graph.as_ref()));
    }
    // The transcript-quiet pair leg: a session-keyed member whose registry
    // row went stale still reads LIVE while its transcript is fresh - the
    // same activity evidence the reap's quiet gate trusts. `harness:sid`
    // answers `live` (fresh), `quiet` (stale), or `unresolved` (no store
    // read); a name never consults a transcript it has no session for.
    if !pairs.is_empty() {
        pair_route_answers(&pairs, &mut answers);
    }
    println!("{}", serde_json::Value::Object(answers));
    0
}

/// The `--names` and `--pairs` lists; `Err` carries the usage exit.
fn node_route_args(rest: &[String]) -> Result<(Vec<String>, Vec<String>), i32> {
    let mut names: Vec<String> = Vec::new();
    let mut pairs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--names" => {
                let Some(value) = rest.get(i + 1) else {
                    eprintln!("fno-agent: node-route --names needs a comma-separated list");
                    return Err(2);
                };
                names.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string),
                );
                i += 2;
            }
            "--pairs" => {
                let Some(value) = rest.get(i + 1) else {
                    eprintln!("fno-agent: node-route --pairs needs a comma-separated list");
                    return Err(2);
                };
                pairs.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_string),
                );
                i += 2;
            }
            "--json" | "-J" => i += 1,
            other => {
                eprintln!("fno-agent: node-route takes only --names/--pairs/--json (got: {other})");
                return Err(2);
            }
        }
    }
    if names.is_empty() && pairs.is_empty() {
        eprintln!("fno-agent: node-route needs --names or --pairs");
        return Err(2);
    }
    Ok((names, pairs))
}

/// One name's provenance cascade verdict.
fn name_route_answer(name: &str, graph: Option<&fno_agent::gc_sweep::GraphRead>) -> Value {
    let mut entry = fno_agent::state::RegistryEntry::new(
        None,
        fno_agent::state::Lineage::unproven("synthetic row for a read, never written"),
    );
    entry.name = name.to_string();
    match graph {
        None => serde_json::json!({"state": "graph-unreadable"}),
        Some(g) => {
            let verdict = fno_agent::gc_sweep::provenance_verdict(&entry, "", g, None, None);
            let node = verdict.route.node.clone();
            let basis = format!(
                "via {}",
                verdict
                    .route
                    .source
                    .map(|s| s.as_str())
                    .unwrap_or("sessions")
            );
            match (&verdict.work, &verdict.hold) {
                (_, Some(hold)) => serde_json::json!({
                    "state": "held",
                    "node": node,
                    "reason": hold.as_str(),
                }),
                (fno_agent::graph_store::WorkState::AllDone { .. }, None) => {
                    serde_json::json!({"state": "retire-eligible", "node": node, "basis": basis})
                }
                (fno_agent::graph_store::WorkState::Open { node: n, status }, None) => {
                    serde_json::json!({"state": "open", "node": node, "reason": format!("{n} {status}")})
                }
                (fno_agent::graph_store::WorkState::NoProvenance, None) => {
                    serde_json::json!({"state": "unresolved"})
                }
            }
        }
    }
}

/// The transcript-quiet answer for each `harness:sid` pair.
fn pair_route_answers(pairs: &[String], answers: &mut serde_json::Map<String, Value>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let grace_secs = fno_agent::agents_config::retire_grace_secs(&cwd) as i64;
    let mut store = fno_agent::gc_inventory::HarnessStoreIndex::default();
    for pair in pairs {
        let Some((harness, sid)) = pair.split_once(':') else {
            answers.insert(
                pair.clone(),
                serde_json::json!({"state": "unresolved", "reason": "not harness:sid"}),
            );
            continue;
        };
        let mut entry = fno_agent::state::RegistryEntry::new(
            Some(sid.to_string()),
            fno_agent::state::Lineage::unproven("synthetic row for a read, never written"),
        );
        entry.harness = Some(harness.to_string());
        let answer = match store.matches(&entry) {
            // the age is the newest timestamped entry through the
            // shared probe, not a file stat.
            Some(hits) if !hits.is_empty() => {
                // The same batched seam the sweeps take, not a deleted
                // per-row wrapper: the entry's harness_session_id is set,
                // so `row_handle` returns it and that is the key the map
                // answers under.
                let age = fno_agent::gc::probe_entry_ages(&[&entry])
                    .get(&fno_agent::gc::row_handle(&entry))
                    .copied()
                    .flatten();
                match age {
                    Some(age) if age <= grace_secs => serde_json::json!({"state": "live"}),
                    Some(_) => serde_json::json!({"state": "quiet"}),
                    None => serde_json::json!({"state": "unresolved"}),
                }
            }
            _ => serde_json::json!({"state": "unresolved"}),
        };
        answers.insert(pair.clone(), answer);
    }
}

/// Apply the owned-interactive (drivable grid pane) defaults to a spawn/host
/// request. Sets `host_mode=interactive`; for claude additionally defaults the
/// PTY lane (`mode=interactive`) and mints a `session_id` when none is pinned or
/// resumed (the daemon's single-writer claim + transcript discovery key on it).
///
/// Shared by `host` (always interactive) and `spawn` (default for PTY providers
/// unless `--once`) so the claude mint lives in exactly ONE place. An
/// explicit `--mode` wins, so `--mode stream_json` opts a claude spawn back out
/// of the PTY lane. Non-claude providers get only `host_mode`; their create argv
/// stays byte-unchanged (the mint is claude-only, mirroring the host contract).
fn apply_interactive_defaults(params: &mut Map<String, Value>) -> Result<(), String> {
    params.insert(
        "host_mode".into(),
        Value::String(fno_agent::state::HOST_MODE_INTERACTIVE.into()),
    );
    if params
        .get("provider")
        .and_then(Value::as_str)
        .map(Harness::parse)
        == Some(Harness::Claude)
    {
        // claude has two interactive lanes; default the owned-PTY pane unless the
        // caller explicitly picked one via --mode.
        if !params.contains_key("mode") {
            params.insert(
                "mode".into(),
                Value::String(fno_agent::state::CLAUDE_MODE_INTERACTIVE.into()),
            );
        }
        let is_pty_lane = params.get("mode").and_then(Value::as_str)
            == Some(fno_agent::state::CLAUDE_MODE_INTERACTIVE);
        if is_pty_lane && !params.contains_key("session_id") && !params.contains_key("resume_id") {
            // The crate's one mint; a getrandom failure surfaces instead of
            // launching with an invented pin.
            let id = fno_agent::identity::mint_fno_id()?;
            params.insert("session_id".into(), Value::String(id));
        }
    }
    Ok(())
}

fn build_request(command: &str, rest: &[String]) -> Result<(String, Value), String> {
    let RequestArgs {
        mut params,
        positional,
        fence_at,
        ..
    } = parse_request_args(command, rest).map_err(|r| r.0)?;
    let method = match command {
        "spawn" => {
            finish_spawn_request(&mut params, positional, fence_at).map_err(|r| r.0)?;
            "agent.spawn"
        }
        "ask" => {
            fno_agent::ask_project::parse(&mut params, &positional)?;
            "agent.ask"
        }
        "list" => "agent.list",
        "status" => "agent.status",
        "stop" => {
            let name = positional.first().ok_or("stop needs a <name>")?;
            params.insert("name".into(), Value::String(name.clone()));
            "agent.stop"
        }
        "rm" => {
            let name = positional.first().ok_or("rm needs a <name>")?;
            params.insert("name".into(), Value::String(name.clone()));
            "agent.rm"
        }
        "rename" => {
            fno_agent::rename::request(&mut params, &positional)?;
            "agent.rename"
        }
        // Only a `--substrate` resume reaches here; a bare resume is the
        // client-side re-entry, intercepted before build_request. The argv
        // is re-parsed through the resume parser rather than read off
        // `params`, so the one refusal vocabulary answers both doors.
        "resume" => {
            let parsed = fno_agent::resume_args::parse_conversion_args(rest)?;
            params.insert("name".into(), Value::String(parsed.name));
            params.insert("dry_run".into(), Value::Bool(parsed.dry_run));
            params.insert("allow_new_id".into(), Value::Bool(parsed.allow_new_id));
            "agent.convert"
        }
        "reconcile" => "agent.reconcile",
        other => {
            return Err(format!(
                "unknown command: {other} (expected {}); see `fno agent --help`",
                ALL_CLIENT_ACTIONS.join("|")
            ))
        }
    };

    Ok((method.to_string(), Value::Object(params)))
}

// Click/Typer accepts `--flag=value` for every string option; the Python
// path forwards e.g. `fno agent ask <name> <msg> --cwd=/repo --timeout=30
// --from-name=bot --provider=codex` verbatim. Since `ask` now auto-routes to
// this client for EVERY provider, the binary must accept the
// equals form for ALL value-carrying flags, not just --provider/--from --
// otherwise a routed `--cwd=...` / `--timeout=...` / `--from-name=...`
// regresses to "unknown flag" instead of reaching the dispatch (Codex P2 on
// PR #379; same regression class as PR #371's --provider=). Normalize
// `--flag=value` into two tokens up front so the space-form match arms below
// handle both syntaxes uniformly.
const VALUE_FLAGS: &[&str] = &[
    "--provider",
    "--harness",
    "--from",
    "--cwd",
    "--message",
    "--name",
    "--node",
    "--slug",
    "--plan",
    "--session-phase",
    "--session-id",
    "--status",
    "--progress",
    "--from-name",
    "--to-project",
    "--answer",
    "--timeout",
    "--model",
    "--mode",
    "--substrate",
    "--workspace",
    "--squad",
    "--pane",
    "--split",
    "--portal",
    "--tab",
    "--machine",
    "--at",
    "--permission-mode",
    "--effort",
    "--add-dir",
    "--audit-actor",
    "--audit-reason",
    "--audit-request-id",
    "--audit-reclaimed-bytes",
    "--agent",
    "--tools",
    "--deny-tools",
    "--account",
    "--harness-arg",
    "--role-level",
    "--role-scope",
    "--promote",
    "--take-over",
    "--output-format",
    "--monitor",
    "--dispatch-account",
    "--role",
];

/// Normalize `--flag=value` into two tokens and fold phone dashes, leaving
/// an `--argv` payload or a `--` fenced tail verbatim.
///
/// On spawn the split is ownership-driven, not table-driven: a flag the
/// boundary calls spawn-owned ([`fno_agent::spawn_bare::spawn_owned`],
/// the same predicate that refuses it after the prompt) must parse in the
/// equals form, never ride the harness argv. A scalar owned flag
/// ([`SCALAR_OWNED_FLAGS`]) folds to its
/// bare form on `=true` and drops on `=false`; any other value refuses.
///
/// The hand-off dry-run incident: `--dry-run=true` reached the unowned-passthrough arm
/// (it contains `=`), rode the harness argv, and the spawn launched - the
/// exact opposite of the flag's job.
fn normalize_value_flags(command: &str, rest: &[String]) -> Result<Vec<String>, RequestRefusal> {
    let mut normalized: Vec<String> = Vec::with_capacity(rest.len());
    let mut rest_iter = rest.iter();
    while let Some(tok0) = rest_iter.next() {
        // The phone fold (the 2026-11-09 spawn-argv ruling, part F): a token-leading U+2014/U+2013 folds
        // into `--` so a phone-typed flag survives; the fence checks below
        // run on the folded token, so a phone-typed `--` fence still stops
        // the pass and the payload stays byte-exact.
        let tok = fno_agent::cli_args::fold_phone_dashes(std::slice::from_ref(tok0))
            .pop()
            .unwrap_or_else(|| tok0.clone());
        // Everything after a bare `--argv` is the provider command line, which
        // the `--argv` normalize arm collects verbatim. Do NOT normalize
        // equals-form tokens in that payload -- a downstream tool's
        // `--timeout=5` must survive untouched. Copy the rest verbatim and stop.
        if tok == "--argv" {
            normalized.push(tok);
            normalized.extend(rest_iter.cloned());
            break;
        }
        // The bare `--` seed fence gets the same verbatim treatment: a
        // fenced `--timeout=5 do X` seed must reach the positional drain
        // as-is, not be split by the equals-form rewrite below.
        if tok == "--" {
            normalized.push(tok);
            normalized.extend(rest_iter.cloned());
            break;
        }
        // the equals-form split is for LONG flags only. The short
        // value flags (-p/-c/-t) take a space-separated value (`-p claude`),
        // matching Click/Typer's short-option command convention; the
        // `-p=value` form is intentionally not normalized here. The
        // phone-motivating surface types the space form, and shorts are
        // additive aliases, not a new syntax.
        if tok.starts_with("--") {
            if let Some(eq) = tok.find('=') {
                let (flag, value) = (&tok[..eq], tok[eq + 1..].to_string());
                // Spawn-owned first: the boundary's own predicate decides,
                // so the parse can never disagree with the refusal that
                // guards the same flag after the prompt.
                if command == "spawn" && fno_agent::spawn_bare::spawn_owned(&tok) {
                    if fno_agent::spawn_bare::SCALAR_OWNED_FLAGS.contains(&flag) {
                        match value.as_str() {
                            "true" => normalized.push(flag.to_string()),
                            "false" => {}
                            other => {
                                return Err(RequestRefusal(format!(
                                    "{flag} takes true or false in the = form, got {other:?}"
                                )))
                            }
                        }
                    } else {
                        normalized.push(flag.to_string());
                        normalized.push(value);
                    }
                    continue;
                }
                if VALUE_FLAGS.contains(&flag) {
                    normalized.push(flag.to_string());
                    normalized.push(value);
                    continue;
                }
            }
        }
        normalized.push(tok);
    }
    Ok(normalized)
}

/// A refused request line: the message the caller prints before exit 2.
struct RequestRefusal(String);

impl From<String> for RequestRefusal {
    fn from(message: String) -> Self {
        RequestRefusal(message)
    }
}

impl From<&str> for RequestRefusal {
    fn from(message: &str) -> Self {
        RequestRefusal(message.to_string())
    }
}

type RequestTokens = std::iter::Peekable<std::vec::IntoIter<String>>;

/// One request line, parsed: the params the flags filled and the tokens
/// left for the command's own arm.
#[derive(Default)]
struct RequestArgs {
    params: Map<String, Value>,
    positional: Vec<String>,
    argv: Option<Vec<String>>,
    /// Where a `--` fence drained the remaining tokens, counted in pre-fence
    /// positionals; spawn uses it to tell a passthrough tail from a seed.
    fence_at: Option<usize>,
    /// Non-spawn commands collect --harness/-H through the loop (the resume
    /// command keeps the spelling); on spawn scan reads it as the one live
    /// binary axis.
    harness_val: Option<String>,
    vendor_val: Option<String>,
}

/// Parse a request line into params, then fold the axes in.
fn parse_request_args(command: &str, rest: &[String]) -> Result<RequestArgs, RequestRefusal> {
    let mut normalized = normalize_value_flags(command, rest)?;
    // The positional boundary pre-pass lives in the spawn seam
    // (spawn_seam::admit_spawn), on the caller's raw argv: normalize would
    // otherwise inject --name and reorder the tail, and the refusal would
    // judge a line the caller never typed. This parse sees the seam's
    // already-fenced form.
    // on spawn the head's axis flags parse ONCE through the shared
    // SpawnAxes schema (the same parser the spawn-overlay command runs), and the
    // axis tokens leave the loop's input below. The values seed the same
    // params and fields the old axis arms fed, after the loop.
    let axes = if command == "spawn" {
        let (axes, fence) = SpawnAxes::scan(&normalized)?;
        normalized = SpawnAxes::strip_axes(&normalized, fence);
        Some(axes)
    } else {
        None
    };
    let mut req = RequestArgs::default();
    let mut it = normalized.into_iter().peekable();
    while let Some(a) = it.next() {
        let taken = parse_axis_flag(command, &a, &mut it, &mut req)?
            || parse_seat_flag(command, &a, &mut it, &mut req)?
            || parse_switch_flag(command, &a, &mut req)?
            || parse_harness_arg_flag(command, &a, &mut it, &mut req)?
            || parse_role_audit_flag(command, &a, &mut it, &mut req)?
            || parse_lane_flag(command, &a, &mut it, &mut req)?;
        if !taken {
            parse_tail_token(command, a, &mut it, &mut req)?;
        }
    }
    apply_request_axes(&mut req, axes.as_ref())?;
    Ok(req)
}

/// Fold the vendor tombstone, the harness and the spawn axes into params.
fn apply_request_axes(
    req: &mut RequestArgs,
    axes: Option<&SpawnAxes>,
) -> Result<(), RequestRefusal> {
    // Off spawn the vendor axis routes nothing, so --provider/-P is a
    // tombstone: exit 2 with the axis map, never silently forwarded. On spawn
    // it refuses the same way: a vendor is not a harness, and the axis-rename
    // line is the teacher scan no longer is.
    if req.vendor_val.is_some() {
        return Err(
            "--provider/-P was split at the axis rename: the CLI binary is \
             --harness/-H; a model vendor is only routable at spawn via the \
             lane template."
                .into(),
        );
    }
    let params = &mut req.params;
    if let Some(v) = req.harness_val.take() {
        params.insert("provider".into(), Value::String(v));
    }
    if let Some(axes) = axes {
        if let Some(v) = &axes.provider {
            // Spawn never reaches here with a value (scan refuses the axis
            // tokens up front); the arm stays a named refusal should the
            // strip ever regress.
            if KNOWN_PROVIDERS.contains(&v.as_str()) || v == "agy" || "opencode" == v {
                return Err(format!("{v} is a harness, not a provider; use --harness {v}").into());
            }
            return Err(format!(
                "--provider {v} names a model vendor; routing is applied by the \
                 lane template, not by fno-agent directly"
            )
            .into());
        }
        if let Some(h) = &axes.harness {
            params.insert("provider".into(), Value::String(h.clone()));
        }
        if let Some(a) = &axes.account {
            params.insert("account".into(), Value::String(a.clone()));
        }
        if let Some(r) = &axes.route {
            params.insert("route".into(), Value::String(r.clone()));
        }
        if let Some(s) = &axes.substrate {
            params.insert("substrate".into(), Value::String(s.clone()));
        }
    }
    if let Some(av) = req.argv.take() {
        params.insert(
            "argv".into(),
            Value::Array(av.into_iter().map(Value::String).collect()),
        );
    }
    Ok(())
}

/// Append tokens to the spawn's `harness_args` list, the one carrier every
/// harness-flag spelling feeds.
fn push_harness_args(params: &mut Map<String, Value>, tokens: impl IntoIterator<Item = Value>) {
    let items = params
        .entry(String::from("harness_args"))
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(list) = items {
        list.extend(tokens);
    }
}

/// The harness axes and the placement flags.
fn parse_axis_flag(
    command: &str,
    a: &str,
    it: &mut RequestTokens,
    req: &mut RequestArgs,
) -> Result<bool, RequestRefusal> {
    let RequestArgs {
        params,
        harness_val,
        vendor_val,
        ..
    } = req;
    match a {
        // --harness/-H is the CLI-binary axis, the --harness vocabulary the
        // rest of fno uses. -H has never meant headless here.
        "--harness" | "-H" => {
            *harness_val = Some(it.next().ok_or("--harness needs a value")?);
        }
        // --provider/-P is a tombstone on every command: the vendor axis only
        // ever meant something alongside a routable spawn.
        "--provider" | "-P" => {
            *vendor_val = Some(it.next().ok_or("--provider needs a value")?);
        }
        // Off `spawn`, -p was the provider short (the harness axis). That axis
        // is --harness/-H everywhere now, and -p/--headless is a spawn-only
        // one-shot, so -p is a loud tombstone here - never silently bound to
        // headless (the arm below) or to a harness. This arm must precede the
        // headless one, which also matches "-p".
        "-p" if command != "spawn" => {
            return Err(format!(
                "-p is not valid here; the one-shot short (--headless) is spawn-only, \
                     and the CLI binary is --harness/-H. \
                     (--provider/-p was split at the axis rename.)"
            )
            .into());
        }
        "--workspace" | "--squad" | "-s" => {
            params.insert("squad".into(), str_arg(it, "-s/--workspace")?);
        }
        "--split" | "-x" => {
            params.insert("split".into(), str_arg(it, "-x/--split")?);
        }
        // The portal placement trio, same spellings the mux
        // thread command uses. Parsed here so the default Rust runtime
        // accepts what the help advertises; the placement itself runs
        // after the spawn receipt (place_thread_portal_after_spawn).
        "--portal" => {
            let raw = str_arg(it, "--portal")?;
            match raw.as_str().and_then(|s| s.parse::<u8>().ok()) {
                Some(n) => {
                    params.insert("portal".into(), Value::from(n));
                }
                // `new` rides its own key, never a computed index: the
                // server picks the next free one, and a caller that
                // computed it from a stale render would repoint another
                // client's portal (the portals.md race). Same spelling
                // the mux thread door accepts.
                None if raw.as_str() == Some("new") => {
                    params.insert("portal_new".into(), Value::from(true));
                }
                None => {
                    return Err("--portal takes an index 0-255 or new".into());
                }
            }
        }
        "--tab" => {
            params.insert("tab".into(), str_arg(it, "--tab")?);
        }
        "--machine" if command == "spawn" => {
            // The cross-host pin: the placement intercept reads
            // the raw argv; parsed here so the flag is known and its
            // value never reads as the prompt.
            params.insert("machine_host".into(), str_arg(it, "--machine")?);
        }

        "--pane" if command == "spawn" => {
            // The existing-pane attach: the spine resolves, checks and
            // starts inside the named pristine shell.
            let raw = str_arg(it, "--pane")?;
            let id: u64 = raw
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| "--pane needs a positive integer pane id".to_string())?;
            params.insert("pane".into(), Value::from(id));
        }
        "--at" => {
            params.insert("at".into(), str_arg(it, "--at")?);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The flags that name the worker, its cell and its node.
fn parse_seat_flag(
    command: &str,
    a: &str,
    it: &mut RequestTokens,
    req: &mut RequestArgs,
) -> Result<bool, RequestRefusal> {
    let RequestArgs { params, .. } = req;
    match a {
        // The named cell a split halves (spawn only): `portal N`, a
        // worker name, or `current`. `current` stays literal here: the
        // placement layer resolves it from FNO_PANE when the spawn ran
        // inside a pane. Every other command's `--from` is promote's
        // resume pointer below.
        "--from" if command == "spawn" => {
            params.insert("from".into(), str_arg(it, "--from")?);
        }
        "--from" => {
            // `promote <name> --from <session-uuid>`: the session to resume
            // interactively. Forwarded as `resume_id` (the daemon infers the
            // provider from the source row).
            params.insert("resume_id".into(), str_arg(it, "--from")?);
        }
        "--cwd" | "-c" => {
            params.insert("cwd".into(), str_arg(it, "--cwd")?);
        }
        "--message" => {
            params.insert("message".into(), str_arg(it, "--message")?);
        }
        // the agent name rides a flag, so the single positional can be
        // the prompt. The seam normalizer mints one when the caller omits it,
        // so a spawn reaching here normally carries --name; the positional
        // fallback below keeps a direct `fno-agent spawn <name>` working.
        "--name" => {
            params.insert("name".into(), str_arg(it, "--name")?);
        }
        "--node" | "--slug" | "--plan" | "--session-phase" => {
            params.insert(a[2..].replace('-', "_"), str_arg(it, a)?);
        }
        // The cross-host relay's explicit parent edge (spawn_remote.rs):
        // an ssh hop forwards no caller env, so the far side's ambient
        // capture resolves nothing. The daemon mint reads the request.
        "--spawned-by" => {
            params.insert("spawned_by_session".into(), str_arg(it, "--spawned-by")?);
            // The explicit edge is a relay leg; ambient provenance never
            // sets this, so the placement guard tells the two apart.
            params.insert("spawned_by_explicit".into(), Value::Bool(true));
        }
        // The seam's node-reason receipt, set only when the seed
        // named a node the seam could not resolve. Forwarded so the mint
        // stamps why the row works no node; never read as a decision.
        "--node-reason" => {
            params.insert("node_reason".into(), str_arg(it, "--node-reason")?);
        }
        "--session-id" => {
            params.insert("session_id".into(), str_arg(it, "--session-id")?);
        }
        "--mode" => {
            // Disambiguates claude's two interactive-host lanes: `interactive`
            // (PTY pane, subscription-billed) vs the default stream-json adopt.
            // The daemon reads `mode`; codex/gemini ignore it. (`drive --mode`
            // is a different parser and never reaches build_request.)
            params.insert("mode".into(), str_arg(it, "--mode")?);
        }
        "--status" => {
            params.insert("status".into(), str_arg(it, "--status")?);
        }
        "--progress" => {
            params.insert("progress".into(), str_arg(it, "--progress")?);
        }
        "--max" => {
            // The provider lane cap override for this one call: the gate
            // replaces the configured number, the count stays account-wide.
            let raw = str_arg(it, "--max")?
                .as_str()
                .ok_or_else(|| "error: --max needs a string value".to_string())?
                .to_string();
            let parsed: usize = raw
                .parse()
                .map_err(|_| "error: --max needs a whole number".to_string())?;
            params.insert(
                "max_provider".into(),
                Value::from(u64::try_from(parsed).unwrap_or(u64::MAX)),
            );
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The boolean switches and the retired no-op flags.
fn parse_switch_flag(
    command: &str,
    a: &str,
    req: &mut RequestArgs,
) -> Result<bool, RequestRefusal> {
    let RequestArgs { params, .. } = req;
    match a {
        "--json" | "-J" => {
            // Client-side rendering flag; never forwarded to the daemon.
            // On spawn the gate refusal's receipt prints only with it.
            if command == "spawn" {
                params.insert("json_out".into(), Value::Bool(true));
            }
        }
        "--verbose" => {}
        "--all" | "-A" => {
            params.insert("all".into(), Value::Bool(true));
        }
        "--discovered" | "--no-discovered" => {
            // client-side rendering flags for the `list`
            // discovered-live-sessions lane. Recognized here so they are not
            // rejected as unknown; captured separately at the call site and
            // never forwarded to the daemon.
        }
        "--force" | "-F" => {
            params.insert("force".into(), Value::Bool(true));
        }
        "--dry-run" if command == "spawn" => {
            // Plan the spawn and print it; launch, register and settle nothing.
            params.insert("dry_run".into(), Value::Bool(true));
        }
        "--no-wait" => {
            // Spawn-gate escape: fail immediately at max_live
            // instead of queueing for a free slot. Client-side only.
            params.insert("no_wait".into(), Value::Bool(true));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The flags that ride the harness argv, and the sender and answer.
fn parse_harness_arg_flag(
    command: &str,
    a: &str,
    it: &mut RequestTokens,
    req: &mut RequestArgs,
) -> Result<bool, RequestRefusal> {
    let RequestArgs { params, .. } = req;
    match a {
        // The harness's own flags, typed before the fence, are shorthand
        // for the same flag behind it. They join the fenced tail in
        // harness_args, so every lane that carries a fenced flag carries
        // these, and a lane that cannot refuses by name.
        "--model" | "-m" | "--effort" | "--permission-mode" | "--add-dir" | "--agent"
        | "--tools"
            if command == "spawn" =>
        {
            let flag = if a == "-m" { "--model" } else { a };
            let value = str_arg(it, flag)?;
            push_harness_args(params, [Value::String(flag.to_string()), value]);
        }
        "--from-name" => {
            // Forwarded for Python flag-parity; every ask path validates it.
            params.insert("from_name".into(), str_arg(it, "--from-name")?);
        }
        // A footnote thread's pending permission ask: allow_once | deny.
        "--answer" => {
            params.insert("answer".into(), str_arg(it, "--answer")?);
        }
        // The one dangling retired short: -p is tombstoned above and -P
        // rides the vendor tombstone, so -r would otherwise fall to the
        // positionals and launch under a nonsense seed. The LONG form
        // forwards; the short names no spawn flag here. -Y is the same
        // shape off spawn (its long --yolo is refused by the catch-all
        // there); the shorts never read as prompt words.
        "-r" if command == "spawn" => {
            return Err(
                "-r is not a spawn flag here; pass --resume <id> to forward it to the harness argv"
                    .into(),
            );
        }
        "-Y" if command != "spawn" => {
            return Err(format!("unknown flag: {a}").into());
        }
        "--yolo" | "-Y" if command == "spawn" => {
            // The never-prompt flag is the harness's own vocabulary: it
            // rides verbatim, and a lane that cannot carry it refuses by
            // name (the thread seat, the pane spine, the daemon-side
            // vocabulary). Off spawn the catch-all keeps the refusal.
            push_harness_args(params, [Value::String("--yolo".to_string())]);
        }
        "--deny-tools" if command == "spawn" => {
            // fno's own spelling of claude's --disallowedTools forwards
            // with its value; the lane that lacks it refuses by name.
            let v = str_arg(it, "--deny-tools")?;
            push_harness_args(params, [Value::String("--deny-tools".to_string()), v]);
        }
        // The front door's carrier for a fenced `--` token a thread or
        // one-shot lane maps (codex: -c/--config/--add-dir). Repeatable;
        // the daemon-side harness_args parser owns the per-harness
        // vocabulary and refuses an unmapped token by name.
        "--harness-arg" => {
            let v = str_arg(it, "--harness-arg")?;
            push_harness_args(params, [v]);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The role, account and audit flags.
fn parse_role_audit_flag(
    command: &str,
    a: &str,
    it: &mut RequestTokens,
    req: &mut RequestArgs,
) -> Result<bool, RequestRefusal> {
    let RequestArgs { params, .. } = req;
    match a {
        // The role halves the Python seam carries for a promoted codex
        // thread spawn; the typed parse lives in spawn_axes.
        f if fno_agent::spawn_axes::ROLE_FLAGS.contains(&f) => {
            fno_agent::spawn_axes::insert_role_flag(a, it, params)?;
        }
        // The row-9 flags: typed parse in spawn_axes (the equals form
        // split above); the value gates run after the loop where harness
        // and substrate are both known.
        "--output-format" | "--monitor" | "--dispatch-account" if command == "spawn" => {
            fno_agent::spawn_axes::insert_spawn_flag(a, it, params)?;
        }
        "--role" if command == "spawn" => {
            // Resolved by spawn_route::role_seam in the spawn arm.
            params.insert("role".into(), str_arg(it, "--role")?);
        }
        "--account" => {
            // Resolved by accounts::launch in the spawn arm.
            params.insert("account".into(), str_arg(it, "--account")?);
        }
        "--audit-actor" => {
            params.insert("audit_actor".into(), str_arg(it, "--audit-actor")?);
        }
        "--audit-reason" => {
            params.insert("audit_reason".into(), str_arg(it, "--audit-reason")?);
        }
        "--audit-request-id" => {
            params.insert(
                "audit_request_id".into(),
                str_arg(it, "--audit-request-id")?,
            );
        }
        "--audit-reclaimed-bytes" => {
            let value = str_arg(it, "--audit-reclaimed-bytes")?;
            let bytes = value
                .as_str()
                .and_then(|raw| raw.parse::<u64>().ok())
                .ok_or("--audit-reclaimed-bytes needs a non-negative integer")?;
            params.insert("audit_reclaimed_bytes".into(), Value::from(bytes));
        }
        "--audit-worktree-touched" => {
            params.insert("audit_worktree_touched".into(), Value::Bool(true));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The lane, cwd and timeout flags.
fn parse_lane_flag(
    command: &str,
    a: &str,
    it: &mut RequestTokens,
    req: &mut RequestArgs,
) -> Result<bool, RequestRefusal> {
    let RequestArgs { params, .. } = req;
    match a {
        "--substrate" => {
            // The session-substrate selector: pane (owned-PTY,
            // default) | bg (claude --bg detached thread; opencode
            // serve-hosted session) |
            // headless (claude -p / codex --exec / agy -p / grok -p / opencode run). The
            // sole routing key the spawn arm reads (replaces --once).
            let v = str_arg(it, "--substrate")?;
            match v.as_str() {
                Some("pane") | Some("thread") | Some("headless") => {
                    params.insert("substrate".into(), v);
                }
                Some("bg") => {
                    return Err("substrate 'bg' was retired; a bare spawn seats a thread".into());
                }
                other => {
                    return Err(format!(
                        "--substrate must be one of: pane, thread, headless (got {})",
                        other.unwrap_or("")
                    )
                    .into());
                }
            }
        }
        "--once" | "-o" => {
            // A one-shot: spawn_bare::settle_substrate makes it headless.
            params.insert("once".into(), Value::Bool(true));
        }
        "--fresh" => {
            // Accepted no-op alias: the worker cwd already defaults
            // to the canonical repo root. Parsed for dispatcher compat.
            params.insert("fresh".into(), Value::Bool(true));
        }
        "--bounded-placement" => {
            // The dispatchers' automated pane spell (fallback_chain /
            // spawn_overlay emit it beside `--as pane`): the pane arm
            // reads it as hold-the-placement-lease-and-`--fit`. Parsed
            // on every command like the other placement flags; only the
            // pane arm consumes it.
            params.insert("bounded_placement".into(), Value::Bool(true));
        }
        "--here" | "--in-place" => {
            // Explicit opt-in to the caller's cwd instead of the canonical
            // default: extend WIP right here.
            params.insert("here".into(), Value::Bool(true));
        }
        "--to-project" | "--any" if command == "ask" => {
            fno_agent::ask_project::take_flag(a, it, params)?;
        }
        "--timeout" | "-t" => {
            let val = str_arg(it, "--timeout")?;
            let secs: u64 = val
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| "--timeout needs a numeric value")?;
            params.insert("timeout".into(), Value::Number(secs.into()));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The tail of a request line: the `--argv` payload, the `--` fence, the
/// spawn-owned flags, the passthrough and the positionals.
fn parse_tail_token(
    command: &str,
    a: String,
    it: &mut RequestTokens,
    req: &mut RequestArgs,
) -> Result<(), RequestRefusal> {
    let RequestArgs {
        params,
        positional,
        argv,
        fence_at,
        ..
    } = req;
    match a.as_str() {
        "--argv" => {
            // Everything after --argv is the provider command line. The
            // documented syntax is `--argv -- <cmd...>`; strip a single
            // leading `--` separator so the worker does not try to exec
            // the literal "--" as argv[0] (Codex P1).
            let mut rest: Vec<String> = it.by_ref().collect();
            if rest.first().map(|s| s == "--").unwrap_or(false) {
                rest.remove(0);
            }
            *argv = Some(rest);
        }
        "--" => {
            // End-of-options: everything after is positional (the seed
            // fence, same contract as the Python CLI's click parser).
            *fence_at = Some(positional.len());
            for a in it.by_ref() {
                positional.push(a);
            }
        }
        // The resume arm re-parses the original `rest` with
        // parse_conversion_args, which owns both flags; swallow them here
        // so the catch-all does not refuse them first.
        "--dry-run" | "--allow-new-id" if command == "resume" => {}
        f if command == "spawn" && fno_agent::spawn_bare::owns(f) => {
            fno_agent::spawn_bare::take_flag(&a, it, params)?;
        }
        other if other.starts_with("--") && command == "spawn" => {
            // The passthrough law, boundary edition (amends law
            // d-00122536): a flag spawn does not own rides the harness
            // argv verbatim. Before the prompt it passes only in
            // --flag=value form: the space form cannot be parsed without
            // a foreign arity table - the incident class the boundary
            // retires - so it refuses and points at the legal forms.
            if !other.contains('=') {
                return Err(format!(
                    "{other} in space form is never parsed before the prompt; \
                         pass {other}=<value>, or move the flag after the prompt \
                         or into the --argv payload (everything after the prompt \
                         is harness argv)"
                )
                .into());
            }
            push_harness_args(params, [Value::String(other.to_string())]);
        }
        other if other.starts_with("--") => {
            return Err(format!("unknown flag: {other}").into());
        }
        _ => positional.push(a),
    }
    Ok(())
}

/// The spawn arm's own step: the parent edge, the name, the seed message or
/// passthrough tail, the lane and the lineage stamp.
fn finish_spawn_request(
    params: &mut Map<String, Value>,
    mut positional: Vec<String>,
    fence_at: Option<usize>,
) -> Result<(), RequestRefusal> {
    // The client runs as a child of the spawning session, so the seam
    // holds the harness markers the daemon's scrubbed env lost. Stamp
    // the ambient parent edge onto the request so a
    // daemon mint reads the parent from HERE, never from its own
    // environment (node-provenance.md: capture is ambient).
    let (session, harness, cwd) = fno_agent::claims::ambient_parent_edge();
    // An explicit `--spawned-by` (the cross-host relay's edge) wins
    // over the ambient capture; the three ride together, so a
    // carried session keeps its harness/cwd out too rather than
    // mixing parentage from two captures.
    if params.get("spawned_by_session").is_none() {
        if let Some(s) = session {
            params.insert("spawned_by_session".into(), Value::String(s));
        }
        if let Some(h) = harness {
            params.insert("spawned_by_harness".into(), Value::String(h));
        }
        if let Some(c) = cwd {
            params.insert("spawned_by_cwd".into(), Value::String(c));
        }
    }
    // With --name the whole positional tail is the message; without it the
    // first positional is still the name (a direct `fno-agent spawn`
    // bypasses the seam normalizer that would have minted one). A bare
    // hand-off names nobody: the seam skips the mint on purpose (a minted
    // slug would arrive as an explicit proposal and decline the documented
    // no-name draw), and the settle draw fills the name at plan, so the
    // parse must not demand one here.
    let msg_from = if params.contains_key("name") {
        0
    } else if params.get("hand_off").and_then(Value::as_bool) == Some(true)
        && params
            .get("promote")
            .and_then(Value::as_array)
            .is_some_and(|scopes| !scopes.is_empty())
    {
        0
    } else {
        let name = positional.first().ok_or("spawn needs a <name> or --name")?;
        params.insert("name".into(), Value::String(name.clone()));
        1
    };
    // A message already collected before the fence makes the fenced
    // tail provider passthrough (the same harness_args list
    // --harness-arg fills; the daemon-side vocabulary check stays the
    // trust boundary). Without a message before the fence the tail is
    // still the seed (the fenced `--timeout=5 do X` case) - the
    // deprecated shape, warned here and refused in the next release.
    if let Some(pre_len) = fence_at {
        if pre_len > msg_from {
            let tail = positional.split_off(pre_len);
            push_harness_args(params, tail.into_iter().map(Value::String));
        } else if positional.len() > msg_from {
            let seed: Vec<String> = positional.iter().skip(msg_from).cloned().collect();
            eprintln!("{}", fno_agent::spawn_bare::seed_fence_warning(&seed));
        }
    }
    if !params.contains_key("message") && positional.len() > msg_from {
        params.insert(
            "message".into(),
            Value::String(positional[msg_from..].join(" ")),
        );
    }
    // The lane resolves once, here: with the --substrate flag retired,
    // an unflagged spawn takes default_substrate, and the value rides
    // params so dispatch, the thread-only grant and the portal all read
    // the same lane. Only `pane` gets the interactive host_mode/mint;
    // `thread` and `headless` never touch the daemon's PTY host. An
    // unknown provider keeps today's behavior (the daemon's
    // provider_for_pty errors as before).
    fno_agent::spawn_bare::settle_substrate(params);
    let substrate = params
        .get("substrate")
        .and_then(Value::as_str)
        .unwrap_or("pane");
    let pty_capable = params
        .get("provider")
        .and_then(Value::as_str)
        .is_some_and(|p| KNOWN_PROVIDERS.contains(&p));
    if substrate == "pane" && pty_capable {
        apply_interactive_defaults(params)?;
    }
    fno_agent::spawn_context::stamp_spawn_lineage(params)?;
    Ok(())
}

/// Stamp the caller's working directory into daemon-bound spawn/ask requests.
///
/// The `fno-agent` daemon is a single long-lived process shared across every
/// project, so its own `std::env::current_dir()` is frozen to wherever it was
/// first lazy-started; it cannot stand in for "the directory the user ran the
/// command from". Only the client sits in the user's directory, so the client
/// must forward `cwd`; otherwise a worker spawned from project A lands in the
/// daemon's home project B (e.g. `fno agent host` opening codex in the wrong
/// repo). An explicit `--cwd` already in `params` always wins.
///
/// `agent.spawn` covers `spawn`/`host`/`promote`; `agent.ask` covers gemini's
/// first-contact auto-spawn (claude/codex `ask` resolve cwd client-side before
/// reaching this send path, so they never depend on it).
fn ensure_request_cwd(method: &str, params: &mut Value, cwd: &std::path::Path) {
    if method != "agent.spawn" && method != "agent.ask" {
        return;
    }
    // build_request always returns Value::Object for these methods; assert it
    // so a future caller passing a non-object is caught in debug rather than
    // silently skipping the cwd stamp.
    debug_assert!(params.is_object(), "spawn/ask params must be a JSON object");
    if let Some(obj) = params.as_object_mut() {
        if !obj.contains_key("cwd") {
            obj.insert(
                "cwd".to_string(),
                Value::String(cwd.to_string_lossy().into_owned()),
            );
        }
    }
}

fn str_arg(
    it: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<Value, String> {
    it.next()
        .map(Value::String)
        .ok_or_else(|| format!("{flag} needs a value"))
}

/// The stderr line a transport-level `rm` failure prints. The pre-exec removal
/// banner is past tense and the transport error alone reads like a completed
/// removal, so the line names the row and answers from the registry: a dead
/// connection does not prove the daemon skipped the write, and the store is
/// the receipt. `None` means the store itself could not be read, and the line
/// says so rather than wearing an absent verdict.
fn rm_failure_line(name: &str, err: &str, still_registered: Option<bool>) -> String {
    let verdict = match still_registered {
        Some(true) => "nothing was removed - the row is still registered",
        Some(false) => "the row is no longer registered",
        None => "the registry could not be read to confirm whether anything was removed",
    };
    format!("fno-agent: rm {name}: {err}; {verdict}")
}

/// Format a successful daemon response for human-readable stdout.
///
/// Returns `Some(line)` for commands with a defined output contract, `None` for
/// commands that still use the generic `serde_json::to_string_pretty` fallback.
///
/// - `stop`: prints `stopped: <name> (<short_id>)` using the `short_id` the
///   daemon now includes in every stop success payload. Falls back to
///   `stopped: <name>` when `short_id` is absent (e.g. an old daemon).
/// - `rm`: names each surface the daemon proved removed or unverified.
/// - `list`: Task 3.1 - JSON when `json_flag` or not a TTY; table otherwise.
/// - `reconcile`: Task 3.1 - JSON when `json_flag` or not a TTY; human summary otherwise.
fn format_success(
    command: &str,
    name: &str,
    result: &Value,
    json_flag: bool,
    is_tty: bool,
    discover: bool,
) -> Option<String> {
    match command {
        "ask" => ask_line(result),
        "stop" => stop_line(name, result),
        "rm" => fno_agent::rm_receipt::receipt(name, result),
        "rename" => fno_agent::rename::receipt(name, result),
        "list" => list_line(result, json_flag, is_tty, discover),
        "reconcile" => {
            if json_flag || !is_tty {
                Some(render_reconcile_json(result))
            } else {
                Some(render_reconcile_human(result))
            }
        }
        "spawn" => Some(spawn_receipt_line(name, result)),
        _ => None,
    }
}

/// The `ask` line: a created agent's short id, an in-flight turn's receipt,
/// or the reply verbatim.
fn ask_line(result: &Value) -> Option<String> {
    // Create path (first contact): daemon returns {created: true, short_id: "..."}.
    // Python prints exactly `<short_id>\n` (no banner).
    if result
        .get("created")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let short_id = result
            .get("short_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Some(short_id.to_string())
    } else {
        // Follow-up path: print the reply verbatim (no added newline; println!
        // in the caller adds the newline, matching Python's behaviour).
        // A `reply: null` with `status: "in_flight"` is the codex
        // thread actor's bounded-ask receipt: the turn is still
        // driving, so say THAT instead of printing an empty line that
        // reads as an empty answer.
        if result.get("reply").is_none_or(Value::is_null)
            && result.get("status").and_then(|v| v.as_str()) == Some("in_flight")
        {
            let turn_id = result
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            return Some(format!(
                "in flight: turn {turn_id} is still driving; the reply surfaces via \
                         agent_ask_done"
            ));
        }
        let reply = result.get("reply").and_then(|v| v.as_str()).unwrap_or("");
        Some(reply.to_string())
    }
}

/// The `stop` line: the refusal, or `stopped:` with the turn outcome and the
/// receipt suffix.
fn stop_line(name: &str, result: &Value) -> Option<String> {
    let outcome = result
        .get("interrupt")
        .and_then(|v| v.as_str())
        .filter(|outcome| *outcome != "no-turn");
    // The daemon REFUSED the stop: its interrupt never settled and the
    // turn is still driving in the worker's worktree. Saying "stopped"
    // here is the report-it-did-not-perform shape the daemon arm exists
    // to prevent, so the word never appears on this path.
    if result.get("stopped").and_then(Value::as_bool) == Some(false) {
        return Some(format!(
            "stop refused: {name} is still running ({})",
            outcome.unwrap_or("the interrupt never settled")
        ));
    }
    let mut line = match result.get("short_id").and_then(|v| v.as_str()) {
        Some(short_id) => format!("stopped: {name} ({short_id})"),
        None => format!("stopped: {name}"),
    };
    // A codex thread stop names what happened to the in-flight turn:
    // a bare "stopped" over an interrupt the daemon never confirmed is
    // the exact report-it-did-not-perform shape this field exists to
    // prevent. `no-turn` (nothing was driving) stays silent.
    if let Some(outcome) = outcome {
        line.push_str(&format!(" (turn {outcome})"));
    }
    line.push_str(&fno_agent::rm_receipt::stop_receipt_suffix(result));
    Some(line)
}

/// The `list` render: JSON when asked or off a TTY, the table otherwise, with
/// the discovered-session lane folded in when `discover` is set.
fn list_line(result: &Value, json_flag: bool, is_tty: bool, discover: bool) -> Option<String> {
    let agents = &result["agents"];
    let fields_omitted = &result["fields_omitted"];
    let filters = result.get("filters_applied").cloned().unwrap_or_else(
        || json!({"cwd": null, "provider": null, "status": null, "progress": null}),
    );
    // merge the P1 host-local live-session lane. The Rust
    // client owns the rendered surface, so it shells out to the Python
    // helper (which has psutil's cross-platform reuse-safe liveness) and
    // folds the result in. Fail-open: an empty lane on any error.
    let discovered = if discover {
        fno_agent::list_render::fetch_discovered_sessions(
            filters.get("cwd").and_then(|v| v.as_str()),
            filters.get("provider").and_then(|v| v.as_str()),
            filters.get("status").and_then(|v| v.as_str()),
            filters.get("progress").and_then(|v| v.as_str()),
        )
    } else {
        Vec::new()
    };
    if json_flag || !is_tty {
        Some(render_list_json(
            agents,
            &filters,
            fields_omitted,
            &discovered,
            result["truth_probe_asked"].as_u64(),
            result["truth_probe_answered"].as_u64(),
            result.get("codex_loaded"),
            result.get("retired_sessions").unwrap_or(&Value::Null),
        ))
    } else {
        Some(fno_agent::list_render::render_list_table(
            agents,
            &discovered,
            result["truth_probe_asked"].as_u64(),
            result["truth_probe_answered"].as_u64(),
            result.get("retired_sessions").unwrap_or(&Value::Null),
        ))
    }
}

/// The `spawn` receipt line for a daemon-hosted pane spawn.
fn spawn_receipt_line(name: &str, result: &Value) -> String {
    // PTY-provider spawns now route through the daemon (owned
    // interactive pane) instead of the client-side claude `--bg` lane.
    // Emit the SAME compact single-line JSON receipt that lane produces
    // ({"name","short_id","harness","status"}). The harness axis is
    // reported under `harness`, never under a `provider` key (a provider
    // key holding a harness literal is the axis defect). The in-repo
    // receipt parsers (skills/target/scripts/dispatch-node.sh and
    // backlog/advance.py) read only `short_id`, so the rename is safe.
    // serde_json::to_string (NOT _pretty) keeps it one line for the
    // line-by-line `json.loads` consumers. `--once` spawns are handled
    // client-side and never reach here.
    let short_id = result
        .get("short_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let harness = result.get("harness").and_then(|v| v.as_str()).unwrap_or("");
    let status = result
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("live");
    let session_id = result
        .get("harness_session_id")
        .or_else(|| result.get("session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let mut receipt = json!({
        "name": name,
        "short_id": short_id,
        "harness": harness,
        "status": status,
    });
    if let Some(session_id) = session_id {
        receipt["session_id"] = json!(session_id);
        if result.get("harness_session_id").is_some() {
            receipt["harness_session_id"] = json!(session_id);
        }
    }
    serde_json::to_string(&receipt).unwrap_or_default()
}

/// The stderr twin of the spawn receipt: the same facts in plain words on
/// the channel output filters pass through, so a filter that drops the
/// JSON stdout line cannot drop the spawn itself.
fn spawn_receipt_twin(name: &str, result: &Value) -> String {
    let short_id = result
        .pointer("/short_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let harness = result
        .pointer("/harness")
        .and_then(Value::as_str)
        .unwrap_or("");
    let status = result
        .pointer("/status")
        .and_then(Value::as_str)
        .unwrap_or("live");
    let session = (!short_id.is_empty())
        .then(|| format!(", session {short_id}"))
        .unwrap_or_default();
    let reach = if status == "failed" {
        format!("logs: fno agent session logs {name}")
    } else {
        format!("fno agent attach {name} to steer")
    };
    format!("spawn receipt: {name} {status} ({harness}{session}) - {reach}")
}

/// Render agents list as Python-matching JSON (Task 3.1; discovered lane
///; provider key restored).
///
/// Shape (schema_version 8): `{"agents": [...], "count": N,
/// "liveness_unknown": K, "discovered_sessions": [...], "discovered_count":
/// M, "fields_omitted": [...], "filters_applied": {...}, "schema_version":
/// 8}`. Stays byte-shape-aligned with Python's `format.render_json`.
/// `liveness_unknown` (v8, additive) counts rows whose served liveness
/// reads `unknown` - a stale or never-run probe: a zero alive
/// count beside a nonzero unknown count says the PROBE went dark, not the
/// fleet.
const LIST_JSON_SCHEMA_VERSION: u32 = 8;

fn render_list_json(
    agents: &Value,
    filters_applied: &Value,
    fields_omitted: &Value,
    discovered: &[Value],
    truth_probe_asked: Option<u64>,
    truth_probe_answered: Option<u64>,
    codex_loaded: Option<&Value>,
    retired: &Value,
) -> String {
    let rows = agents.as_array();
    let count = rows.map_or(0, |a| a.len());
    let liveness_unknown = rows.map_or(0, |a| {
        a.iter()
            .filter(|r| r.get("liveness").and_then(Value::as_str) == Some("unknown"))
            .count()
    });
    // `codex_loaded` is additive and present only when the caller probed the
    // codex daemon (`--harness codex`); the key is omitted otherwise.
    let mut payload = json!({
        "agents": agents,
        "count": count,
        "liveness_unknown": liveness_unknown,
        "discovered_sessions": discovered,
        "discovered_count": discovered.len(),
        "fields_omitted": fields_omitted,
        "filters_applied": filters_applied,
        "truth_probe_asked": truth_probe_asked,
        "truth_probe_answered": truth_probe_answered,
        "schema_version": LIST_JSON_SCHEMA_VERSION,
    });
    if let Some(block) = codex_loaded {
        payload["codex_loaded"] = block.clone();
    }
    fno_agent::reap_render::attach_retired(&mut payload, retired);
    serde_json::to_string_pretty(&payload).unwrap_or_default()
}

/// Render reconcile result as Python-matching JSON (Task 3.1).
///
/// Shape: `{"scanned": N, "orphaned": [...], "recovered": [...], "skipped": [...], "errors": [...]}`
/// Matches Python cmd_reconcile's JSON payload exactly.
fn render_reconcile_json(result: &Value) -> String {
    // The daemon now returns scanned/orphaned/recovered/skipped/errors directly.
    let payload = json!({
        "scanned": result.get("scanned").cloned().unwrap_or(Value::Null),
        "orphaned": result.get("orphaned").cloned().unwrap_or_else(|| json!([])),
        "recovered": result.get("recovered").cloned().unwrap_or_else(|| json!([])),
        "skipped": result.get("skipped").cloned().unwrap_or_else(|| json!([])),
        "errors": result.get("errors").cloned().unwrap_or_else(|| json!([])),
    });
    serde_json::to_string(&payload).unwrap_or_default() + "\n"
}

/// Render reconcile result as human-readable summary (Task 3.1).
fn render_reconcile_human(result: &Value) -> String {
    let scanned = result["scanned"].as_u64().unwrap_or(0);
    let orphaned = result["orphaned"].as_array().map(|a| a.len()).unwrap_or(0);
    let recovered = result["recovered"].as_array().map(|a| a.len()).unwrap_or(0);
    let skipped = result["skipped"].as_array().map(|a| a.len()).unwrap_or(0);
    let errors = result["errors"].as_array().map(|a| a.len()).unwrap_or(0);
    format!(
        "scanned: {scanned}  orphaned: {orphaned}  recovered: {recovered}  skipped: {skipped}  errors: {errors}\n"
    )
}

/// Map a daemon error code to the design's command exit codes.
fn exit_code_for(code: ErrorCode) -> i32 {
    match code {
        ErrorCode::AgentNotFound | ErrorCode::AgentExists | ErrorCode::InvalidStatus => 13,
        ErrorCode::SpawnFailed => 14,
        ErrorCode::LockTimeout => 15,
        ErrorCode::Busy => 18,
        ErrorCode::InvalidParams | ErrorCode::MalformedFrame | ErrorCode::UnknownMethod => 2,
        ErrorCode::ChannelUnknown => 13,
        ErrorCode::Internal => 1,
        // Distinct from Internal on purpose: a caller racing daemon teardown
        // can retry, where a real Internal fault should not be retried blind.
        ErrorCode::ShuttingDown => 19,
        // Also distinct, and for the opposite reason: two fno builds disagree
        // about the registry schema, so retrying changes nothing. The repair is
        // a deploy or a redirect. A caller that folded this into Internal would
        // report a daemon fault, which is how the 2026-08-28 outage was
        // misdiagnosed twice.
        ErrorCode::SchemaMismatch => 20,
    }
}

fn warns_on_daemon_drift(command: &str) -> bool {
    matches!(command, "list" | "rm")
}

/// The `status --<leaf>` arguments whose leaf parses its own `--help`.
const STATUS_LEAVES_WITH_HELP: &[&str] = &[
    "--register",
    "--name",
    "--whoami",
    "--registry-repair",
    "--heal-token",
    "--pane-identity",
    "--watch",
    "--truth",
    "--peek",
    "--team",
    "--role",
];

/// True when `--help`/`-h` appears in the command's OWN options, i.e. before an
/// `--argv`/`--` payload boundary. A `--help` after that boundary belongs to a
/// spawned command's argv (e.g. `spawn wk --harness codex --argv -- tool
/// --help`) and must not be captured as our per-command help request
/// (review: gemini HIGH / codex P2).
fn is_help_request(opts: &[String]) -> bool {
    let boundary = opts
        .iter()
        .position(|a| a == "--" || a == "--argv")
        .unwrap_or(opts.len());
    opts[..boundary].iter().any(|a| a == "--help" || a == "-h")
}

fn print_help() {
    println!(
        "{}",
        json!({
            "binary": "fno-agent",
            "verbs": CLIENT_COMMAND_USAGE,
        })
    );
}
