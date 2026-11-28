//! The `fno agent` menu: twelve entries, each holding at most twelve commands,
//! and nothing hidden except a retired spelling: it still answers with its
//! replacement notice, but no help list shows it. A nested spelling is
//! rewritten lexically to the argv
//! that answers today, so no command's behavior, flags or output change. An old
//! flat spelling still answers for one release: it prints one stderr line
//! naming the nested form, then runs as before (stderr, because callers
//! parse stdout). `fno doctor lint menu-caps` counts this table as the
//! `agents` group.

use std::ffi::OsString;

/// One entry, group or command, of the agents menu.
#[derive(Debug)]
pub struct Node {
    pub name: &'static str,
    pub about: &'static str,
    /// The words after `agents` this node forwards to. Empty for a group
    /// only this table defines.
    pub legacy: &'static [&'static str],
    /// Older spellings (words after `agents`) that still answer, with the
    /// notice, for one release.
    pub old: &'static [&'static [&'static str]],
    pub children: &'static [Node],
    /// A group whose bare or flag-led spelling forwards its legacy argv.
    pub bare: bool,
    /// A group whose unknown word refuses here. Otherwise the word goes on
    /// to the doors that owned the group before this table.
    pub strict: bool,
    /// `org chart rundown`: add the default `--out` when the caller gave none.
    pub pages_out: bool,
    /// A retired spelling: it answers with its replacement notice, but no
    /// help list shows it.
    pub retired: bool,
}

