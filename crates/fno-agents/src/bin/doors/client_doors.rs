//! The binary-direct doors of `bin/client.rs`'s `run`, split out by the
//! question each group answers: which daemon-free command answers this argv.
//! A door returns `Some(exit)` when it owns the command and `None` to let the
//! next group look. The groups run in the order `run` once held them inline,
//! so a command two doors could answer still lands on the first.
//!
//! `fno doctor lint menu-caps` and the agents-surface lint read this file's
//! `if command ==` and `matches!(command, ...)` spellings as dispatched
//! commands, the same way they read `run` itself.

use clap::Parser as _;
use fno_agents::cli_args::{refusal_line, RestartArgs};
use fno_agents::paths::AgentsHome;

/// The doors that answer before per-command `--help` handling: each parses
/// its own argv, `--help` included.
pub(super) async fn before_help(command: &str, args: &[String]) -> Option<i32> {
    if let Some(code) = mail_and_relay_doors(command, args).await {
        return Some(code);
    }
    machine_leaf_doors(command, args).await
}

/// The doors that answer after per-command `--help` handling and before the
/// daemon request is built.
pub(super) async fn after_help(command: &str, args: &[String]) -> Option<i32> {
    if let Some(code) = loop_and_record_doors(command, args).await {
        return Some(code);
    }
    if let Some(code) = target_and_merge_doors(command, args) {
        return Some(code);
    }
    if let Some(code) = spawn_policy_and_lead_doors(command, args) {
        return Some(code);
    }
    if let Some(code) = fleet_read_doors(command, args) {
        return Some(code);
    }
    if let Some(code) = registry_doors(command, args).await {
        return Some(code);
    }
    if let Some(code) = pr_and_event_doors(command, args).await {
        return Some(code);
    }
    // `status` reports on a *running* daemon: it must NOT lazy-start one just to
    // describe it as up. A down daemon is exit 13 (AC10-ERR).
    if command == "status" {
        return Some(status_door(args.get(1..).unwrap_or(&[])).await);
    }
    daemon_lifecycle_doors(command, args).await
}

/// The `status` read. `--json`/`-J` selects the machine payload; the default
/// renders the human arms table + daemon lines. Anything else is rejected
/// rather than silently ignored.
async fn status_door(rest: &[String]) -> i32 {
    if let Some(code) = status_leaf(rest) {
        return code;
    }
    if let Some(code) = status_flag_read(rest) {
        return code;
    }
    let json_out = rest.iter().any(|a| a == "--json" || a == "-J");
    let extras: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json" && *a != "-J")
        .collect();
    if !extras.is_empty() {
        eprintln!(
            "fno-agents: status takes no arguments other than --json/-J (got: {})",
            extras.join(" ")
        );
        return 2;
    }
    super::run_status(json_out).await
}

/// Which leaf the first `status` argument names. d-fe66560a makes the action
/// list shrink-only, so the /fno-me join (`fno agents register ...` arrives as
/// `status --register ...`), the name, whoami, registry-repair and heal-token
/// leaves ride `status` as arguments, daemon-free like registry-json.
fn status_leaf(rest: &[String]) -> Option<i32> {
    let tail = rest.get(1..).unwrap_or(&[]);
    let code = match rest.first().map(String::as_str)? {
        "--register" => fno_agents::register::run_register(tail, &AgentsHome::from_env()),
        "--name" => fno_agents::naming::run_agents_name(tail),
        "--whoami" => fno_agents::registry_commands::run_whoami(tail, &AgentsHome::from_env()),
        "--registry-repair" => fno_agents::registry_commands::run_registry_repair(tail),
        "--heal-token" => fno_agents::store_heal::run_heal_token(tail),
        "--pane-identity" => fno_agents::pane_identity::run_pane_identity(tail),
        "--watch" => fno_agents::watch::run_watch(tail),
        "--truth" => fno_agents::truth_cli::run_truth(tail),
        "--peek" => fno_agents::peek::run_peek(tail),
        "--team" => fno_agents::team_read::run_team(tail),
        "--role" => fno_agents::role_grant::run_role(tail),
        op @ ("--lint-registry" | "--lint-field-coverage") => {
            fno_agents::lint_registry::run(op, tail, &AgentsHome::from_env())
        }
        _ => return None,
    };
    Some(code)
}

/// Which read a flag anywhere in the `status` argv selects. The Python router
/// rewrites `fno agents capabilities <h> ...` into
/// `fno-agents status --capabilities <h> ...`; the notify port's two ops ride
/// the same way (`fno doctor event fanout tick` execs `status --fanout`, and
/// scripts/lib/wake-signals.sh execs `status --wake-signals`). With no flag
/// the behavior is exactly the plain status read.
fn status_flag_read(rest: &[String]) -> Option<i32> {
    let has = |flag: &str| rest.iter().any(|a| a.as_str() == flag);
    let has_cap = has("--capabilities");
    let has_family = has("--target-family");
    if has_cap && has_family {
        eprintln!("fno-agents: status takes one of --capabilities or --target-family, not both");
        return Some(2);
    }
    if has_cap {
        let sub = without_flag(rest, "--capabilities");
        return Some(fno_agents::capability_leaves::run_capabilities(&sub));
    }
    if has("--liveness") {
        let sub = without_flag(rest, "--liveness");
        return Some(fno_agents::liveness_cmd::run(&sub, &AgentsHome::from_env()));
    }
    if has("--fanout") {
        let sub = without_flag(rest, "--fanout");
        return Some(fno_agents::status_fanout::run_tick_command(&sub));
    }
    if has("--wake-signals") {
        let sub = without_flag(rest, "--wake-signals");
        return Some(fno_agents::wake_read::run_wake_signals(&sub));
    }
    if has_family {
        let sub = without_flag(rest, "--target-family");
        return Some(fno_agents::capability_leaves::run_target_family(&sub));
    }
    None
}

