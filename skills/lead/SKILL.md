---
name: lead
description: Lead a backlog territory over time or run one governed planning wave.
argument-hint: "<scope> [--once]"
metadata:
  requires:
    harness:
      - loop
      - spawn
---

<!-- style-exception: monitor cadences and command spellings are load-bearing literals -->

# Lead

When `$CODEX_THREAD_ID` is nonblank, the Codex provider goal is the primary continuation receipt. It is separate from Footnote's `Stop` receipt: the goal proves the objective and continuation owner, while `Stop` proves the hook that drives Footnote's loop. Neither receipt substitutes for the other.

You are the tenured lead over `<scope>`. With `--once` you rule one wave and step down. Without it you stay. Your job is not to build. It is to keep the territory moving. Read indicators on a beat, pull levers, and escalate what a lever cannot fix. If parked is the honest state, park.

A user turn that tells the lead to stand down blocks the stop gate until it is acked. Answer it as a verdict on this lead, not as a general question.

## Who runs this

The role is granted, never inferred. Verify it before anything else:

1. Run `fno agents hq --json`.
2. If a row carries a role-source field, use it. If it does not, call the reader directly: resolve `term_state(scope)` (`fno agents hq chart manifest-path` resolves the same file) and print `ROLE-SOURCE: term_state (team field absent)`.
3. This session's handle must appear titled over `<scope>` (rung 0, 1 or 2). The manifest-versus-registry answer must read `split: false`. AND `conflicts` must carry no entry whose scope is `<scope>`. `agree` answers neither question ([why](references/once.md#splits-conflicts-and-a-missing-grant)).
4. Any split, conflict or unknown STOPS the skill and prints both session ids. The live manifest heals an unstamped row. A `conflicts` of null is an unknown. A lead cannot lead through a disputed role, beside a rival, or re-role itself.
5. Otherwise print that this session is not promoted. Mail your lead for the grant: `fno agents hq promote <handle> --scope <scope>`. Never hand that line to the user. A grant the user makes in chat, and the full steps: [once.md](references/once.md#splits-conflicts-and-a-missing-grant). Stop leading, and read [Without a role](#without-a-role).

How a role is granted, the ladder and succession: [the role model](references/once.md#who-runs-this-the-role-is-granted).

## Without a role

A session with no role over the scope does not lead. It runs no check-in, spawns no worker for the scope, and records no ruling. It can read, report with `fno backlog note <scope> "<what you saw>" --quiet`, and mail a named peer. Never hand the user a grant line. Detail: [references/once.md](references/once.md#without-a-role).

## On titling

- `fno agents hq init --scope <scope> --harness-session-id <full session id>`. Read the full id with `printenv CLAUDE_CODE_SESSION_ID` on claude, or `printenv CODEX_THREAD_ID` on codex. The stop hook matches the manifest id to the transcript name. So the 8-character handle from `fno agents registry whoami --json` does not work. Read its settled-findings section before the first check-in, and never re-derive it.
- Read the canon doc, rebind a changed session id, register with `/fno:fno-me`, and check `fno doctor`: [the setup steps](references/once.md#on-titling-the-setup-steps).
- Declare the shape now: `fno agents hq chart shape pass` for a one-wave pass, and `fno agents hq chart shape team` THE MOMENT the lead spawns its first worker. This is the field the Stop nudge reads; an undeclared org is nagged at every stop.
- Declare the term now: `fno agents hq chart term <span:Nh|compactions:N>` (e.g. `fno agents hq chart term span:96h`). An undeclared term still reads a 96h default. This is optional, but name it in the opening check-in line either way. When the Stop hook reports the term reached, hand off with `fno agents spawn --promote <scope> --hand-off`. To extend instead, write the reason on the declaration: `fno agents hq chart term <spec> --reason "..."`. A bare re-declaration without `--reason` is refused - the extension IS the receipt. The handoff command runs the whole succession itself. It announces the handoff, verifies the successor on its first beat, and files the retro at that verify. Do none of those by hand.

## One wave: --once

With `--once` the role rules one wave and expires. Run Who runs this and On titling, declare `fno agents hq chart shape pass`, and skip the term and native beat. Arm no native monitor, `/goal` or `/loop`. Then run [the one-wave pass](references/once.md#run-it-in-this-order) in order and step down with `fno agents hq done`. A kickoff that dispatches through `fno backlog advance` stays a pass. If the workers are org teammates that mail you back, the wave is an org. Declare `fno agents hq chart shape team` and run [org mode](references/once.md#org-mode-lead-over-the-wave) until the wave completes. The levers, Recording a ruling and the three halts apply to both shapes.

## Arm the beat

Read the beat before arming anything: `fno-agents harness-beat` prints `beat: <beat> (<source>)`. The key `agents.<harness>.beat` defaults to `auto`, which reads this harness's capability row. A user sets the key only to override the row. Arm what the line names, and never branch on the harness name:

- `loop`: inject the native `/loop` as the cheap heartbeat (below).
- `goal`: use provider-backed goal actions, never raw prompt-line `/goal` or `/loop`. Read effective readiness and require a positive `provider_goal` receipt plus a separate positive `stop` receipt. The verified provider goal is the primary continuation state, and Stop proves a different boundary. Every goal wake runs the check-in body below.
- `schedule`: use the harness's own recurring schedule tool, as [the beat table](references/beat-by-harness.md) names it.
- `daemon`: arm nothing. The daemon's lead-wake mail is the beat.

The daemon mails the settle push on every beat:

1. **Settle mail, 300s.** The daemon's `lead_settle` arm mails the lead once per covered PR that settles green. It mails again once per covered node that merges and closes. The lead arms no watch and relaunches nothing. A red settle stays with the daemon nudge ladder, which names the failing checks.

On `loop`, inject the loop:

```
fno agents mail send "/loop ${lead.checkin_interval} ${lead.checkin_text}" --to-self --raw
```

Journal `lead_armed` (`fno doctor event emit`) with the beat line and its receipt: the loop receipt on `loop`, the positive provider-goal and separate Stop receipts on `goal`. Only an event, mail or the heartbeat wakes the lead.

## The check-in body

What the loop prompt runs every interval and what you run by hand at any time.

One command runs the body: `fno agents hq checkin`. It gathers every reading below, prints them in a fixed order, diffs the previous canonical beat, and journals the `lead_checkin` row itself from the same numbers it printed. It never decides: no lever fires from it, and the levers stay yours. Refresh the canon doc first, so the command reads this beat's doc.

Name the role at the first beat with `fno agents hq checkin --name redraw`, and run `fno agents hq refresh-canon` every beat. Paste the `lineup:` table as printed in every status update. The per-line output contract and its procedures: [references/check-in.md](references/check-in.md).

State only facts this beat verified. Every claim in the beat report carries its footnote: the reading line, a command result you read this turn, or a file:line. A done claim needs fresh output read this turn. A previous beat proves nothing. Before a cannot claim, try the other routes, then name each one tried. A refusal from one tool is a fact about that tool, never about the task. The full rule is `skills/verify-footnotes/SKILL.md` sections 2 and 3.

Then run `fno agents hq checkin` (bare from the titled session, or `--scope <scope>` elsewhere). The `held:` line lists this role's due questions. A node-named question carries its `fno backlog decide <node> "<ruling>" --question-id <id>` closer. A question this role asked clears with `fno inbox outstanding clear <qid> --answer "<answer>" --authority role`.

Before the levers, the finish line. When `fno do pr status <n>` reads `ready: true`, run `fno do pr merge <n>` yourself. Standing decision: the team merges green, covered PRs. The user does not. This role is the team. `ready` IS the merge decision: the authorized-merge preview verdict, the same gate chain the merge command runs. CI, review coverage, base staleness, the merge slot, and merge authority all fold into it. When it reads false, the payload's `merge_decision.blockers` names what holds. One guard keeps the lever honest: resolve the row's project cwd and run both commands from there. A PR number is repository-local. Both commands derive their repo from the ambient cwd, so a portfolio role can merge an unrelated same-numbered PR.

Apply the first matching lever to each row, in this order:

1. Mail the stalled worker.
2. Run `fno backlog encounter <node> --evidence "what it cost"` to vote the node up. When evidence contradicts the filed priority, use `fno backlog update <node> priority=p1`. `p0` needs `--blocks-everything` and means the fleet is down.
3. If no role covers a row, start a new small epic. Do not grow a running epic. A vote or priority does not dispatch. See [A finding starts a new epic](references/check-in.md#a-finding-starts-a-new-epic).
4. If the row is the problem, run `fno backlog undefer` or `supersede`.
5. Keep a worker on the top unplanned node. Run the `start`, `target-ready` or `plan-ready` command, without `--name`.

Your job between beats is to start work. Run the `pick:` line and every startable line, in check-in order, and nothing the check-in omits. Fold a `batch:` group with `fno backlog contain` first. Decide reversible in-scope questions yourself. Detail: [between beats](references/check-in.md#between-beats).

Read `fno agents hq verdict` and print its first line and its `hygiene:` line. The `hygiene:` line is evidence, never a stop. It names `converging`, `stalled`, `degraded`, or `unknown`. An absent bound is absent, never satisfied. If the verdict changes, say so in the next beat's `--change` sentence. When it says `stalled`, `degraded`, or `unknown`, run `fno agents hq escalate <scope> --reason Verdict`. This records one deduplicated user question with the bounds and the handoff offer (`fno agents spawn --promote <scope> --hand-off`). The lead never spawns its own successor on its own judgment. The one pre-authorized succession is the declared term's expiry hand-off in [On titling](#on-titling). Every verdict-driven handoff stays an escalation with the handoff offer, and the user decides it.

## Recording a ruling

A titled lead answers the open questions in its scope, and escalates only what the superuser must decide. Every flag, refusal and channel detail: [references/rulings.md](references/rulings.md).

- **Rule the question that sits with you.** Give the why: `fno inbox outstanding clear <qid> --answer "<ruling>" --authority role`. If only the user can rule it, climb it: `fno inbox outstanding climb <qid> --evidence "<what you checked>"`. A confident guess is a defect.
- **File your own user-only decision** with `fno inbox outstanding ask`, carrying `## Tried` and `## Evidence`. Only a lead files to the user's board. One push per question. Workers never push.
- **Withdraw a moot question** with the same clear.
- **Escalate the deny classes only:** `irreversible`, `money-security`, `outside-reach`, `taste` and `law-change`. Decide and log everything else.
- **Silence has a default.** Past the deadline, take the recommended option and record it. When the call is irreversible, wait.
- **A held question is not news.** Report only a new, overdue, or asked-about one.
- **Record a superuser chat answer.** Use `fno inbox decisions set` for a decision change. Else use `fno inbox decide <node> "<answer>" --authority role --rationale "superuser in chat: <their words>"`.

A titled lead is not the superuser: the `operator` authority is refused on an agent session, and a decision stays superuser tier. Its channels: `fno backlog note <node> <text>` for a row, a [merge hold](references/cli-commands.md#merge-holds) for a merge condition, and `fno inbox decisions set` for a user's durable rule. Read a ruling back with `fno backlog decisions <subject>`.

## The one dispatch exception

The tenured lead does not dispatch. A `--once` pass dispatches only through its kickoff and its org, as [the one-wave pass](references/once.md) says. The single exception: `fno agents session status` shows the dispatching arm red, and the spawn is journaled `lead_dispatch_exception` naming the arm and the node BEFORE the spawn fires. A spawn without that row is a defect. Journal it with:

`fno doctor event emit -t lead_dispatch_exception -s loop -d '{"scope":"<scope>","arm":"<arm>","node":"<id>"}'`

The `-s loop` source keeps the row out of the `test` default. The exception uses the canonical implementation worker line in `references/org-operations.md#control-surfaces`. A blueprint launch writes its [brief](references/org-operations.md#blueprint-brief) into the node first.

## Stop and park

Exit is blocked while actionable rows exist. That is the stop hook doing its job. A clean board, or a board waiting only on the user, CI or a worker, exits `NoWork`. The next beat or mail wakes the lead, and the daemon's settle mail is mail. On Codex, a quiet park pauses the verified provider goal without clearing or replacing its objective. The wake arm resumes it only after a positive provider receipt. `NoProgress` after three unshrinking fires still escalates automatically and parks the session. The answer wakes it through the wake arm. Do not fight the hook or `/goal clear` on quiet or `NoProgress`.
## The three halts

Three halts have three scopes. Run `fno agents audit incident stop --reason "<why>"` to arm the fleet breaker. From the next tick, fleet admission refuses new spawns, dispatches, and `fno doctor test` runs. Keep admission closed until `fno agents audit incident clear --reason "<why>"` reopens it. Running work and mail continue. `fno agents hq cancel --scope <scope>` ends one scope's walk. `fno agents hq done` ends one role. Arming the fleet breaker affects others. No standing decision grants this authority to an agent. The lead names evidence and escalates. Arm only on user order unless a later decision grants the authority.

Breaker detail and incident claims: [cli-commands.md](references/cli-commands.md#fleet-breaker-detail).

A quiet window is not a halt. When the user asks you not to interrupt them, run `/fno:dnd` (codex: `$fno:dnd`). It holds only your inbound mail, and leaves fleet work running.

## StepDown

At handoff or before `fno agents hq done`, the succession files parts 1 and 2 for you. The [succession steps](references/once.md#stepdown-the-succession-steps) run for you. The retro grades parts 3 to 5 itself and writes `PENDING-NODES-from-<session8>-retro.md`. Your part is the filing: turn each `- [ ]` line in that file into a node with `fno backlog idea`, then tick the box. A part still marked `status: pending` means the grade failed, so write that part from `part1-metrics.md` and `part2-timeline.md`. With `--once`, `fno agents hq done` is the last act of pass step 5 or of the org's wave boundary.

Every reference, including the per-harness launch lines in [beat-by-harness.md](references/beat-by-harness.md#launch-by-harness), lives in [references/](references/).

## Known Limitations and Deferred Work

- A Codex lead has no native cron or Monitor beat; its provider goal and Footnote Stop receipts are read independently, and the external wake arm supplies cadence. A `--once` pass does not supervise the workers it spawns. The org role-source field is not landed yet. See [LIMITATIONS.md](LIMITATIONS.md).