const fn command(name: &'static str, legacy: &'static [&'static str], about: &'static str) -> Node {
    Node {
        name,
        about,
        legacy,
        old: &[],
        children: &[],
        bare: false,
        strict: true,
        pages_out: false,
        retired: false,
    }
}

/// A retired spelling that still answers with its replacement notice for one
/// release, and that no help list shows. The about line stays the tombstone
/// answer itself: the node's own `--help` forward and the refusal carry it.
const fn retired(name: &'static str, legacy: &'static [&'static str], about: &'static str) -> Node {
    Node {
        retired: true,
        ..command(name, legacy, about)
    }
}

const fn moved(
    name: &'static str,
    legacy: &'static [&'static str],
    old: &'static [&'static [&'static str]],
    about: &'static str,
) -> Node {
    Node {
        old,
        ..command(name, legacy, about)
    }
}

/// A group this table defines: an unknown word refuses.
const fn group(name: &'static str, about: &'static str, children: &'static [Node]) -> Node {
    Node {
        children,
        ..command(name, &[], about)
    }
}

/// A group that existed before this table, under the same name: an unknown
/// word goes on to its old doors.
const fn kept(
    name: &'static str,
    legacy: &'static [&'static str],
    about: &'static str,
    children: &'static [Node],
) -> Node {
    Node {
        children,
        strict: false,
        ..command(name, legacy, about)
    }
}

#[rustfmt::skip]
const ADMIT: &[Node] = &[
    command("gate", &["spawn-gate"], "The one spawn gate: stdin JSON payload in, admitted or refused out."),
    command("guard", &["spawn-guard"], "The shared bg-dispatch guard and its mutex."),
    command("overlay", &["spawn-overlay"], "Harness-keyed spawn-defaults resolver over a stdin payload."),
    command("axes", &["spawn-axes"], "Spawn billing axes (route, account, model) over a stdin payload."),
    command("status", &["gate-status"], "The spawn gate's read-only capacity verdict as JSON."),
    command("revival", &["revival-check"], "Whether a spawn --resume revives an existing row."),
    command("fallback", &["fallback-chain"], "Failover chain walk over a stdin payload."),
    command("slot", &["route-slot"], "Delivery-slot resolver over a stdin payload."),
    command("provider-cap", &["provider-cap"], "The provider-cap actor."),
    command("capabilities", &["capabilities"], "One harness's config-independent capability contract."),
    command("liveness", &["liveness"], "One 'is this worker alive' answer per handle."),
    command("autonomy", &["autonomy"], "Every path that can start a session without an operator."),
];

#[rustfmt::skip]
const MAIL_ADMIN: &[Node] = &[
    command("drain-self", &["mail", "drain-self"], "Drain this session's own cross-harness inbox."),
    command("hold", &["mail", "hold"], "Busy mode: hold incoming mail and drain it on a timer."),
    command("hold-release", &["mail", "hold-release"], "Sleep until a hold expires, then release it."),
    command("pane-prepare", &["mail", "pane-prepare"], "The pane-send renderer in the fno-agent binary."),
    command("seed-provenance", &["mail", "seed-provenance"], "The seed-provenance command in the fno-agent binary."),
    command("sent", &["mail", "sent"], "List mail this session sent, with claim state."),
    command("team", &["mail", "team"], "Announce one body to a fleet scope as one bus line."),
    command("withdraw", &["mail", "withdraw"], "Retract a message the recipient has not picked up."),
    command("backfill", &["mail", "backfill"], "Import past cross-session mail from transcripts into the bus log; dry run by default, --apply writes."),
    command("teach", &["mail-teach"], "The fmail read-command lesson state."),
    command("mcp", &["mcp"], "MCP sidecar client commands."),
];

#[rustfmt::skip]
const MAIL: &[Node] = &[
    command("send", &["mail", "send"], "Send a message to a registered agent or a project."),
    command("reply", &["mail", "reply"], "Reply to a message, routed by the answered message's lane."),
    command("unread", &["mail", "unread"], "Show unread bus messages addressed to a name."),
    command("show", &["mail", "show"], "Print one message."),
    command("ack", &["mail", "ack"], "Advance a read cursor to a message id."),
    retired("list", &["mail", "list"], "Retired: prints its replacement, unread."),
    retired("drain", &["mail", "drain"], "Retired: prints its replacement, drain-self."),
    retired("status", &["mail", "status"], "Retired: prints its replacement, unread."),
    retired("triage", &["mail", "triage"], "Retired: prints its replacement, show."),
    command("ask", &["ask"], "Send a follow-up message to a registered agent."),
    command("announce", &["announce"], "Fleet announcements on one scope."),
    group("admin", "Mailbox upkeep: holds, sends, retractions, lessons.", MAIL_ADMIN),
];

#[rustfmt::skip]
const ORG_CHART: &[Node] = &[
    moved("fold", &["org-fold"], &[&["org", "fold"]], "The role scope fold."),
    moved("vacancies", &["org-vacancies"], &[&["org", "vacancies"]], "Roles whose holder is gone."),
    moved("shape", &["lead", "shape"], &[&["org", "shape"]], "Declare this term's shape: a pure pass, or a team."),
    moved("style", &["lead", "style"], &[&["org", "style"]], "List, read or pick this role's SOUL.md style."),
    moved("ledger", &["lead", "ledger"], &[&["org", "ledger"]], "Render the term ledger page."),
    Node {
        pages_out: true,
        ..moved(
            "rundown",
            &["lead", "ledger"],
            &[&["org", "rundown"]],
            "The per-team rundown page.",
        )
    },
    moved("manifest-path", &["lead", "manifest-path"], &[&["org", "manifest-path"]], "This promoted session's scope manifest path."),
    moved("drain", &["lead", "drain"], &[&["org", "drain"]], "How many scope nodes are not done."),
    moved("term", &["lead", "term"], &[&["org", "term"]], "Declare or extend this term."),
    command("checkin-raw", &["lead-checkin"], "The check-in body over explicit scope and event paths."),
    command("history-raw", &["lead-history"], "The check-in readback over explicit scope and event paths."),
    command("rundown-raw", &["lead-rundown"], "The rundown page renderer over explicit inputs."),
];

#[rustfmt::skip]
const ORG: &[Node] = &[
    command("init", &["lead", "init"], "Write the role manifest (step 1 of the lead skill)."),
    command("checkin", &["lead", "checkin"], "One lead beat: gather, print, diff, journal."),
    command("refresh-canon", &["lead", "refresh-canon"], "Refresh the beat's canon doc, resolving the plugin root itself."),
    command("done", &["lead", "done"], "Expire this role: vacate the row and clear the manifest."),
    command("cancel", &["lead", "cancel"], "Set or clear the cancel signal beside a role manifest."),
    command("escalate", &["lead", "escalate"], "Escalate to the presiding role with work pending."),
    command("verdict", &["lead", "verdict"], "Read the term tenure verdict."),
    command("history", &["lead", "history"], "This role's recorded term, newest first."),
    command("promote", &["role"], "Grant a role to a live session: <session> --scope <scope>."),
    command("faq", &["lead", "faq"], "Durable answers a promoted session leaves behind."),
    command("roles", &["roles"], "Bounded business-role definitions and resolutions."),
    group("chart", "The HQ chart's folds, pages and term records.", ORG_CHART),
];

#[rustfmt::skip]
const DISTRESS: &[Node] = &[
    command("scan", &["distress-scan"], "Read a transcript for a <help> tag; record a hit."),
    command("verdicts", &["distress-verdicts"], "Print each session's distress verdict."),
];

#[rustfmt::skip]
const LOOP: &[Node] = &[
    command("run", &["loop", "run"], "The unified driver loop: run --driver target."),
    command("check", &["loop-check"], "The stop-hook decision over external truth (read-only)."),
    command("consult", &["loop", "consult"], "One bounded stronger-model question from this session's transcript."),
    command("control", &["loops"], "Every scheduled loop: the arms table, pause-all, resume-all."),
    command("finalize", &["finalize"], "The terminal-only side-effect writer."),
    command("probe", &["probe-run"], "Evaluate a plan's named probe list."),
    command("kill-check", &["kill-check"], "Evaluate a plan's kill_criteria."),
    command("verify-evidence", &["verify-evidence"], "Verify child-promise event evidence."),
    command("prove-it", &["prove-it-verdicts"], "Read terminal prove-it records."),
    command("review-coverage", &["review-coverage"], "Emit the review_coverage event for a PR."),
    command("judge", &["judge"], "Grade a plan against the five product questions."),
    group("distress", "The <help> tag scan and its verdicts.", DISTRESS),
    command("attest", &["attest"], "The review-attestation carrier over the shared primary."),
];

#[rustfmt::skip]
const FLEET: &[Node] = &[
    command("census", &["census"], "One JSON row per long-lived process, with its build drift."),
    command("top", &["top"], "Every live worker process, fno-spawned and foreign."),
    command("orphans", &["orphans"], "Processes that outlived whatever started them."),
    command("sweep", &["sweep"], "Workers whose transcripts went quiet past their deadline."),
    command("watchdog", &["watchdog"], "Unfinished work: started nodes nobody holds."),
    command("truth", &["truth"], "A worker's supervision state from its transcript tail."),
    command("trace", &["trace"], "An agent's dispatch lifecycle from the event store."),
    command("history", &["history"], "Each matching session's whole story."),
    command("digest", &["digest"], "The catch-up fold over events and ledger for a session."),
    command("feed", &["feed"], "The activity feed over questions and the graph."),
    command("needs", &["needs"], "The needs-me queue across all sessions."),
    command("pane-identity", &["pane-identity"], "Cross-check mux panes against registry rows."),
];

#[rustfmt::skip]
const AUDIT: &[Node] = &[
    command("bash-census", &["bash-census"], "Bash-call shares and top command tables over transcripts."),
    command("honesty-sweep", &["honesty-sweep"], "Declared-vs-measured sweep over declared populations."),
    command("evals-macro", &["evals-macro"], "The macro-eval failure-pattern leaderboard."),
    command("drive-authority", &["drive-authority"], "Whether an operator holds a gate-hardening drive window."),
    command("stale-escalate", &["stale-escalate"], "Reconcile the stale-row question to the measured fleet."),
    command("friction-escalate", &["friction-escalate"], "Reconcile one watchdog-friction question to the fleet."),
    moved("incident", &["incident"], &[&["fleet-incident"]], "The durable fleet incident breaker."),
    command("test-run", &["test-run"], "The native test-suite process-group owner."),
    moved("restart", &["restart"], &[&["session", "restart"]], "Restart running fno processes onto fresh binaries."),
    command("harness", &["harness"], "Instruments over the harness capability table."),
    command(
        "claude-fallback",
        &["claude-fallback"],
        "Count claude sessions on node worktrees in a window, for the daily gate's no-fallback part.",
    ),
];

#[rustfmt::skip]
const SESSION: &[Node] = &[
    command("list", &["list"], "List registered agents."),
    command("logs", &["logs"], "Tail or follow an agent's log output."),
    command("attach", &["attach"], "Attach to a running agent session."),
    command("peek", &["peek"], "Observe a peer read-only."),
    command("watch", &["watch"], "Observe a held stream-json thread's turns live."),
    command("wait", &["wait"], "Block until an agent's row reaches a state."),
    command("stop", &["stop"], "Stop an agent's underlying session."),
    command("rm", &["rm"], "Remove an agent: its session first, its registry row after."),
    command("resume", &["resume"], "Resume a registered agent."),
    command("recover", &["recover"], "Restore a recorded claude session under its account."),
    command("move", &["move"], "Move a session to another machine with its transcript."),
    command("status", &["status"], "Daemon liveness and the control-plane arms table."),
];

#[rustfmt::skip]
const REGISTRY: &[Node] = &[
    command("adopt", &["adopt"], "Register an orphaned session by its session id."),
    command("register", &["register"], "Join this session to the mesh roster."),
    command("rename", &["rename"], "Rename a registry row's label."),
    command("name", &["name"], "The bridge to the canonical agent-name owner."),
    command("retask", &["retask"], "Retask one finished worker onto the node's next command."),
    command("reap", &["reap"], "Retire finished agent rows."),
    command("reconcile", &["reconcile"], "Sync registry status with provider reality."),
    command("report", &["report"], "The inside-leg state push the per-turn hook calls."),
    command("whoami", &["whoami"], "This mesh worker's own registered name."),
    command("compaction", &["compaction"], "Compaction stamps."),
    command("transcript", &["transcript"], "Send or receive a session-transcript bundle between machines."),
    command("subscribe", &["subscribe"], "Stream registry transitions and pane exits as NDJSON."),
];

#[rustfmt::skip]
const CLAIM: &[Node] = &[
    command("acquire", &["claim", "acquire"], "Acquire a claim on KEY for HOLDER."),
    command("list", &["claim", "list"], "Enumerate claims under the claims directory."),
    command("reap", &["claim", "reap"], "Archive every provably-dead claim."),
    command("main-red", &["claim", "main-red"], "Claim one repo's red main for repair; the first fixer takes it."),
    command("refresh", &["claim", "refresh"], "Extend a TTL claim's expires_at."),
    command("release", &["claim", "release"], "Release a claim this holder owns."),
    command("session-pid", &["claim", "session-pid"], "Resolve the durable session pid."),
    command("status", &["claim", "status"], "Inspect a single claim."),
    command("backlog", &["claim", "backlog"], "The shared-store backlog claims."),
    command("export", &["claim", "export"], "Export the shared claim store."),
];

#[rustfmt::skip]
const REMOTE: &[Node] = &[
    command("serve", &["remote", "serve"], "Run the tailnet door on loopback, or install its launchd agent."),
    command("pair", &["remote", "pair"], "Print a one-time pairing offer (URL and QR) for a phone."),
    command("devices", &["remote", "devices"], "List paired devices."),
    command("revoke", &["remote", "revoke"], "Revoke a paired device by id."),
    command("status", &["remote", "status"], "The running door's status: bind, tailnet name, last beat."),
];

#[rustfmt::skip]
const WORKSPACE: &[Node] = &[
    command("register-worker", &["workspace", "register-worker"], "Register a worker in the workers registry."),
    command("reap", &["workspace", "reap"], "Age-reap expendable state files."),
    command("worktree", &["workspace", "worktree"], "Worktree lifecycle: ensure, status, cleanup, archive."),
    command("worker", &["worker"], "Delivery worker phases."),
    group("remote", "The tailnet door a paired phone reads the fleet through.", REMOTE),
];

#[rustfmt::skip]
const INTERNAL: &[Node] = &[
    group("registry", "Registry rows and the session records the daemon and mux write.", INTERNAL_REGISTRY),
    group("session", "Session discovery, birth, resume and wake plumbing.", INTERNAL_SESSION),
    group("mail", "The mail transport doors hooks and the mux call.", INTERNAL_MAIL),
    group("pr", "PR gates, the GitHub transport and the merge operation.", INTERNAL_PR),
    group("target", "Target, plan, wave and task-context doors.", INTERNAL_TARGET),
    group("backlog", "Backlog reads and writes outside the backlog CLI.", INTERNAL_BACKLOG),
    group("harness", "Harness capability reads, the hook fire and plugin installs.", INTERNAL_HARNESS),
    group("evals", "Evals, intel, scoreboards and release records.", INTERNAL_EVALS),
    group("names", "Name, term and lead-shape parsers.", INTERNAL_NAMES),
    group("worktree", "Worktree and launch-directory doors.", INTERNAL_WORKTREE),
    group("fleet", "Fleet sweeps, reaps, scratch space and remote hand-offs.", INTERNAL_FLEET),
];

#[rustfmt::skip]
const INTERNAL_REGISTRY: &[Node] = &[
    moved("json", &["registry-json"], &[&["internal", "registry-json"]], "Registry rows as JSON, daemon-free."),
    moved("repair", &["registry-repair"], &[&["internal", "registry-repair"]], "Roll a poisoned registry's schema_version back."),
    moved("heal-token", &["heal-token"], &[&["internal", "heal-token"]], "Adopt the session a token names, as JSON."),
    command("commit", &["registry-commit"], "Commit one registry row write."),
    command("pending-row", &["pending-session-row"], "The pending session row a spawn writes before birth."),
    command("pane-rebind", &["pane-rebind"], "The workspace restore's registry rebind."),
];

#[rustfmt::skip]
const INTERNAL_SESSION: &[Node] = &[
    moved("discovered-json", &["discovered-json"], &[&["internal", "discovered-json"]], "The discovered-live-sessions lane as JSON."),
    moved("codex-for-pid", &["codex-session-for-pid"], &[&["internal", "codex-session-for-pid"]], "A codex pane's session id from its rollout."),
    moved("start-bytes", &["session-start-bytes"], &[&["internal", "session-start-bytes"]], "The session-start preamble byte total."),
    command("backfill", &["session-backfill"], "Fill a session row's missing fields."),
    command("claude-birth-exec", &["claude-birth-exec"], "Exec a claude birth under the supervisor."),
    command("codex-assign-project", &["codex-assign-project"], "Bind a codex session to its project."),
    command("manifest", &["manifest-for-session"], "The target manifest a session owns."),
    command("resume-argv", &["resume-argv"], "The harness resume argv for one session."),
    command("resume-receipt", &["resume-receipt"], "The resume receipt's argv door."),
    command("reentry-plan", &["reentry-plan"], "The re-entry plan for a restored session."),
    command("worker-wake", &["worker-wake"], "Wake one parked worker."),
    command("wake-context", &["wake-context"], "The context a bare resume word wakes with."),
    command("context-run", &["context-run"], "The context-meter probe and session cost read."),
];

#[rustfmt::skip]
const INTERNAL_MAIL: &[Node] = &[
    command("envelope", &["mail-envelope"], "Wrap a payload in the mail envelope."),
    command("notify-self", &["mail-notify-self"], "The inject-mail-notify hook's self notice."),
    command("inject", &["mail-inject"], "Inject one message into a live pane."),
    command("receipt", &["mail-receipt"], "A delivery receipt read."),
    command("record", &["mail-record"], "Record one mail row."),
    command("backfill", &["mail-backfill"], "Backfill mail rows into the record plane."),
    command("hold", &["mail-hold"], "The mux keystroke arm's mail hold."),
    command("threads", &["mail-threads"], "The Messages tab's thread read model."),
    command("machine-send", &["machine-mail-send"], "Send machine mail as the daemon."),
    command("notify-watch", &["notify-watch"], "The operator-notice sampler."),
    command("chats", &["chats"], "The conversation record plane."),
    command("remote", &["mail-remote"], "Mail to a session on another host."),
];

#[rustfmt::skip]
const INTERNAL_PR: &[Node] = &[
    moved("authorized-merge", &["authorized-merge"], &[&["internal", "authorized-merge"]], "The one authorized merge operation over stdin JSON."),
    command("gh", &["gh"], "The gh proxy the gh shim execs."),
    command("gh-adapter", &["gh-adapter"], "The fixed-purpose gh read: coverage or discretionary."),
    command("gh-cache", &["gh-cache"], "Permanent GitHub-fact rows."),
    command("canonical-check", &["canonical-check"], "The canonical-sync divergence read."),
    command("body-check", &["pr-body-check"], "Run the repo's PR-body guards on a body file."),
    command("cite-check", &["pr-cite-check"], "Refuse an internal D number a customers-scope body cites."),
    command("closure-parse", &["pr-closure-parse"], "Parse a PR body's closure line."),
    command("closure-render", &["pr-closure-render"], "Render a node's closure line."),
    command("park", &["pr-park"], "Park a PR watch."),
    command("watch", &["pr-watch"], "The PR-state watcher family."),
    command("worktree", &["pr-worktree"], "The local checkout for a PR head branch."),
    command("base-check", &["pr-base-check"], "The stale-base age guard."),
    command("base-lineage-check", &["pr-base-lineage-check"], "Refuse a base that no longer leads to the default branch."),
    command("merge-result-check", &["pr-merge-result-check"], "Compile a PR's merge result against its base."),
    command("verify", &["pr-verify"], "Verify an external PR gate: merged or reviews."),
    command("create", &["pr-create"], "The duplicate-guarded PR create."),
    command("heal", &["pr-heal"], "Classify a red check and apply its fix."),
    command("push", &["pr-push"], "The one guarded push."),
    command("rebase", &["pr-rebase"], "The conflict-delegation rebase."),
    command("publish-review", &["publish-review"], "Post a review verdict as the reviewer identity."),
    command("sync-canonical", &["sync-canonical"], "The post-merge canonical-checkout sync."),
    command("review-summary", &["review-summary"], "The one display line for a reviewed head."),
    command("review-start", &["review-start"], "Open a review round."),
    command("review-level", &["review-level"], "Resolve a review level and its findings."),
    command("review-policy", &["review-policy"], "The review assurance verdict."),
];

#[rustfmt::skip]
const INTERNAL_TARGET: &[Node] = &[
    moved("family", &["target-family"], &[&["internal", "target-family"]], "The merge-posture family test for a message."),
    moved("permission-tokens", &["permission-tokens"], &[&["internal", "permission-tokens"]], "The permission-posture owner over a stdin payload."),
    moved("sandbox-probe", &["sandbox-probe"], &[&["internal", "sandbox-probe"]], "The codex sandbox verdict over a stdin payload."),
    command("gates", &["target-gates"], "The target gate commands."),
    command("status", &["target-status"], "The target orientation report."),
    command("init", &["target-init"], "Write a target manifest and claim its node."),
    command("start", &["target-start"], "Cold-start a target worktree."),
    command("blast-check", &["target-blast-check"], "The blast-radius classifier for target init."),
    command("state", &["state"], "Session state reads and writes."),
    command("state-cli", &["state-cli"], "The state family's CLI."),
    command("stub-manifest", &["stub-manifest"], "The stub-manifest surface."),
    command("plan-cli", &["plan-cli"], "The plan document CLI."),
    command("executor-route", &["executor-route"], "Resolve a task's executor."),
    command("surface-check", &["surface-check"], "The plan surface check validate-plan.sh reads."),
    command("style-check", &["style-check"], "The house style check."),
    command("wave", &["wave"], "The wave dispatcher."),
    command("evidence-gate", &["evidence-gate"], "The evidence gate."),
    command("test-delta", &["test-delta"], "Net test declarations against a base."),
    command("think-inspect", &["think-inspect"], "The read-only design-discovery receipt."),
    command("think-dispatch", &["think-dispatch"], "Hand a node to a background think."),
    command("research", &["research"], "The scout backbone."),
    command("visual", &["visual"], "The visual door."),
    command("delivery-evaluate", &["delivery-evaluate"], "The native delivery evaluator."),
    command("task-context-gate", &["task-context-gate"], "The task-context execution gate."),
    command("task-context-payload", &["task-context-payload"], "The task-context payload."),
    command("task-context-prepare", &["task-context-prepare"], "Prepare a task context."),
    command("task-context-revalidate", &["task-context-revalidate"], "Revalidate a task context."),
    command("task-context-show", &["task-context-show"], "Show a task context."),
    command("task-context-stage", &["task-context-stage"], "Stage a task context."),
];

#[rustfmt::skip]
const INTERNAL_BACKLOG: &[Node] = &[
    moved("select-read", &["select-read"], &[&["internal", "select-read"]], "One bounded backlog read under the select timeout."),
    moved("worked-nodes", &["worked-nodes"], &[&["internal", "worked-nodes"]], "The worked-overlay join over a stdin rows payload."),
    command("orphan-plans", &["backlog-orphan-plans"], "The SessionStart reconcile sweep."),
    command("active-receipt", &["active-backlog-receipt"], "The active backlog receipt."),
    command("node-bind", &["node-bind"], "The first-turn node bind."),
    command("node-origin", &["node-origin"], "A node's origin read."),
    command("node-route", &["node-route"], "The cascade verdict per name."),
    command("question-intake", &["question-intake"], "Take in a filed question."),
    command("question-clear", &["question-clear"], "Clear an answered question."),
    command("territory-rows", &["territory-rows"], "The territory fact set's reads."),
    command("territory-verdict", &["territory-verdict"], "The territory verdict."),
    command("board", &["board"], "The lead board collector."),
    command("day", &["day"], "The day read."),
];

#[rustfmt::skip]
const INTERNAL_HARNESS: &[Node] = &[
    command("probe", &["harness-probe"], "Harness capability probe."),
    command("matrix", &["harness-matrix"], "The harness capability matrix."),
    command("beat", &["harness-beat"], "The harness beat read."),
    command("roster", &["harness-roster"], "The one-roster JSON read."),
    command("terminals", &["terminals"], "The delivered-terminal vocabulary."),
    command("user-hours", &["user-hours"], "The [user.working_hours] read."),
    command("state-root", &["state-root"], "The state-root seal handshake."),
    command("hook", &["hook"], "Fire one hook."),
    command("commands", &["commands"], "The harness command list."),
    command("plugin-install", &["plugin-install"], "Install the plugin for one harness."),
    command("detect", &["detect"], "Detect a pane's prompt state."),
];

#[rustfmt::skip]
const INTERNAL_EVALS: &[Node] = &[
    command("evals", &["evals"], "The evals family."),
    command("arm", &["evals-arm"], "The evals arm."),
    command("attempt", &["evals-attempt"], "The attempt-eligibility verdict."),
    command("trend", &["evals-trend"], "The evals trend fold."),
    command("intel", &["intel"], "The session-provenance fold."),
    command("scoreboard", &["scoreboard"], "The telemetry scoreboard."),
    command("manifest-eval", &["manifest-eval"], "Evaluate a manifest."),
    command("component-verdict", &["component-verdict"], "One component's install verdict."),
    command("release-notes", &["release-notes"], "The update modal's release notes."),
    command("update-journal", &["update-journal"], "The update lifecycle's journal."),
    command("source-pin", &["source-pin"], "The source pin read."),
    command("stalls", &["stalls"], "The stall-cause table."),
];

#[rustfmt::skip]
const INTERNAL_NAMES: &[Node] = &[
    command("codes", &["name-codes"], "The name code table."),
    command("mint", &["name-mint"], "Mint a name."),
    command("parse", &["name-parse"], "Parse a name."),
    command("term-shape", &["term-shape"], "A term's shape."),
    command("term-state", &["term-state"], "A term's state."),
    command("lead-shape", &["lead-shape"], "A lead's shape."),
    command("lead-state", &["lead-state"], "A lead's state."),
    command("lead-escalation-text", &["lead-escalation-text"], "A lead escalation's text."),
];

#[rustfmt::skip]
const INTERNAL_WORKTREE: &[Node] = &[
    command("policy", &["worktree-policy"], "The worktree policy read."),
    command("ensure", &["worktree-ensure"], "Ensure a worktree."),
    command("status", &["worktree-status"], "One worktree's status."),
    command("list", &["worktree-list"], "List worktrees."),
    command("stranded", &["worktree-stranded"], "Stranded worktrees."),
    command("resolve-node", &["worktree-resolve-node"], "The node a worktree serves."),
    command("overlap-record", &["worktree-overlap-record"], "Record a worktree overlap."),
    command("overlaps", &["worktree-overlaps"], "The worktree overlap report."),
    command("register-worker", &["workspace-register-worker"], "Register a worker in the workers registry."),
    command("dispatch-pin", &["worktree-dispatch-pin"], "The dispatch pin."),
    command("reapable", &["worktree-reapable"], "Is removing this worktree safe."),
    command("launch-workdir", &["launch-workdir"], "Ensure a launched worker's directory."),
];

#[rustfmt::skip]
const INTERNAL_FLEET: &[Node] = &[
    command("fleet-task", &["fleet-task"], "One fleet task."),
    command("orphan-reap", &["orphan-reap"], "Reap orphaned processes."),
    command("roster-reap", &["roster-reap"], "Reap claude rows no fno row names."),
    command("reclaim", &["reclaim"], "The machine janitor."),
    command("scratch", &["scratch"], "Scratch space."),
    command("org-remote", &["org-remote"], "Promote a role on another host."),
    command("spawn-remote", &["spawn-remote"], "Spawn on another host."),
];

#[rustfmt::skip]
const ENTRIES: &[Node] = &[
    command("spawn", &["spawn"], "Spawn a new agent; `mail ask` is the follow-up lane."),
    group("admit", "Spawn admission: gate, guard, overlays, axes, slot.", ADMIT),
    kept("mail", &["mail"], "Durable polled mailbox: send, reply, unread, ack, drain.", MAIL),
    Node {
        bare: true,
        strict: true,
        old: &[&["org"]],
        ..kept(
            "hq",
            &["team"],
            "The HQ chart of roles and their lifecycle.",
            ORG,
        )
    },
    group("loop", "The driver loop, its stop check, and the verdicts it reads.", LOOP),
    group("fleet", "Fleet reads: processes, workers, transcripts, feeds.", FLEET),
    group("audit", "Sweeps, censuses and the incident breaker.", AUDIT),
    group("session", "One agent's session: list, logs, attach, stop, resume.", SESSION),
    group("registry", "Registry rows: register, name, reap, reconcile, report.", REGISTRY),
    kept("claim", &["claim"], "Work-claim coordination primitive.", CLAIM),
    kept("workspace", &["workspace"], "Worktree lifecycle, worker registration, the phone door.", WORKSPACE),
    group("internal", "Stdin-JSON commands a hook or the daemon calls.", INTERNAL),
];

/// The whole `agents` group as one node.
pub static ROOT: Node = Node {
    strict: false,
    ..group(
        "agents",
        "sessions, mail, roles and the loop that drives them",
        ENTRIES,
    )
};

/// What the front door does with an invocation this module claims.
#[derive(Debug, PartialEq, Eq)]
pub enum Agents {
    /// Go on through the front door with this argv.
    Run(Vec<OsString>),
    /// Print this help on stdout, exit 0.
    Help(String),
    /// Print this refusal on stderr, exit 2.
    Refuse(String),
}

fn os(text: &str) -> OsString {
    OsString::from(text)
}

/// `--out` for `org chart rundown` at the place-resolved rundown page when
/// the state root is faithful; None otherwise (the ledger then writes its
/// own default, and the web bridge writes nothing).
#[cfg(not(test))]
fn rundown_out_arg() -> Option<OsString> {
    let (root, faithful) = crate::org_root::lead_state_root();
    if !faithful {
        return None;
    }
    Some(OsString::from(
        crate::state_layout::place_new(&root, "rundown.html")
            .display()
            .to_string(),
    ))
}

#[cfg(test)]
fn rundown_out_arg() -> Option<OsString> {
    None
}

/// `agents` + the node's legacy words + the caller's tail, plus the rundown
/// page's default `--out` when the caller gave none.
fn forward(node: &Node, rest: &[OsString], default_out: Option<OsString>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![os("agents")];
    argv.extend(node.legacy.iter().map(|w| os(w)));
    argv.extend(rest.iter().cloned());
    if node.pages_out {
        let has_out = rest.iter().any(|a| {
            a.to_str()
                .map(|s| s == "--out" || s.starts_with("--out="))
                .unwrap_or(false)
        });
        if let (false, Some(path)) = (has_out, default_out) {
            argv.push(os("--out"));
            argv.push(path);
        }
    }
    argv
}

/// Every node under `node`, with its path of names below `agents`.
pub fn walk(node: &'static Node) -> Vec<(Vec<&'static str>, &'static Node)> {
    fn go(
        node: &'static Node,
        path: &mut Vec<&'static str>,
        out: &mut Vec<(Vec<&'static str>, &'static Node)>,
    ) {
        for child in node.children {
            path.push(child.name);
            out.push((path.clone(), child));
            go(child, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    go(node, &mut Vec::new(), &mut out);
    out
}

/// The spellings a node answered to before this table: its legacy words
/// when they differ from its path, and its listed older spellings. The
/// rundown page shares `lead ledger` with `org chart ledger`, which keeps it.
pub fn old_spellings(path: &[&str], node: &Node) -> Vec<&'static [&'static str]> {
    let mut spellings: Vec<&'static [&'static str]> = node.old.to_vec();
    if !node.legacy.is_empty() && node.legacy != path && !node.pages_out {
        spellings.push(node.legacy);
    }
    spellings
}

/// The node whose old spelling is the longest prefix of `words`.
fn old_hit(words: &[&str]) -> Option<(Vec<&'static str>, &'static Node, usize)> {
    let mut best: Option<(Vec<&'static str>, &'static Node, usize)> = None;
    for (path, node) in walk(&ROOT) {
        for spelling in old_spellings(&path, node) {
            let len = spelling.len();
            if words.len() >= len
                && words[..len] == *spelling
                && best.as_ref().map(|b| b.2 < len).unwrap_or(true)
            {
                best = Some((path.clone(), node, len));
            }
        }
    }
    best
}

fn spelled(words: &[&str]) -> String {
    if words.is_empty() {
        "fno agent".to_string()
    } else {
        format!("fno agent {}", words.join(" "))
    }
}

/// The menu of one group: every live child, one line each. A retired
/// spelling answers but no list shows it, so the footer says so when one
/// sits in this group.
pub fn menu(path: &[&str], node: &Node) -> String {
    let shown: Vec<&Node> = node.children.iter().filter(|c| !c.retired).collect();
    let width = shown.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let footer = if shown.len() == node.children.len() {
        "Nothing is hidden: this list is the whole group."
    } else {
        "Retired spellings still answer for one release; they are not listed."
    };
    let mut text = format!("{} - {}\n\n", spelled(path), node.about);
    for child in shown {
        let mark = if child.children.is_empty() {
            ""
        } else {
            " (group)"
        };
        text.push_str(&format!("  {:width$}  {}{mark}\n", child.name, child.about));
    }
    text.push_str(&format!(
        "\n{} <command> --help shows one command. {footer}\n",
        spelled(path)
    ));
    text
}

fn unknown(path: &[&str], node: &Node, word: &str) -> String {
    let names: Vec<&str> = node
        .children
        .iter()
        .filter(|c| !c.retired)
        .map(|c| c.name)
        .collect();
    format!(
        "{} {word}: unknown command. Commands: {}. `{} --help` shows them.",
        spelled(path),
        names.join(", "),
        spelled(path)
    )
}

/// Resolve the words after `agents`, returning the outcome and the one
/// stderr notice an old spelling prints. None hands the argv on unchanged.
fn resolve(args: &[OsString]) -> Option<(Agents, Option<String>)> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let mut node: &'static Node = &ROOT;
    let mut depth = 0usize;
    loop {
        let path = &words[..depth];
        let rest = &args[depth..];
        if node.children.is_empty() {
            return Some((Agents::Run(forward(node, rest, rundown_out_arg())), None));
        }
        match words.get(depth).copied() {
            None if node.bare => return Some((Agents::Run(forward(node, rest, None)), None)),
            None | Some("-h" | "--help") => return Some((Agents::Help(menu(path, node)), None)),
            Some(flag) if flag.starts_with('-') => {
                if node.bare {
                    return Some((Agents::Run(forward(node, rest, None)), None));
                }
                if node.strict {
                    return Some((Agents::Refuse(unknown(path, node, flag)), None));
                }
                return None;
            }
            Some(word) => {
                if let Some(child) = node.children.iter().find(|c| c.name == word) {
                    node = child;
                    depth += 1;
                    continue;
                }
                if let Some((new_path, hit, len)) = old_hit(&words) {
                    let notice = format!(
                        "{} is now {}; the old spelling goes after one release.",
                        spelled(&words[..len]),
                        spelled(&new_path)
                    );
                    // The rewritten prefix is the node's own path, so the
                    // tail re-walks the table exactly as the new spelling
                    // would: an old GROUP spelling composes with every
                    // newer sub-spelling. The direct legacy forward stays
                    // as the fallback for a tail the table does not claim.
                    let mut rewritten: Vec<OsString> = new_path.iter().map(|w| os(w)).collect();
                    rewritten.extend(args.iter().skip(len).cloned());
                    match resolve(&rewritten) {
                        Some((outcome, _)) => return Some((outcome, Some(notice))),
                        None => {
                            let argv = forward(hit, &args[len..], rundown_out_arg());
                            return Some((Agents::Run(argv), Some(notice)));
                        }
                    }
                }
                if node.strict {
                    return Some((Agents::Refuse(unknown(path, node, word)), None));
                }
                return None;
            }
        }
    }
}

/// `fno help agents [<entry>...] [--all]`: the same menus, claimed here
/// before the Python help command sees them. A command's own help forwards.
fn help(args: &[OsString]) -> Option<Agents> {
    let words: Vec<&str> = args
        .iter()
        .map(|a| a.to_str())
        .collect::<Option<Vec<&str>>>()?
        .into_iter()
        .filter(|w| !w.starts_with('-'))
        .collect();
    let mut node: &'static Node = &ROOT;
    let mut words = words;
    let mut depth = 0usize;
    while let Some(word) = words.get(depth).copied() {
        match node.children.iter().find(|c| c.name == word) {
            Some(child) => {
                node = child;
                depth += 1;
            }
            None => {
                // A deprecated spelling walks under its new path, so
                // `help agents org` reads as `help agents hq`.
                let hit = words.get(..=depth).and_then(|prefix| old_hit(prefix));
                match hit {
                    Some((new_path, _, len)) if len == depth + 1 => {
                        let mut rewritten: Vec<&str> = new_path.clone();
                        rewritten.extend(words.iter().skip(depth + 1).copied());
                        words = rewritten;
                        node = &ROOT;
                        depth = 0;
                    }
                    _ => {
                        let walked: Vec<&str> = words.iter().take(depth).copied().collect();
                        return Some(Agents::Refuse(unknown(&walked, node, word)));
                    }
                }
            }
        }
    }
    if node.children.is_empty() {
        let mut argv = forward(node, &[], None);
        argv.push(os("--help"));
        return Some(Agents::Run(argv));
    }
    Some(Agents::Help(menu(&words, node)))
}

/// The lexical claim on `fno agent ...` and `fno help agents ...`. Some
/// whenever this module decides the invocation; the notice an old spelling
/// owes is printed here, on stderr.
pub fn classify(args: &[OsString]) -> Option<Agents> {
    match (
        args.first().and_then(|a| a.to_str()),
        args.get(1).and_then(|a| a.to_str()),
    ) {
        (Some("help"), Some("agents")) => return help(&args[2..]),
        (Some("agents"), _) => {}
        _ => return None,
    }
    if let Some(command) = args.get(1).and_then(|a| a.to_str()) {
        if let Some(instead) = crate::role_migration::retired_verb(command) {
            return Some(Agents::Refuse(format!(
                "this role command is retired; use {instead}"
            )));
        }
        if command == "promote" {
            return Some(Agents::Refuse(
                "fno agent promote is now fno agent hq promote <session> --scope <scope>".into(),
            ));
        }
    }
    let (outcome, notice) = resolve(&args[1..])?;
    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
    Some(outcome)
}

/// The fno-agent argv prefix for each forwarded `agents <word>` the front
/// execs natively. The front execs from this table, and the Explorer tabs
/// resolve their public argv through it, so a leaf rename moves both.
const NATIVE_LEAF: &[(&str, &[&str])] = &[
    ("top", &["census", "--workers"]),
    ("org-fold", &["org-fold"]),
];

/// The leaf prefix for a natively exec'd forwarded word; empty for any other.
pub fn native_leaf(word: &str) -> &'static [&'static str] {
    NATIVE_LEAF
        .iter()
        .find(|(w, _)| *w == word)
        .map_or(&[], |(_, leaf)| leaf)
}