/// The `status` argv with one selector flag removed, for the leaf it selects.
fn without_flag(rest: &[String], flag: &str) -> Vec<String> {
    rest.iter()
        .filter(|a| a.as_str() != flag)
        .cloned()
        .collect()
}

/// Which mail, relay or naming door answers this command, daemon-free.
async fn mail_and_relay_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `mail-inject`: the one-shot live-delivery command `fno agents mail send` calls
    // to inject a turn into a live `claude --bg` session. Binary-direct, not
    // routable (the `matches!` shape hides it from the parity guard); it connects
    // to an existing daemon and never lazy-starts one.
    if matches!(command, "mail-inject") {
        // The control drain rides this action as a mode flag: the PreToolUse
        // hook calls it binary-direct at every tool boundary, so a frozen
        // worker's freeze mail lands even when the daemon is the thing wedged.
        if rest.iter().any(|a| a == "--control-drain") {
            let rest: Vec<String> = rest
                .iter()
                .filter(|a| a.as_str() != "--control-drain")
                .cloned()
                .collect();
            return Some(fno_agents::mail_control_drain::run(&rest));
        }
        return Some(fno_agents::mail_inject::run_mail_inject(rest).await);
    }

    // `mail-record`: binary-direct origin-record leaf `mail send/reply` calls.
    if matches!(command, "mail-record") {
        return Some(fno_agents::decision_trace::run_mail_record(rest));
    }

    if matches!(command, "mail-envelope") {
        return Some(fno_agents::mail_envelope::run(rest));
    }

    if matches!(command, "mail") {
        return Some(fno_agents::mail_cli::run(rest));
    }

    // Cross-host relay command; binary-direct like `mail-inject`. Contract: mail_remote's module docs.
    if matches!(command, "mail-remote") {
        return Some(fno_agents::mail_remote::run(rest));
    }

    // The cross-machine relays, binary-direct like `mail-remote` (contract: each module's docs).
    if matches!(command, "org-remote") {
        return Some(fno_agents::org_remote::run(rest));
    }
    if matches!(command, "spawn-remote") {
        return Some(fno_agents::spawn_remote::run(rest));
    }

    // Binary-direct mail transport for the events/note Python adapters; not routable.
    if matches!(command, "machine-mail-send") {
        return Some(fno_agents::machine_mail::run(rest).await);
    }

    if matches!(command, "manifest-eval") {
        return Some(fno_agents::manifest::run_manifest_eval(rest));
    }

    // `orphan-reap` is the hidden reap lever for orphaned cargo test binaries:
    // the daemon's 300s sweep and a human on a wedged machine both run
    // it binary-direct. Same `matches!` treatment as `mail-inject` so it stays
    // out of CLIENT_COMMAND_USAGE / RUST_CLIENT_COMMANDS and the routable-command
    // parity guard - it reads one ps snapshot and prints, it is not an
    // `fno agents` command.
    if matches!(command, "orphan-reap") {
        return Some(fno_agents::orphan_reap::run_orphan_reap(rest));
    }

    // `reentry-plan` is the INTERNAL machine resolver behind every
    // Claude re-entry door: the Rust/Python attach+resume arms and
    // the mux gestures consume its verdict instead of each rebuilding a
    // provider argv. Matched with `matches!` (like `claim`/`detect`) so it
    // stays out of the routable-command parity sets - it is not an `fno agents`
    // command, and adding one needs the Python surface to grow with it.
    if matches!(command, "reentry-plan") {
        return Some(fno_agents::reentry::run_reentry_plan(
            rest,
            &AgentsHome::from_env(),
        ));
    }

    // `resume-argv` is the INTERNAL machine command behind the mux resume
    // gesture: the server shells it for the codex lane instead of
    // re-deriving the declared form and losing the writable-roots grant.
    // Same `matches!` treatment as `reentry-plan` - it is not an `fno agents`
    // command, so the routable-command parity guard never sees it.
    if matches!(command, "resume-argv") {
        return Some(fno_agents::pane_relaunch::run_resume_argv(rest));
    }

    if matches!(command, "manifest-for-session") {
        return Some(fno_agents::manifest_lookup::run_manifest_for_session(rest));
    }

    // `name-mint`/`name-parse`/`name-codes` are the INTERNAL machine commands
    // behind the delegation flip: Python's naming.py shells them so the
    // vocabulary tables, mint, and parse own exactly one implementation.
    // `matches!` like `reentry-plan`: not `fno agents` commands, so the
    // routable-command parity sets never see them.
    if matches!(command, "name-mint") {
        return Some(fno_agents::naming::run_name_mint(rest));
    }
    if matches!(command, "name-parse") {
        return Some(fno_agents::naming::run_name_parse());
    }
    if matches!(command, "name-codes") {
        return Some(fno_agents::naming::run_name_codes(rest));
    }

    if matches!(command, "claude-birth-exec") {
        // `revive-proof` rides this action as an argument (the fleet-incident
        // gh-budget shape): law d-fe66560a allows no new client action, and
        // the revival gate belongs to this door. A fork that never started
        // must refuse even when the daemon is the thing wedged.
        if rest.first().map(String::as_str) == Some("revive-proof") {
            return Some(fno_agents::revive_proof::run_revive_proof(
                rest.get(1..).unwrap_or(&[]),
            ));
        }
        return Some(fno_agents::claude_supervisor::run_birth_exec(rest));
    }
    None
}

