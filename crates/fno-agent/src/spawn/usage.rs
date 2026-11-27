//! The `fno-agent --help` surface: one usage line per dispatchable command.
//!
//! LIVES OUTSIDE THE BIN to keep the bin inside its 5,000-line budget: this
//! table is the one piece a new command still has to touch, so it moved to a
//! module named for the question it answers. Display order == slice order == `--help` order.

/// Usage line per dispatchable command; the leading token is the command name and the
/// slice order is the `--help` display order. This MUST cover every routable
/// command (the `build_request` match arms plus the directly-dispatched specials).
/// `test_rust_client_commands_match_client_rs` (Python) guards client.rs<->router
/// parity; `print_help_lists_every_routable_command` (below) guards this display
/// list against that set, so a new command cannot land without a `--help` entry
///.
pub const CLIENT_COMMAND_USAGE: &[&str] = &[
    "spawn <name> [prompt] [-H <harness>] [--route <provider/model|lane>] [--as pane|thread|headless|cloud] [--machine <host>] [-s <squad>] [-x left|right|up|down] [--cwd <dir>|--fresh|--here] [--force] [--no-wait] [--max <n>] [--when <time>] [--every <interval>] [--on <command>] [--grace <duration>] [--list-scheduled] [--cancel-scheduled <id>] [--runbook <file>] [--rehearse]  # a schedule flag queues the spawn for the daemon to fire through the gate at that time; local, named-zone or UTC time, both shown on every echo; --runbook makes the runbook file the prompt and writes a run file under the agents home, --rehearse skips its irreversible steps [+flag=<value> harness args folded to their -- spelling like the composer's + picker; a +token with a non-flag body stays prompt text; nothing folds after the fence] [--] [everything after the prompt is harness argv, never parsed; a spawn-owned flag there refuses with the corrected line; unowned flags before the prompt pass in the --flag=value form only] | --argv -- <cmd...>",
    "ask <name> <message> [--cwd <dir>|--fresh|--here] [--answer allow_once|deny]",
    "list [--all] [--status <writing|quiet|parked|orphaned|refused|unknown|signed_out|harness_error>] [--progress <advancing|awaiting-operator|parked|refused|unknown>]",
    "status",
    // --force is break-glass: it SIGKILLs a wedged lock holder (and would kill
    // a healthy one too). Plain restart is the graceful path; say so here
    // because this line is what `restart --help` prints. --if-down is the
    // idempotent ensure: start only when nothing serves, never bounce a live one.
    "restart [--force] [--if-down]  # --force: break-glass SIGKILL of the lockfile holder; plain restart is graceful; --if-down swaps only a measured-down daemon and exits 0 when one is serving",
    "reap [--json] [--dry-run] [--release <row>] [--keep <row>] [--unkeep <row>] [--kept]",
    "rename <name> --name <new-label>   -- renames the registry label and, for a teamed session, its team name (generation restarts at 1); old label stays an alias; a quiet session whose harness has a native rename also gets its title pushed, and the receipt says why when it does not",
    "rename --from-journal [--apply]    -- plan registry label rebuilds from the agents journal; --apply renames",
    "stop <name> [--force]",
    // retired-ok: help names the existing Claude callee to describe actual behavior, not to teach a direct retired command.
    "rm <name> [--force]   --force drops the registry row even when the row is LIVE or harness teardown fails; a live pane worker that cannot be stopped is still refused; a claude row's harness session is removed too (claude rm <short_id>), and claude removes that session's WORKTREE under its own guards - it keeps a worktree with uncommitted changes and refuses one holding commits it cannot confirm are saved elsewhere; a non-claude bg or headless process survives, a mux-hosted pane is killed with it",
    "loop-check --state <target-state.md> --transcript <transcript.jsonl> --cwd <project-root> [--events <events.jsonl>] [--global-events <global.jsonl>] [--settings <config.toml>] [--ledger <ledger.json>] [--now <rfc3339>] [--gh-bin <path>] [--git-bin <path>]",
    "finalize --state <target-state.md> --cwd <project-root> --reason <TerminationReason> [--transcript <transcript.jsonl>]",
    "reconcile",
    "drive-authority [--json]",
    "trace [options]",
    "registry-json",
    // The `--substrate thread` arm is a LIFECYCLE move, not a re-entry: it
    // converts a live pane into a persistent thread under the same session
    // id. The three flags that belong to it are spelled out here because
    // this line is what `resume --help` prints.
    "resume <name> [--print-command] [--message/-m <text>] [--cwd <existing-checkout>] [--account <id>] [--route <provider/model>] [--substrate thread] [--dry-run] [--allow-new-id]   # --substrate thread converts a live pane into a persistent thread, keeping the session id, node, claims and team; --dry-run prints the plan and moves nothing; --allow-new-id accepts a relaunch that minted a different session id (refused on a teamed row). Both need --substrate thread. --route moves a claude row, live or stopped, to another provider/model: it stops a live session, restamps the row's route and relaunches the same session id with the new --settings (claude-only; the account's config dir never moves). A row that recorded no launch account takes the account whose config root holds its transcript. A claude session that has exited relaunches itself on a thread under its original name. Exit codes: 0 done; 13 refused (stderr names why); 16 no live process and fno could not relaunch it (stderr names the cause and the hand-run relaunch)",
    "adopt <session-id>",
    "attach <name>",
    "logs <name> [--follow] [options]",
    "loop run --driver target [options]",
    "report --session-id <uuid> --seq <n> --state working|blocked|done [--reason <text>] [--ttl-ms <n>]",
    "wait --agent <name> --state idle|blocked|done [--timeout-ms <n>] [--json]",
    "subscribe [--agent <name>] [--kinds state,exit] [--json]",
    "digest --session <s> [--since <ts> | --since-epoch <secs>] [--json]",
    "needs [--since-epoch <secs>] [--fires-floor <n>] [--json]",
    "feed [--since-epoch <secs>] [--until-epoch <secs>] [--limit <n>] [--before <cursor>] [--after <cursor>] [--node <ids>] [--session <id>] [--kind <prefix>] [--area <names>] [--agent <name>] [--harness <names>] [--lead <name>] [--json] [--otlp] [--serve]",
    // `review-coverage` deliberately has NO entry here: the per-command --help
    // intercept would print a one-line usage and shadow the command's own
    // --help, which states the load-bearing contract (no way to assert
    // coverage without the reads, and the strict manifest-less defaults).
    //
    // `distress-scan` deliberately has NO entry here either, same reason:
    // it intercepts its own --help (DISTRESS_SCAN_USAGE in distress.rs)
    // with the fuller best-effort-always-exits-0 contract a one-liner would
    // shadow, and it is a hidden command with no external callers to discover
    // it from a top-level list.
];