/// The fno-agent argv a public `fno agent ...` argv execs, when the front
/// execs it natively; None when the front would route it anywhere else.
pub fn native_leaf_argv(public: &[&str]) -> Option<Vec<OsString>> {
    let args: Vec<OsString> = public.iter().map(OsString::from).collect();
    let forwarded = match resolve(args.get(1..)?)? {
        (Agents::Run(argv), None) => argv,
        _ => return None,
    };
    let leaf = native_leaf(forwarded.get(1)?.to_str()?);
    if leaf.is_empty() {
        return None;
    }
    Some(
        leaf.iter()
            .map(OsString::from)
            .chain(forwarded[2..].iter().cloned())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osv(texts: &[&str]) -> Vec<OsString> {
        texts.iter().map(OsString::from).collect()
    }

    fn run(texts: &[&str]) -> Option<(Agents, Option<String>)> {
        resolve(&osv(texts))
    }
    #[test]
    fn agents_menu_nested_spellings_rewrite_and_old_ones_notice() {
        // A nested command forwards the argv that answers today, tail intact.
        assert_eq!(
            run(&["fleet", "census", "--json"]),
            Some((Agents::Run(osv(&["agents", "census", "--json"])), None))
        );
        assert_eq!(
            run(&["mail", "admin", "hold", "1h"]),
            Some((Agents::Run(osv(&["agents", "mail", "hold", "1h"])), None))
        );
        assert_eq!(
            run(&["claim", "release", "node:x"]),
            Some((
                Agents::Run(osv(&["agents", "claim", "release", "node:x"])),
                None
            ))
        );
        assert_eq!(
            run(&["spawn", "--json"]),
            Some((Agents::Run(osv(&["agents", "spawn", "--json"])), None))
        );
        // An old flat spelling still answers, with one notice naming the
        // nested form; a nested old spelling rewrites to its forward.
        for (old, argv, new) in [
            (
                &["loop-check", "-J"][..],
                &["agents", "loop-check", "-J"][..],
                "fno agent loop check",
            ),
            (
                &["mail", "hold"][..],
                &["agents", "mail", "hold"][..],
                "fno agent mail admin hold",
            ),
            (
                &["lead", "checkin"][..],
                &["agents", "lead", "checkin"][..],
                "fno agent hq checkin",
            ),
            (
                &["team", "-J"][..],
                &["agents", "team", "-J"][..],
                "fno agent hq",
            ),
            (
                &["org", "fold", "fno"][..],
                &["agents", "org-fold", "fno"][..],
                "fno agent hq chart fold",
            ),
            (
                &["fleet-incident"][..],
                &["agents", "incident"][..],
                "fno agent audit incident",
            ),
        ] {
            match run(old) {
                Some((Agents::Run(got), Some(notice))) => {
                    assert_eq!(got, osv(argv), "{old:?}");
                    assert!(notice.contains(new), "{notice}");
                    assert!(notice.contains("one release"), "{notice}");
                }
                other => panic!("{old:?}: expected a noticed Run, got {other:?}"),
            }
        }
        // The deprecated group spelling composes with every sub-spelling:
        // a leaf under `org` rewrites to the same argv as under `hq`, and
        // the notice names the group once.
        for (old, argv) in [
            (&["org", "checkin"][..], &["agents", "lead", "checkin"][..]),
            (
                &["org", "chart", "fold", "fno"][..],
                &["agents", "org-fold", "fno"][..],
            ),
            (
                &["org", "promote", "s", "--scope", "x"][..],
                &["agents", "role", "s", "--scope", "x"][..],
            ),
        ] {
            match run(old) {
                Some((Agents::Run(got), Some(notice))) => {
                    assert_eq!(got, osv(argv), "{old:?}");
                    assert!(notice.contains("fno agent hq"), "{notice}");
                }
                other => panic!("{old:?}: expected a noticed Run, got {other:?}"),
            }
        }
        // The hq group keeps its shapes: bare and flag-led forward the
        // team argv, rundown adds the default out only when absent.
        match run(&["hq"]) {
            Some((Agents::Run(argv), None)) => assert_eq!(argv, osv(&["agents", "team"])),
            other => panic!("bare hq: expected an unnoticed Run, got {other:?}"),
        }
        match run(&["hq", "-J"]) {
            Some((Agents::Run(argv), None)) => assert_eq!(argv, osv(&["agents", "team", "-J"])),
            other => panic!("flag-led hq: expected an unnoticed Run, got {other:?}"),
        }
        // The deprecated group spelling keeps those shapes too, noticed.
        match run(&["org"]) {
            Some((Agents::Run(argv), Some(notice))) => {
                assert_eq!(argv, osv(&["agents", "team"]));
                assert!(notice.contains("fno agent hq"), "{notice}");
            }
            other => panic!("bare org: expected a noticed Run, got {other:?}"),
        }
        // The canon-refresh row forwards to its lead-family door, spelled
        // either way.
        match run(&["hq", "refresh-canon"]) {
            Some((Agents::Run(argv), None)) => {
                assert_eq!(argv, osv(&["agents", "lead", "refresh-canon"]))
            }
            other => panic!("hq refresh-canon: expected an unnoticed Run, got {other:?}"),
        }
        match run(&["org", "refresh-canon"]) {
            Some((Agents::Run(argv), Some(notice))) => {
                assert_eq!(argv, osv(&["agents", "lead", "refresh-canon"]));
                assert!(notice.contains("fno agent hq"), "{notice}");
            }
            other => panic!("org refresh-canon: expected a noticed Run, got {other:?}"),
        }
        let rundown = walk(&ROOT)
            .into_iter()
            .find(|(p, _)| p == &["hq", "chart", "rundown"])
            .unwrap()
            .1;
        let out = Some(OsString::from("/s/pages/rundown.html"));
        assert_eq!(
            forward(rundown, &[], out.clone()),
            osv(&["agents", "lead", "ledger", "--out", "/s/pages/rundown.html"])
        );
        assert_eq!(
            forward(rundown, &osv(&["--out=/x.html"]), out),
            osv(&["agents", "lead", "ledger", "--out=/x.html"])
        );
        // An unknown command under a group this table defines refuses with one
        // line listing the group; under a kept group it goes on unchanged.
        match run(&["fleet", "nope"]) {
            Some((Agents::Refuse(line), None)) => {
                assert!(
                    line.contains("census") && line.contains("pane-identity"),
                    "{line}"
                );
                assert!(!line.contains('\n'), "{line}");
            }
            other => panic!("expected Refuse, got {other:?}"),
        }
        assert_eq!(run(&["mail", "read", "fmail-x"]), None);
        assert_eq!(run(&["no-such-command"]), None);
        // Menus are native and list every child.
        match run(&["--help"]) {
            Some((Agents::Help(text), None)) => {
                for entry in ENTRIES {
                    assert!(text.contains(entry.name), "{text}");
                }
            }
            other => panic!("expected Help, got {other:?}"),
        }
        assert!(matches!(run(&["session"]), Some((Agents::Help(_), None))));
        assert!(matches!(
            help(&osv(&["fleet", "--all"])),
            Some(Agents::Help(text)) if text.contains("watchdog")
        ));
        assert_eq!(
            help(&osv(&["fleet", "census"])),
            Some(Agents::Run(osv(&["agents", "census", "--help"])))
        );
        // A deprecated group spelling reads as its new path in help too.
        match help(&osv(&["org"])) {
            Some(Agents::Help(text)) => assert!(text.contains("hq - The HQ chart"), "{text}"),
            other => panic!("help org: expected the hq menu, got {other:?}"),
        }
        assert_eq!(classify(&osv(&["mux", "ls"])), None);
        assert!(matches!(
            classify(&osv(&["agents", "promote", "x"])),
            Some(Agents::Refuse(m)) if m.contains("fno agent hq promote")
        ));
    }

    #[test]
    fn agents_menu_holds_twelve_at_each_level_and_places_every_python_leaf() {
        assert!(ENTRIES.len() <= 12);
        // A group over 12 holds its seeded debt ceiling: the same relief the
        // menu-caps lint reads, and the ceiling may only shrink.
        let debt = include_str!("../../../../scripts/ci/menu-caps-debt.tsv");
        for (path, node) in walk(&ROOT) {
            if path.len() < 2 {
                let printed = format!(
                    "agent {}",
                    path.iter()
                        .map(|s| s.as_ref())
                        .collect::<Vec<&str>>()
                        .join(" ")
                );
                let cap = debt
                    .lines()
                    .filter(|l| !l.starts_with('#'))
                    .filter_map(|l| {
                        let mut cols = l.split('\t');
                        let name = cols.next()?;
                        if name == printed {
                            cols.next().and_then(|c| c.parse::<usize>().ok())
                        } else {
                            None
                        }
                    })
                    .next()
                    .unwrap_or(12);
                assert!(
                    node.children.len() <= cap,
                    "agents {path:?} holds {} (cap {cap})",
                    node.children.len()
                );
            }
        }
        // Every `agents` row of the collapse map resolves to exactly one
        // destination: the node whose path, forward, or old spelling is the
        // longest prefix of the row, or a deleted command's tombstone.
        let deleted = ["gate", "ping", "team-fold", "team-orphans", "term-ledger"];
        let map = include_str!("../../../../scripts/ci/command-collapse-map.tsv");
        let mut unplaced: Vec<String> = Vec::new();
        for line in map.lines().skip(1) {
            let leaf = line.split('\t').next().unwrap_or("");
            let words: Vec<&str> = leaf.split(' ').collect();
            if words.first() != Some(&"agents") || words.len() < 2 {
                continue;
            }
            let row = &words[1..];
            if deleted.contains(&row[0]) {
                continue;
            }
            let mut best = 0usize;
            let mut hits = 0usize;
            for (path, node) in walk(&ROOT) {
                let mut spellings = old_spellings(&path, node);
                spellings.push(&[]);
                for spelling in spellings {
                    let candidate: Vec<&str> = if spelling.is_empty() {
                        path.clone()
                    } else {
                        spelling.to_vec()
                    };
                    let len = candidate.len();
                    if row.len() >= len && row[..len] == candidate[..] {
                        if len > best {
                            best = len;
                            hits = 1;
                        } else if len == best {
                            hits += 1;
                        }
                    }
                }
            }
            if best == 0 || hits != 1 {
                unplaced.push(format!("{leaf} ({hits} destinations)"));
            }
        }
        assert!(
            unplaced.is_empty(),
            "agents rows with no single home: {unplaced:#?}"
        );
    }

    #[test]
    fn agents_menu_hides_retired_rows_but_the_tombstone_answers() {
        let node = |path: &[&str]| -> &'static Node {
            walk(&ROOT)
                .into_iter()
                .find(|(p, _)| p.as_slice() == path)
                .map(|(_, n)| n)
                .expect("path in table")
        };
        // The mail group's menu hides its retired rows and says so in the
        // footer; every other group's footer keeps the whole-group promise.
        let mail = menu(&["mail"], node(&["mail"]));
        for word in ["list", "drain", "status", "triage"] {
            assert!(!mail.contains(&format!("  {word} ")), "{mail}");
        }
        assert!(
            mail.contains("Retired spellings still answer for one release"),
            "{mail}"
        );
        let fleet = menu(&["fleet"], node(&["fleet"]));
        assert!(fleet.contains("Nothing is hidden"), "{fleet}");

        // The unknown-word refusal lists live commands only.
        let refused = unknown(&["mail"], node(&["mail"]), "lst");
        assert!(!refused.contains("list,"), "{refused}");
        assert!(refused.contains("send"), "{refused}");

        // The tombstone still answers: the resolve walk finds the retired
        // child and forwards its legacy argv, tail intact.
        assert_eq!(
            run(&["mail", "list"]),
            Some((Agents::Run(osv(&["agents", "mail", "list"])), None))
        );
        // `help agents mail` shows the menu, not the tombstone rows.
        match help(&osv(&["mail"])) {
            Some(Agents::Help(text)) => {
                assert!(!text.contains("Retired:"), "{text}");
            }
            other => panic!("help agents mail: expected Help, got {other:?}"),
        }
    }
}