/// Which hidden machine leaf answers this command before any `--help` read.
async fn machine_leaf_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `review-summary` and `review-level` are binary-direct review doors:
    // review-summary authors the display line a pre-push reviewed PR carries -
    // the /fno:ship pr create worker runs it against the attestation journal
    // (global state root by default) and appends its stdout to the body - and
    // review-level is the resolve-level leaf the Python caller reaches through
    // the binary. The `review_doors::dispatch` match table routes them the
    // way the `matches!` arms route `claim`/`detect`, outside the
    // routable-command parity guard - they read state and print.
    if let Some(code) = fno_agents::review_doors::dispatch(command, rest) {
        return Some(code);
    }

    // PR creation reads this branch-vs-base test inventory for its body; it
    // stays binary-direct because it only folds a local diff and needs no daemon.
    if matches!(command, "test-delta") {
        return Some(fno_agents::test_delta::run_test_delta(rest));
    }

    // The style gate's hidden binary-direct door (mail, encounters, markdown);
    // matched with `matches!` so the routable-command parity sets never see it.
    if matches!(command, "style-check") {
        return Some(fno_agents::backlog::style_check::run_cli(rest));
    }

    // The mail receipt renderer's hidden binary-direct door: the send receipt
    // prose moved from Python (file budget) with Python keeping transports.
    // Same `matches!` treatment so the parity guard never sees it.
    if matches!(command, "mail-receipt") {
        return Some(fno_agents::mail_receipt::run_mail_receipt(rest));
    }

    // The historical-mail row contract's hidden binary-direct door: the
    // archive id, the receiver-side block parser, and the provenance
    // validation live in Rust once; Python keeps the transcript scan and the
    // bus write. Same `matches!` treatment so the parity guard never sees it.
    if matches!(command, "mail-backfill") {
        return Some(fno_agents::mail_backfill::run_mail_backfill(rest));
    }

    // `component-verdict` is the HIDDEN decision command for deployed-component
    // convergence: reads one JSON request on stdin (expected rev +
    // per-component probes) and prints the per-component verdict. Binary-direct
    // transport for update/doctor, matched with `matches!` like `state` so it
    // stays out of CLIENT_COMMAND_USAGE / RUST_CLIENT_COMMANDS and the parity guard.
    if matches!(command, "component-verdict") {
        return Some(fno_agents::component_update::run_component_verdict(rest));
    }

    // `source-pin` is the HIDDEN decision command for machine-wide update source
    // eligibility: resolve|record|sync. A refusal is data (exit 0,
    // decision: refuse), not a process error, so the Python transport can map
    // it to its own refusal message verbatim. Same `matches!` treatment as
    // `component-verdict` so the routable-command parity guard does not see it.
    if matches!(command, "source-pin") {
        return Some(fno_agents::source_pin::run_source_pin(rest));
    }

    // `task-context-prepare`/`-gate`/`-stage`/`-show`/`-revalidate`/`-payload`
    // are the INTERNAL machine commands behind the task-context execution binding
    // the doors (target init, resume receipt validate/show, spawn
    // payload adapter) shell them so every enforced decision (validation,
    // digest, stage monotonicity, live-source revalidation, declared-gate env
    // semantics, bounded payload render) is native. stdin-JSON like
    // evidence-gate, so the routable-command parity sets never see them.
    if matches!(command, "task-context-prepare") {
        return Some(fno_agents::task_context::run_prepare(rest));
    }
    if matches!(
        command,
        "task-context-gate"
            | "task-context-stage"
            | "task-context-show"
            | "task-context-revalidate"
            | "task-context-payload"
    ) {
        return Some(match command {
            "task-context-gate" => fno_agents::task_context::run_gate(rest),
            "task-context-stage" => fno_agents::task_context::run_stage(rest),
            "task-context-show" => fno_agents::task_context::run_show(rest),
            "task-context-payload" => fno_agents::task_context::run_payload(rest),
            _ => fno_agents::task_context::run_revalidate(rest),
        });
    }

    // `evidence-gate` is the hidden binary-direct transport for the ruling and
    // note evidence gates: the checker + bounded read runner ported
    // out of the file-budget-gated Python `fno.decide.evidence` module. Reads
    // one JSON request on stdin (lane, text, reads, root, timeout) and prints
    // one JSON answer on stdout; a refusal is data (`ok: false`), not a
    // process error. Same `matches!` treatment as `component-verdict`, so no
    // advertised fno command is added.
    if matches!(command, "evidence-gate") {
        return Some(fno_agents::evidence::run_evidence_gate(rest));
    }

    // `lead-escalation-text` is the hidden binary-direct transport for the
    // lead escalation renderer (ported out of `fno.lead.escalate`): Python
    // keeps the question fold and the liveness read, this side only renders.
    // Same `matches!` treatment as `component-verdict`, so no advertised fno
    // command is added.
    if matches!(command, "lead-escalation-text") {
        return Some(fno_agents::lead_escalation::run_lead_escalation_text(rest));
    }

    // `fleet-task` is the hidden binary-direct transport the Python reconcile
    // lanes ride through command_call (fleet_task.rs). Same `matches!` treatment
    // as `component-verdict`: no advertised fno command.
    if matches!(command, "fleet-task") {
        return Some(fno_agents::fleet_task::run_fleet_task(rest));
    }

    // `review-start` (hidden codex review command, node): the app-server
    // `review/start` RPC; claude's `--raw /code-review` counterpart; no advertised command.
    if matches!(command, "review-start") {
        return Some(fno_agents::codex_inject::run_review_start(rest).await);
    }

    // `scratch` is the scratch-shape sweep: `fno doctor scratch
    // sweep|report` routes here binary-direct via the Python leaf. Matched
    // with `matches!` like `mail-inject` so the routable-command parity guard
    // does not see it - it is not an `fno agents` command; the doctor group
    // owns its surface.
    if matches!(command, "scratch") {
        return Some(fno_agents::scratch::run_cli(rest));
    }

    // `codex-assign-project` is the hidden project-assignment command:
    // resolve or create the repo's codex project for --cwd, and when
    // --thread-id is given, assign that bound thread to it. The Python headless
    // create lane shells this binary fire-and-forget after `thread.started`.
    // Same `matches!` treatment as `review-start` so it stays out of
    // CLIENT_COMMAND_USAGE / RUST_CLIENT_COMMANDS and the routable-command parity guard.
    if matches!(command, "codex-assign-project") {
        return Some(fno_agents::codex_inject::run_codex_assign_project(rest).await);
    }

    // Hidden native claim operations; `fno agents claim` owns the operator surface.
    if matches!(command, "claim") {
        if rest.first().map(String::as_str) == Some("birth") {
            return Some(fno_agents::first_check::run_record(
                rest.get(1..).unwrap_or(&[]),
            ));
        }
        return Some(fno_agents::claim_commands::run_claim(rest));
    }

    // `detect` is the HIDDEN debug front over the screen-manifest fallback
    // authority (`fno_agents::scrape`): `detect explain <agent>` prints which
    // rung of the badge lattice currently badges the agent. Same `matches!`
    // treatment as `claim` so it stays out of CLIENT_COMMAND_USAGE /
    // RUST_CLIENT_COMMANDS and the parity guard.
    // `node-origin` is the HIDDEN transport command over the request-origin
    // decision (fno_agents::node_origin): Python birth assembly posts birth
    // records and stamps the receipt. Same `matches!` treatment as `claim`
    // so the routable-command parity guard does not see it; the command IS
    // registered in ALL_CLIENT_ACTIONS because the command-surface ratchet's
    // binary probe reads the unknown-command refusal.
    if matches!(command, "node-origin") {
        return Some(fno_agents::node_origin::run_node_origin(rest));
    }

    if matches!(command, "detect") {
        return Some(fno_agents::scrape::run_detect(rest));
    }
    None
}