/// Return the usage line for `command` (matched on the leading token), or `None`
/// for an unrecognized command.
pub fn command_usage(command: &str) -> Option<&'static str> {
    CLIENT_COMMAND_USAGE
        .iter()
        .copied()
        .find(|usage| usage.split_whitespace().next() == Some(command))
}

/// Full per-command help for a command whose contract does not fit the one-line
/// table above; checked before `command_usage` by the per-command `--help`
/// intercept in the bin. Same reason `review-coverage` owns its `--help`
/// inline: the load-bearing contract has to live in a string the
/// binary prints, next to the table entry that stays one line for the
/// top-level list.
pub const LOOP_CHECK_USAGE: &str = "\
usage: fno-agent loop-check --state <manifest> --transcript <transcript.jsonl> --cwd <project-root>
       [--driver target|lead] [--events <p>] [--global-events <p>] [--settings <p>]
       [--global-settings <p>] [--ledger <p>] [--gh-budget-ledger <p>] [--now <rfc3339>]
       [--author-harness <h>] [--hook-input-stdin] [--gh-bin <p>] [--git-bin <p>]
       [--fno-bin <p>] [--read-timeout-ms <n>] [--harness <h>] [--harness-session <id>]

The stop-hook decision command: it decides whether a driven session may stop,
and every verdict comes from external truth read fresh on each fire - PR
existence, CI, required-bot review, plan done_probes, budget. The
session's own claim of done is not an input.

--driver selects the arm and the manifest kind --state points at. target
(the default) reads a target manifest (target-state.md) and asks whether
its one deliverable shipped. lead reads a lead manifest (frontmatter
scope) and asks whether the team scope drained. The arm is chosen by the
flag, never by sniffing the file, and any other value is refused.

--harness and --harness-session name the harness and harness session id
of the caller that asked this target to stop. With both set, the
registry answers who may drive this target before any progress logic
runs. With either absent, the engine answers exactly as it always has.

Required: --state, --transcript, --cwd. Unknown flags are tolerated for
shim forward-compat. stdout is one JSON decision; exit 0 = a decision
(verdict allow or block), 2 = bad arguments.
";