/// Which schema, loop, test-run or fleet-record door answers this command.
async fn loop_and_record_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `--emit-schema` is a read-only introspection flag: prints the unified
    // envelope + status-v1 schema + known event kinds as JSON to stdout, then
    // exits 0. Used by scripts/check-event-schema-parity.sh. Must not start
    // the daemon or read any runtime state (AC2-HP: side-effect-free).
    if command == "--emit-schema" {
        let schema = fno_agents::emit_schema_json();
        match serde_json::to_string_pretty(&schema) {
            Ok(s) => {
                println!("{s}");
                return Some(0);
            }
            Err(e) => {
                eprintln!("fno-agents --emit-schema: serialization error: {e}");
                return Some(1);
            }
        }
    }

    // `loop-check`: stop-hook decision command (see loopcheck.rs module doc).
    // Direct dispatch; no daemon RPC.
    if command == "loop-check" {
        return Some(fno_agents::loopcheck::run_loop_check(rest));
    }

    // `loops`: daemon-free global pause sentinel owner. Direct dispatch keeps
    // the safety switch usable when the daemon itself is unavailable.
    if command == "loops" {
        return Some(fno_agents::loops_pause::run_loops(rest));
    }

    // `state path` is the shell-hook surface for project-space path resolution
    // (matched with `matches!` like `version`/`mail-inject`: a binary-direct
    // command that stays out of CLIENT_COMMAND_USAGE / RUST_CLIENT_COMMANDS and so out
    // of the client<->router parity guard; the command-surface ratchet's binary
    // probe reads the unknown-command refusal, so the command IS registered in
    // ALL_CLIENT_ACTIONS like every direct dispatch). No daemon, from cwd.
    if matches!(command, "state") {
        return Some(fno_agents::state_path::run(rest));
    }

    // `honesty-sweep`: declared-vs-measured sweeps over declared populations
    // (see its own doc in honesty_sweep.rs). Direct dispatch; no daemon RPC -
    // a sweep reads tables, manifests, or piped rows from disk.
    if command == "honesty-sweep" {
        return Some(fno_agents::honesty_sweep::run_honesty_sweep(rest));
    }

    // `probe-run`: see its own doc in acceptance_evidence.rs. Direct dispatch.
    if command == "probe-run" {
        return Some(fno_agents::acceptance_evidence::run_probe_run(rest));
    }

    // `prove-it-verdicts`: the one reader for terminal prove-it records
    // (see its own doc in prove_it_verdicts.rs). Direct dispatch; no
    // daemon RPC - a verdict read walks the graph and plan artifacts files.
    if command == "prove-it-verdicts" {
        return Some(fno_agents::prove_it_verdicts::run_prove_it_verdicts(rest));
    }

    // `test-run`: the native process-group owner behind `fno doctor test`
    // (see test_run.rs doc). Direct dispatch, no daemon RPC - a test run must
    // not depend on a live daemon to clean up after itself. Same `matches!`
    // treatment as `probe-run`/`state` so it stays out of CLIENT_COMMAND_USAGE /
    // RUST_CLIENT_COMMANDS and the routable-command parity guard: not an `fno
    // agents` command; callers are `cli/src/fno/test_runner.py` and the wrapper.
    // `test-run`: the bounded suite runner, plus the test family's raw
    // doors as its arguments (the shrink law allows no new action, so
    // `env`, `sweep` and `command` are arguments, not actions): `test-run env`
    // is the hermetic child env door, `test-run sweep` the orphaned-keeper
    // sweep for the pytest conftest, and `test-run doctor <argv>` the whole
    // native test command (pytest/rust/smoke/census; the advertised surface is
    // the `fno doctor test` front).
    if command == "test-run" {
        let tail = rest.get(1..).unwrap_or(&[]);
        match rest.first().map(String::as_str) {
            Some("env") => return Some(fno_agents::test_env::run_test_env(tail)),
            Some("sweep") => return Some(fno_agents::graph_keeper::run_keeper_sweep(tail)),
            Some("doctor") => return Some(fno_agents::test_command::run_test_command(tail)),
            _ => {}
        }
        return Some(fno_agents::test_run::run_test_run(rest));
    }

    // `fleet-incident`: the durable fleet incident breaker (see
    // fleet_incident.rs doc). Direct dispatch, no daemon RPC; the routing
    // body lives in the module beside the breaker.
    if command == "fleet-incident" {
        return Some(fno_agents::fleet_incident::run_dispatch(rest).await);
    }

    if command == "compaction" {
        return Some(fno_agents::compaction::run_compaction(rest));
    }

    // `mail-teach`: the fmail read command's once-per-session lesson state
    // (see mail_teach.rs doc). Direct dispatch like compaction - a hook
    // carrier must resolve under the same session-start and post-compact
    // budgets where the daemon is beside the point.
    if command == "mail-teach" {
        return Some(fno_agents::mail_teach::run_mail_teach(rest));
    }

    // `provider-cap`: the armed cap actor's read + decide commands (see
    // provider_cap.rs doc). Direct dispatch, no daemon RPC: a status read
    // computes on demand when no fresh daemon snapshot exists, and a decision
    // record must be writable when the daemon is the thing wedged.
    if command == "provider-cap" {
        return Some(fno_agents::provider_cap_commands::run_provider_cap(rest));
    }

    // `announce`: fleet announcements (see announce.rs doc). Direct
    // dispatch; no daemon RPC - a send is one locked bus append, a read is a
    // scan + cursor write, and both must work when the daemon is wedged.
    if command == "announce" {
        return Some(fno_agents::announce::run_announce(rest));
    }

    // `chats`: the conversation record plane (see chats.rs doc). Direct
    // dispatch like announce; no daemon RPC - a record write must work when
    // the daemon is wedged, and the read doors back the thread read model.
    if command == "chats" {
        return Some(fno_agents::chats::run_chats(rest));
    }
    None
}

/// Which target-loop, merge or spawn-gate door answers this command.
fn target_and_merge_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `mail-threads`: the mux Messages tab's thread read model (see
    // mail_threads.rs doc). Direct dispatch like chats; no daemon RPC - a
    // read must work when the daemon is wedged. Hidden from help and from
    // ALL_CLIENT_ACTIONS, like team-fold.
    if command == "mail-threads" {
        return Some(fno_agents::mail_threads::run_mail_threads(rest));
    }

    // `update-journal`: the `fno doctor update` lifecycle's one Rust door
    // (see update_journal.rs doc). Direct dispatch, no daemon RPC: the
    // lifecycle rows land even when the daemon is wedged, which is the
    // failure the node exists to name. Never registered in
    // ALL_CLIENT_ACTIONS (the action list is shrink-only); the update
    // lifecycle execs the binary directly, the dispatch rides the
    // `command ==` arm.
    if command == "update-journal" {
        return Some(fno_agents::update_journal::run_update_journal(rest));
    }

    // `capabilities` / `target-family` (change 2): read-only leaves
    // over the packaged capability table and the merge-posture family table,
    // `review-coverage`: standalone review_coverage producer (see its own doc
    // in loopcheck.rs). Direct dispatch like loop-check; no daemon RPC.
    if command == "review-coverage" {
        return Some(fno_agents::loopcheck::run_review_coverage(rest));
    }

    // `loop run`: unified driver loop (see loop_target.rs doc). Direct
    // dispatch like loop-check; no daemon RPC.
    if command == "loop" {
        return Some(fno_agents::loop_target::run_loop_command(rest));
    }
    // `finalize`: terminal-only side-effect WRITER (see finalize.rs doc); no daemon RPC.
    if command == "finalize" {
        return Some(fno_agents::finalize::run_finalize(rest));
    }

    // `kill-check`: the Rust port of scripts/lib/kill-criteria.sh; no daemon RPC.
    if command == "kill-check" {
        return Some(fno_agents::kill_criteria::run_kill_check(rest));
    }

    // `authorized-merge`: the one merge/arm authorization (see
    // authorized_merge.rs doc). One payload in, one receipt out, one verdict.
    if command == "worked-nodes" {
        return Some(fno_agents::worked_nodes::run_worked_nodes(rest));
    }

    if command == "authorized-merge" {
        return Some(fno_agents::authorized_merge::run_authorized_merge(rest));
    }

    if command == "rename" && rest.first().map(String::as_str) == Some("retask") {
        return Some(fno_agents::retask::front::run_retask_cli(
            rest.get(1..).unwrap_or(&[]),
        ));
    }

    if command == "route-slot" {
        return Some(fno_agents::route_slot::run_route_slot(rest));
    }

    if command == "spawn-gate" {
        return Some(fno_agents::spawn_gate_command::run_spawn_gate(rest));
    }
    None
}