/// The fuller help body for `command`, when one exists above. `None` falls
/// through to the one-line `command_usage` entry.
pub fn command_help(command: &str) -> Option<&'static str> {
    match command {
        "loop-check" => Some(LOOP_CHECK_USAGE),
        "announce" => Some("fno agent announce - Fleet announcements on one scope."),
        "attest" => Some("fno agent attest - The review-attestation carrier over the shared primary."),
        "authorized-merge" => Some("fno agent authorized-merge - The one authorized merge operation over stdin JSON."),
        "autonomy" => Some("fno agent autonomy - Every path that can start a session without an operator."),
        "bash-census" => Some("fno agent bash-census - Bash-call shares and top command tables over transcripts."),
        "claim" => Some("fno agent backlog - The shared-store backlog claims."),
        "compaction" => Some("fno agent compaction - Compaction stamps."),
        "fallback-chain" => Some("fno agent fallback - Failover chain walk over a stdin payload."),
        "harness" => Some("fno agent harness - Instruments over the harness capability table."),
        "honesty-sweep" => Some("fno agent honesty-sweep - Declared-vs-measured sweep over declared populations."),
        "judge" => Some("fno agent judge - Grade a plan against the five product questions."),
        "lead" => Some("fno agent refresh-canon - Refresh the beat's canon doc, resolving the plugin root itself."),
        "lead-checkin" => Some("fno agent checkin-raw - The check-in body over explicit scope and event paths."),
        "lead-history" => Some("fno agent history-raw - The check-in readback over explicit scope and event paths."),
        "lead-rundown" => Some("fno agent rundown-raw - The rundown page renderer over explicit inputs."),
        "mcp" => Some("fno agent mcp - MCP sidecar client commands."),
        "orphans" => Some("fno agent orphans - Processes that outlived whatever started them."),
        "org-fold" => Some("fno agent org chart fold - The role scope fold."),
        "org-vacancies" => Some("fno agent org chart vacancies - Roles whose holder is gone."),
        "permission-tokens" => Some("fno agent permission-tokens - The permission-posture owner over a stdin payload."),
        "probe-run" => Some("fno agent probe - Evaluate a plan's named probe list."),
        "prove-it-verdicts" => Some("fno agent prove-it - Read terminal prove-it records."),
        "provider-cap" => Some("fno agent provider-cap - The provider-cap actor."),
        "recover" => Some("fno agent recover - Restore a recorded claude session under its account."),
        "revival-check" => Some("fno agent revival - Whether a spawn --resume revives an existing row."),
        "roles" => Some("fno agent roles - Bounded business-role definitions and resolutions."),
        "route-slot" => Some("fno agent slot - Delivery-slot resolver over a stdin payload."),
        "sandbox-probe" => Some("fno agent sandbox-probe - The codex sandbox verdict over a stdin payload."),
        "select-read" => Some("fno agent select-read - One bounded backlog read under the select timeout."),
        "session-start-bytes" => Some("fno agent session-start-bytes - The session-start preamble byte total."),
        "spawn-axes" => Some("fno agent axes - Spawn billing axes (route, account, model) over a stdin payload."),
        "spawn-gate" => Some("fno agent gate - The one spawn gate: stdin JSON payload in, admitted or refused out."),
        "spawn-overlay" => Some("fno agent overlay - Harness-keyed spawn-defaults resolver over a stdin payload."),
        "test-run" => Some("fno agent test-run - The native test-suite process-group owner."),
        "transcript" => Some("fno agent registry transcript - Send or receive a session-transcript bundle between machines."),
        "verify-evidence" => Some("fno agent verify-evidence - Verify child-promise event evidence."),
        "worked-nodes" => Some("fno agent worked-nodes - The worked-overlay join over a stdin rows payload."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    /// the + form the node shipped shows in the one line a user reads
    #[test]
    fn the_spawn_usage_line_names_the_plus_form() {
        let line = command_usage("spawn").expect("spawn has a usage line");
        assert!(
            line.contains("+flag=<value>"),
            "the usage line names the + form: {line}"
        );
        assert!(
            line.contains("nothing folds after the fence"),
            "the usage line states the byte-exact payload rule: {line}"
        );
    }

    /// every flag loop-check's parser accepts appears in its help
    /// body. This is the gate that retires the class: --driver was
    /// load-bearing, accepted, and absent from the one place a user can
    /// look, and nothing caught that. The gate reads parse_args's own
    /// source, so a new flag cannot land undocumented.
    #[test]
    fn loop_check_help_covers_every_accepted_flag() {
        let source = include_str!("../gate/loopcheck/args.rs");
        let start = source
            .find("fn parse_args(")
            .expect("parse_args not found in loopcheck/args.rs");
        let body = &source[start..];
        let body = &body[..body.find("\n}\n").expect("parse_args has no closing brace")];

        let flag = Regex::new(r#"try_flag_value\(arg, "(--[\w-]+)"|arg == "(--[\w-]+)""#).unwrap();
        let mut flags: Vec<&str> = Vec::new();
        for caps in flag.captures_iter(body) {
            let f = caps
                .get(1)
                .or_else(|| caps.get(2))
                .expect("one alternation matched")
                .as_str();
            if !flags.contains(&f) {
                flags.push(f);
            }
        }
        // Positive control: the extraction must find the known flags, so a
        // broken scan fails loudly instead of passing on an empty set.
        assert!(
            flags.contains(&"--state"),
            "scan missed --state; extraction is broken: {flags:?}"
        );
        assert!(
            flags.contains(&"--driver"),
            "scan missed --driver; extraction is broken: {flags:?}"
        );
        assert!(
            flags.len() >= 16,
            "unexpectedly few flags scanned: {flags:?}"
        );

        for f in &flags {
            assert!(
                LOOP_CHECK_USAGE.contains(f),
                "loop-check accepts {f} but LOOP_CHECK_USAGE never names it"
            );
        }
        // The contract words the node asked for, beyond the flag spellings.
        // Whitespace-normalized: the const line-wraps its prose.
        let one_line: String = LOOP_CHECK_USAGE
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(command_help("loop-check").is_some());
        assert!(one_line.contains("target (the default)"));
        assert!(one_line.contains("target manifest"));
        assert!(one_line.contains("lead manifest"));
    }
}