/// Which spawn-policy, publish or lead-shape door answers this command.
fn spawn_policy_and_lead_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `spawn-overlay`: the spawn-defaults resolver; Python's spawn seam, the
    // doctor readout and the failover walker send one payload, read the answer.
    if command == "spawn-overlay" {
        return Some(fno_agents::spawn_overlay::run_spawn_overlay(rest));
    }

    // `spawn-axes`: the seam's billing axes (route/account/model); the Python
    // front door projects the seam's facts and applies the returned plan.
    if command == "spawn-axes" {
        return Some(fno_agents::spawn_axes::run_spawn_axes(rest));
    }

    // `permission-tokens`: the one permission vocabulary; Python's pane mapper,
    // the seam's mappability read and both front doors call it - ONE table,
    // not three disagreeing copies.
    if command == "permission-tokens" {
        return Some(fno_agents::codex_posture::run_permission_tokens(rest));
    }

    // `sandbox-probe`: the pre-seating verdict as a command. The spawn seam
    // calls the probe in process and owns the exit-85 refusal.
    if command == "sandbox-probe" {
        return Some(fno_agents::sandbox_probe::run_sandbox_probe(rest));
    }

    // `release-notes`: the update modal's PR-grouped release notes (see
    // release_notes.rs). Python's `fno doctor update --check` resolver calls
    // it through the command seam; it answers `{"notes": null}` on any git
    // failure rather than refusing - the payload never blocks on it.
    if command == "release-notes" {
        return Some(fno_agents::release_notes::run_release_notes(rest));
    }

    // `fallback-chain`: the failover chain walk (see fallback_chain.rs doc).
    // Python resolves config and paths and serializes the candidate links;
    // this command reads the provider runtime-state file, derives headroom
    // verdicts, and answers the eligible links with their walk-memory ids
    // and spawn flags.
    if command == "fallback-chain" {
        return Some(fno_agents::fallback_chain::run_fallback_chain(rest));
    }

    // `commands`: hidden claim-style (matches!); the action list is
    // shrink-only, so the harness-commands skill teaches the spelling.
    if matches!(command, "commands") {
        return Some(fno_agents::harness_commands::run_commands(rest));
    }

    // `publish-review`: the reviewer lane's second GitHub identity (see
    // publish_review.rs doc). Direct dispatch; no daemon RPC. Python's emit
    // chokepoint and the hidden `fno do pr publish-review` command send one JSON
    // payload and read the answer back; the command is binary-first, never an
    // auto-routed `fno agents` surface.
    if command == "publish-review" {
        return Some(fno_agents::publish_review::run_publish_review(rest));
    }

    // `canonical-check`: the canonical-sync divergence read. Direct
    // dispatch; no daemon RPC. The Python post-merge sync sends one JSON
    // payload and reads the answer back; binary-first like `publish-review`.
    if command == "canonical-check" {
        return Some(fno_agents::canonical_check::run_canonical_check(rest));
    }

    // `sync-canonical`: the post-merge canonical sync + its catch-up sweep
    // and staleness alarm, native. The Python `fno do pr sync-canonical`
    // transport sends one JSON payload and reads the answer back;
    // binary-first like `canonical-check`.
    if command == "sync-canonical" {
        return Some(fno_agents::sync_canonical::run_sync_canonical_command(rest));
    }

    // `lead-state`/`lead-shape`: the lead reader and the shape rewrite (see
    // lead_state.rs doc). Direct dispatch, daemon-free reads; the Python
    // `fno agents hq shape` shell and escalate's client invoke the binary
    // directly rather than routing through the agents command set.
    if matches!(command, "lead-state" | "term-state") {
        return Some(fno_agents::lead_state::run_lead_state(rest));
    }
    if matches!(command, "lead-shape" | "term-shape") {
        return Some(fno_agents::lead_state::run_lead_shape_or_term(rest));
    }

    // `graph-get`/`bash-census`/`session-start-bytes`: daemon-free reads, not routable `fno agents` commands (same reasoning as kill-check).
    // `judge`: the blueprint judge's grading half (lens prompts,
    // model spawn, verdict parsing). Daemon-free like graph-get; the Python
    // `fno doctor observer judge` / `sweep --judge` wrappers shell HERE and
    // own event emission (fno.events single-sourced there).
    if command == "judge" {
        return Some(fno_agents::blueprint_judge::run_judge(rest));
    }

    // `backlog-notes` and `backlog-note` folded into the grouped `backlog`
    // dispatcher (the early transport arm); see backlog::cli's doc.
    // `org-vacancies`: the orphan-team sweep for `fno agents hq`, daemon-free;
    // `==` dispatch like graph-get, registered in ALL_CLIENT_ACTIONS.
    if command == "org-vacancies" || command == "team-orphans" {
        return Some(fno_agents::lead_state::run_org_vacancies(rest));
    }
    // `org-fold`: the team scope fold for `fno agents hq --nodes` and the
    // local board's org section, daemon-free like org-vacancies; the workers
    // column rides the same native claim verdicts `claim sweep` established,
    // so a fold and the claims surface cannot disagree about who holds a node.
    if command == "org-fold" || command == "team-fold" {
        return Some(fno_agents::org_fold::run_org_fold(rest));
    }
    None
}

/// Which daemon-free fleet read answers this command.
fn fleet_read_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `lead-history`: the team-scope lead_checkin readback for `fno agents hq history`, exec'd by the front door.
    //
    // Daemon-free `==` dispatch like org-fold: the caller's team scope and the live journals resolve natively when the argv names none. `--verdict` reads the same journals as a tenure verdict (law d-fe66560a).
    if command == "lead-history" {
        if rest.iter().any(|a| a == "--verdict") {
            if let Err(message) = fno_agents::role_cwd::rebind("verdict") {
                eprintln!("{message}");
                return Some(2);
            }
            return Some(fno_agents::lead_history::run_lead_verdict(rest));
        }
        return Some(fno_agents::lead_history::run_lead_history(rest));
    }

    // `lead-checkin`: one command runs the lead check-in body for
    // `fno agents hq checkin`. Daemon-free beat like lead-history: Python
    // resolves the caller's team scope and Python-owned paths, the native
    // side gathers, prints, diffs and journals the row, reusing the org-fold
    // fold and the lead-history scan in process.
    if command == "lead-checkin" {
        if let Err(message) = fno_agents::role_cwd::rebind("checkin") {
            eprintln!("{message}");
            return Some(2);
        }
        return Some(fno_agents::lead_checkin::run_lead_checkin(rest));
    }
    // `evals-macro`: the macro-eval failure-pattern leaderboard for
    // `fno doctor evals macro`. Daemon-free read, `==` dispatch like
    // lead-history: Python resolves the journal paths, the native side owns
    // the fold so the file-budget Python-tree ratchet holds.
    if command == "evals-macro" {
        return Some(fno_agents::evals_macro::run_evals_macro(rest));
    }
    // `stalls`: the weekly stall-cause table, daemon-free read, == dispatch
    // like evals-macro: never registered in ALL_CLIENT_ACTIONS; `fno doctor
    // stalls` shells HERE through resolve_binary.
    if command == "stalls" {
        return Some(fno_agents::stalls::run_stalls(rest));
    }
    // `lead-rundown`: the lead ledger page for `fno agents lead ledger`.
    // Same split as lead-history: Python resolves the org and the paths,
    // the native side owns the page assembly, and the fold's scope_nodes ride
    // in the org JSON, so the page cannot disagree with the org.
    if command == "lead-rundown" || command == "term-ledger" {
        return Some(fno_agents::rundown::run_lead_ledger(rest));
    }
    if command == "bash-census" {
        return Some(fno_agents::bash_census::run_bash_census(rest));
    }
    if command == "session-backfill" {
        return Some(fno_agents::session_backfill::run(rest));
    }
    // `intel`: the session-provenance fold, daemon-free read, == dispatch
    // like board/reclaim: never registered in ALL_CLIENT_ACTIONS (the action
    // list is shrink-only, d-fe66560a) and never routed by `fno agents`;
    // `fno doctor intel` shells HERE through resolve_binary.
    if command == "intel" {
        return Some(fno_agents::intel::run_intel(rest));
    }
    // `reclaim`: the machine janitor, daemon-free. Not a routable
    // `fno agents` command; the Python surface is `fno doctor reclaim`, a thin
    // wrapper that shells HERE, and the daemon's daily sweep calls the gate
    // in-process.
    if command == "reclaim" {
        return Some(fno_agents::reclaim::run_reclaim(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    // `plugin-install`: the filtered-stage installer for the plugin
    // harnesses, daemon-free. The Python surface `fno config plugin install`
    // shells HERE for every harness.
    if command == "plugin-install" {
        return Some(fno_agents::plugin_install::run_plugin_install(rest));
    }
    if command == "session-start-bytes" {
        return Some(fno_agents::session_start_bytes::run_session_start_bytes(
            rest,
        ));
    }

    // `territory-rows`: the territory fact set's daemon-free reads. Direct
    // dispatch like graph-get: the Python `fno config active-backlog-*`
    // passthroughs invoke the binary directly.
    if command == "territory-rows" {
        return Some(fno_agents::territory::run_territory_rows(rest));
    }
    if command == "active-backlog-receipt" {
        return Some(fno_agents::territory::run_active_backlog_receipt(rest));
    }
    if command == "select-read" {
        return Some(fno_agents::select_read::run(rest));
    }
    if command == "territory-verdict" {
        return Some(fno_agents::spawn_gate::run_territory_verdict(rest));
    }

    // `board`: the lead board collector, read-only, daemon-free. Not a
    // routable `fno agents` command (same `matches!` treatment as graph-get): the
    // front door execs `board --user-command` for `fno inbox board` / `fno lead
    // board`, which renders the text page here, and the stop hook reads the
    // collector in-process. Matched with `==` beside graph-get so the routable-command
    // parity guard does not see it - no advertised fno command is added.
    if command == "board" {
        return Some(fno_agents::org_board::run_board(rest));
    }

    // `notify-watch`: the operator-notice sampler, read-only,
    // daemon-free. Same `==` treatment as `board`: a read-only collector, not
    // a routable `fno agents` command, so no advertised fno command is added.
    if command == "notify-watch" {
        return Some(fno_agents::operator_notice::run_notify_watch_command(rest));
    }

    // `verify-evidence`: Rust port of scripts/lib/verify-event-evidence.sh
    // (see verify_evidence.rs doc). Direct dispatch.
    if command == "verify-evidence" {
        return Some(fno_agents::verify_evidence::run_verify_evidence(rest));
    }
    None
}

/// Which registry-direct door answers this command, before build_request.
async fn registry_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // Retired at G4: the grid, the WebSocket drive surface, and the
    // interactive daemon PTY hosting behind `host`/`promote` were deleted when
    // the mux became the agent-PTY substrate. Each prints a one-line pointer to
    // the mux and exits non-zero, never a silent no-op (AC5-EDGE).
    if let Some(pointer) = super::retired_command_pointer(command) {
        eprintln!("{pointer}");
        return Some(2);
    }

    // Python-only commands ported to the Rust client: these read state/registry/
    // event files directly (or print a stub) without a daemon RPC, so they
    // dispatch here before build_request. Byte-for-byte parity with the Python
    // implementations is the contract; see `fno_agents::client_commands`.
    if command == "drive-authority" {
        return Some(fno_agents::client_commands::run_drive_authority(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    if command == "trace" {
        return Some(fno_agents::client_commands::run_trace(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    // rename --from-journal: the daemon-free label rebuild, keyed by session
    // id; every other rename form still RPCs the daemon below.
    if command == "rename" && rest.iter().any(|a| a == "--from-journal") {
        return Some(fno_agents::rename::run_from_journal(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    // registry-json: the daemon-free registry projection the hooks read.
    // Starts nothing, so the Stop hook's never-lazy-start promise holds.
    if command == "registry-json" {
        return Some(fno_agents::registry_json::run_registry_json(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    if matches!(command, "worker-wake") {
        return Some(fno_agents::worker_wake::run_dry_run(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    // revival-check: whether a spawn --resume revives an existing row instead
    // of forking. Starts nothing.
    if command == "revival-check" {
        return Some(fno_agents::revival_check::run_revival_check(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    // `resume --substrate thread` is the pane-to-thread LIFECYCLE move, not a
    // re-entry: it falls through to build_request, which routes it to the
    // daemon's agent.convert, whose agent lock outlives the client.
    // A row that is not a pane has nothing to convert, so the same flag falls
    // back to the plain resume, which relaunches an exited session as a thread.
    let resume_rest = if command != "resume" {
        None
    } else if fno_agents::resume_args::requests_conversion(rest) {
        fno_agents::resume_args::reentry_argv_for_unpaned_conversion(rest, &AgentsHome::from_env())
    } else {
        Some(rest.to_vec())
    };
    if let Some(rest) = resume_rest {
        // resume_wake's wake arms build their own runtimes and block_on them;
        // on this thread that panics inside the ambient runtime. A fresh
        // thread is legal in both contexts (gc_sweep::stop_row_process is
        // the same shape).
        let home = AgentsHome::from_env();
        return Some(
            match std::thread::spawn(move || fno_agents::client_commands::run_resume(&rest, &home))
                .join()
            {
                Ok(code) => code,
                Err(payload) => std::panic::resume_unwind(payload),
            },
        );
    }
    if command == "adopt" {
        return Some(fno_agents::client_commands::run_adopt(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    if command == "attach" {
        return Some(fno_agents::attach::run_attach(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    // `recover`: manual restoration of a recorded session under its
    // account/route; reads the registry and resolver directly, no daemon RPC.
    if command == "recover" {
        return Some(fno_agents::client_commands::run_recover(
            rest,
            &AgentsHome::from_env(),
        ));
    }
    if command == "logs" {
        return Some(fno_agents::client_commands::run_logs(rest, &AgentsHome::from_env()).await);
    }
    // Inside-leg state push (E3.2): a per-turn hook reports {working|blocked|done}.
    // `report`: sends to an ALREADY-RUNNING daemon; must never lazy-start one.
    // `report --kind session` rides the SAME action (the action list is
    // shrink-only): the SessionStart transport with the raw payload on stdin.
    if command == "report" {
        return Some(
            fno_agents::session_report::run_report_dispatch(rest, &AgentsHome::from_env()).await,
        );
    }
    // `wait`: poll registry.json directly for a state (no daemon RPC).
    if command == "wait" {
        return Some(fno_agents::wait::run_wait(rest, &AgentsHome::from_env()).await);
    }
    None
}

/// Which PR or event-stream door answers this command.
async fn pr_and_event_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // The list path is an internal operand of the existing binary action;
    // public callers keep using `fno do pr list` through the Python adapter.
    if matches!(command, "pr-heal") {
        if rest.first().map(String::as_str) == Some("list") {
            return Some(fno_agents::pr_list::run_pr_list(
                rest.get(1..).unwrap_or(&[]),
            ));
        }
        return Some(fno_agents::heal::run_heal(rest));
    }
    if matches!(command, "pr-push") {
        return Some(fno_agents::pr_push::run_push(rest));
    }
    // The binary-direct `pr-*` commands (body-check, create, base-lineage-check,
    // and the ports that follow them) route through one door, so a new command
    // costs a match arm in pr_door.rs, not routing lines against this file's
    // shrink-only budget.
    if let Some(code) = fno_agents::pr_door::run_command(command, args) {
        return Some(code);
    }
    // `pr-closure-*`: the closure-line parser/renderer (parse, render) and
    // the produce commands ported from Python (branch-ids, ensure, trailer,
    // bind-created, context); binary-direct like `pr-body-check`.
    if command.starts_with("pr-closure") {
        return Some(fno_agents::org_board::pr_closure::run_command(args));
    }
    if matches!(command, "pr-rebase") {
        return Some(fno_agents::pr_rebase::run_rebase(rest));
    }
    // `subscribe`: follow the daemon's own `events.jsonl` and stream registry
    // state transitions + pane exits as NDJSON. File-follow, no daemon RPC, so it
    // dispatches here before build_request.
    if command == "subscribe" {
        return Some(fno_agents::subscribe::run_subscribe(rest, &AgentsHome::from_env()).await);
    }

    if command == "digest" {
        return Some(fno_agents::digest::run_digest(rest, &AgentsHome::from_env()).await);
    }

    if command == "distress-scan" {
        return Some(fno_agents::distress::run_distress_scan(rest));
    }
    if command == "attest" {
        return Some(fno_agents::attest::dispatch(rest));
    }

    // `needs`: read-only needs-me-queue fold over events + ledger; daemon-free.
    if command == "needs" {
        return Some(fno_agents::needs::run_needs(rest, &AgentsHome::from_env()).await);
    }

    // `feed`: one projection joining questions.jsonl + graph.json
    // into an ordered feed whose rows carry the node id + session id the mux
    // deep link resolves. Read-only, daemon-free like `needs`: it dispatches
    // here before build_request. events.jsonl is deliberately NOT a source
    // (72% ticks; lifecycle derives from the graph, never copied).
    if command == "feed" {
        return Some(fno_agents::feed::run_feed(rest, &AgentsHome::from_env()).await);
    }

    // `day`: daemon-free like `feed`; `--commit` appends the boundary row.
    // `matches!` treatment as `claim`: the command is unregistered (the action
    // list is shrink-only, d-fe66560a); the inbox relay reaches it through
    // resolve_binary, and no advertised `fno agents day` spelling exists.
    if matches!(command, "day") {
        return Some(fno_agents::day::run_day(rest, &AgentsHome::from_env()));
    }
    None
}

/// Which daemon or registry lifecycle door answers this command, before build_request.
async fn daemon_lifecycle_doors(command: &str, args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    // `restart` swaps a stale daemon for one built from the current binary
    // SIGTERM the running daemon (graceful drain; PTY workers
    // survive), wait for the socket to clear, lazy-start fresh. Like `status`,
    // it does not fit the one-shot build_request path and dispatches here.
    // `--force` is the break-glass variant: SIGKILL the lockfile's
    // holder BEFORE any probe, because a wedged holder is exactly what the
    // probe cannot see.
    // `census`: one JSON row per long-lived process, the build-
    // staleness read that doctor/restart/update render. Composes the daemon
    // status (in-process), a ps walk of keepers, and `fno mux ls --json`.
    if command == "census" {
        return Some(fno_agents::census::run_command(rest).await);
    }

    if command == "restart" {
        let parsed = match RestartArgs::try_parse_from(rest) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{}", refusal_line("fno-agents restart", &e));
                return Some(2);
            }
        };
        if parsed.keepers_only {
            return Some(fno_agents::restart_run::run_keepers_only(parsed.json.json).await);
        }
        return Some(
            fno_agents::restart_run::run_restart(
                parsed.force,
                parsed.json.json,
                parsed.if_drifted,
                parsed.mux,
            )
            .await,
        );
    }

    // `reap` is the manual dead-row GC: the SAME sweep the daemon runs
    // on its idle tick, on demand. It operates on the registry directly under the
    // shared flock, so it needs no running daemon and dispatches here before
    // build_request.
    if command == "reap" {
        return Some(super::run_reap(rest));
    }

    // The roster-side sweep (gap one): claude rows no fno row names.
    // Like `reap`, it operates directly on live surfaces so it needs no
    // running daemon. Dry-run by default; `--apply` executes.
    if command == "roster-reap" {
        return Some(super::run_roster_reap(rest));
    }

    // The cascade verdict per NAME: the squad store's Unknown
    // members carry no session id, so the fno side asks this command for a
    // positive verdict per name instead of re-implementing the cascade.
    if command == "node-route" {
        return Some(super::run_node_route(rest));
    }
    None
}
