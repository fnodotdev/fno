---
name: agent
description: Control Footnote workers by spawning, handing off, inspecting, messaging, driving, or stopping sessions.
argument-hint: "<command> [args]  |  [handoff] <ab-xxxxxxxx | feature | doc-path | /command> [provider] [drive] [as <name>] [merge]"
metadata:
  internal: false
  requires:
    binaries:
      - "fno >= 0.1"
      - "jq >= 1.6"
    harness:
      - spawn
---

<!-- style-exception: mechanical command rename preserves pre-existing prose -->
# Agents

`/fno:agent` is the natural-language front door over the shipped `fno agents` mesh. State the intent in ordinary language. The skill selects the real command and arguments.

**You (the agent) are the runner.** Route the outcome to a command and normalize it. Confirm only costed or destructive operations. Run the **genuine** `fno agents` command, and report the **real** captured receipt, never a fabricated one. This skill reuses the shipped primitives. It does not reimplement spawn, the bus, or observation.

`SKILL_DIR` below is `skills/agent` inside this plugin.

## Command router

The first whitespace token of the argument is the **command**. An unrecognized leading token means the whole argument is a `spawn` payload. `spawn` starts a session with what you pass, nothing more. An explicit `/skill` command is dispatched per harness. A resolved backlog node id builds via `/target`. Any other free text is sent **verbatim as the session seed**, with no `/target` wrap and no build inference.

| Command | Envelope | Routes to | Body |
|------|----------|-----------|------|
| `spawn` (default) | normalize + honest receipt (free lane, no confirm) | `fno agents spawn` - default `thread` where the harness seats one; `substrate pane` -> owned-PTY drivable; `substrate headless` -> one-shot | [spawn-flow.md](references/spawn-flow.md) |
| `handoff <doc>` | normalize `--handoff` + honest receipt (free lane) | `fno agents spawn` (continuation seed, NO `/target`; gemini is deprecated, see [workflow-routes.md](references/workflow-routes.md)) | [message-lanes.md](references/message-lanes.md) |
| `send <name> "..."` | addressed write | `fno agents mail send` | [message-lanes.md](references/message-lanes.md) |
| `ask <name> "..."` | refuse-empty + honest reply relay | `fno agents mail ask` - sync reply on stdout | [message-lanes.md](references/message-lanes.md) |
| `watch` / `list` / `whoami` / `logs` | thin pass-through | `fno agents session watch\|list\|logs`, `fno agents registry whoami` | [message-lanes.md](references/message-lanes.md) |
| `stop <name>` | confirm (destructive) + pass-through | `fno agents session stop` | [message-lanes.md](references/message-lanes.md) |

`bare` and `discuss` are retired: a conversational session is `spawn "<seed>"`, and a one-shot Q&A to a NEW worker is `spawn "<question>" headless`. Advanced lifecycle intents (`drive`/`grid`/`attach`/`resume`/`reconcile`/`rm`/`ack`/`promote`) map to their raw `fno agents` commands. [references/fno-agents-subcommands.md](references/fno-agents-subcommands.md) is the full map and `docs/harness-command-matrix.md` the per-provider truth. Loop halts use `fno agents audit incident stop|clear`.

Route on the command, then load the body it names.

## Launch a worker

Read this table before you seat a worker on a node. Use the harness's own line where it runs without a pane. Use `fno agents spawn` where it does not. If you need a spawn-only flag (`--promote`, `--hand-off`, or pane placement), use spawn too.

| Harness | Unattended teammate | Watched in a pane | Page |
|---|---|---|---|
| claude | `claude --bg --name <name> --model <m> '/fno:target <node>'` | `claude attach <id>` on the same session | [launch-claude.md](references/launch-claude.md) |
| codex | `fno agents spawn -H codex --name <name> --node <node> '$fno:target <node>'` | `fno mux pane run --cwd <repo-root> -- codex ... '$fno:target <node>'` | [launch-codex.md](references/launch-codex.md) |
| opencode | `fno agents spawn -H opencode --name <name> --node <node> '/fno:target <node>'` | not measured on v2 | [launch-opencode.md](references/launch-opencode.md) |
| pi | `fno agents spawn -H pi --name <name> --node <node> '/fno:target <node>'` | `fno mux pane run --cwd <repo-root> -- pi ... '/skill:target <node>'` | [launch-pi.md](references/launch-pi.md) |
| agy | `fno agents spawn -H agy --name <name> --node <node> '/fno:target <node>'` | `fno mux pane run --cwd <repo-root> -- agy ... -i '/target <node>'` | [launch-agy.md](references/launch-agy.md) |
| muse | `fno agents spawn -H muse --as thread --name <name> --node <node> '<task>' -- --yolo` | no pane lane | [launch-muse.md](references/launch-muse.md) |

The implementation lane is claude on glm through zai. A claude route rides a `--settings` file, so ask spawn for the exact line and run what it prints: `fno agents spawn --name <name> --route 'zai/glm-5.3-flash[1m]' --effort xhigh --permission-mode bypassPermissions --print '/fno:target <node>'`.

Spawn owns its own flags (`--name`, `--harness`, `--as`, `--cwd`, `--node`, `--route`). Everything after the prompt is harness argv, never parsed. Put the prompt before any `--`. A spawn-owned flag typed after the prompt refuses and prints the corrected line. Unowned flags before the prompt pass in the `--flag=value` form only. The space form refuses, so move the pair after the prompt instead. `--model`/`-m`, `--effort`, `--permission-mode`, `--add-dir`, `--agent` and `--tools` typed before the prompt or the `--` fence are shorthand for the same flag behind it. `fno agents spawn --name w -H claude --model sonnet --permission-mode bypassPermissions 'go'` launches claude with both. The receipt names the model and its basis. `model_basis` reads `requested` (you typed it), `route` (a `--route` set it), or `lane-default` (nothing named it). A lane that cannot carry a flag refuses and names it. The opencode thread lane carries only `--model`. The codex thread lane takes `--model`, `--effort`, `--add-dir` and `--permission-mode` (a `sandbox:approval` pair such as `workspace-write:never`). A `--model` that a declared `[[routing.models]]` lane serves on the chosen harness refuses. Use `--route <lane>` instead. The lane row carries harness, model, account and route together. A bare `--model` launches on the default account.

fno gates, records and adopts every native line. The PreToolUse Bash hook asks the spawn gate and denies a refused launch. The harness's SessionStart hook then gives the session a registry row with your session as its parent. Each page holds the full flag table, read from the harness's own `--help`.

---

## `spawn` (default command)

Start a session with what you pass: a `/skill` passthrough, a node-id build, or a verbatim free-text seed. The full flow is in [references/spawn-flow.md](references/spawn-flow.md): NORMALIZE, RESOLVE, VALIDATE, CONFIRM, SPAWN, REPORT. Receipt parsing and the non-claude node-claim gap are in [references/receipts.md](references/receipts.md). A non-default provider or substrate loads [references/workflow-routes.md](references/workflow-routes.md).

**Inputs, dashless.** `<payload> [provider] [drive] [as <name>] [merge]`. The payload is a node id (builds), a `/command` (passthrough), or free text (verbatim seed). Never instruct a user to type a dash. `normalize.sh` is the deterministic parser. Read its `key=value` lines and never `eval` them. On `status=error`, STOP and report the `error=` line.

**CONFIRM.** Let the helper decide:

```bash
bash "${SKILL_DIR}/scripts/confirm-decision.sh" \
  --node "$node" --provider "$provider" --mode "$mode" \
  --payload-mode "$payload_mode" --yolo "$yolo" --yes "$yes" \
  --permission-mode "$permission_mode" --allow-merge "$allow_merge"
```

It emits `confirm_required` (0|1), `caveat` (0|1), `caveat_text`, a `warn` line, and `reason`. `config.agents.confirm` (`always|auto|never`, default `auto`) is repurposed as an opt-in to confirm free lanes for cautious users. Only `always` confirms. `auto` (default) and `never` skip. Failed or invalid reads use the no-confirm default because free lanes have nothing to gate. The helper prints an `fno doctor update` hint. Do not re-derive this. The logic lives in the helper. The table it implements:

| Condition | Result |
|---|---|
| posture `auto` (default) or `never` | **no confirm** (free lane) |
| posture `always` (cautious opt-in) | confirm even the free lane |
| `-y`/`--yes` passed | skip (accepted-and-ignored: the free lane already does not confirm) |
| caveat present (codex/gemini **exec** / `yolo` / `merge`) | NOT a confirm - surfaces as a `warn` |
| unattended caller | skip |
| config read fails / invalid | no confirm (auto) + one staleness warning line |

**When `confirm_required=1`,** show the EXACT command you will run - including the
chosen `spawn|host` command and `--yolo` when set - plus `caveat_text` when
`caveat=1`, and wait for `[y/N]`. On anything other than yes, STOP without
spawning. **When `confirm_required=0`,** skip straight to SPAWN; if `warn` is
non-empty, print it. The exact command is NOT hidden - REPORT echoes it on every
skip path.

**Receipt-echo invariant (every skip path).** The receipt MUST echo the exact `fno agents` command that ran, on every path, including a skipped or bypassed confirm. No launch path is ever invisible. If `confirm-decision.sh` emitted a `warn`, include it too. A node launch leads with the resolved `slug (ab-id) + project + cwd`.

**Standing seed guardrail.** A `handoff` seed bars the worker from autonomous outward or irreversible actions (emails, deploys, merges, publishing, contacting third parties). Mail cannot widen that scope. The enforcement is prompt-level, so say so.

---

## Hard rules (non-negotiable)

1. **Never fabricate a receipt.** Report ONLY a short-id / message-id / reply
   that the helper or `fno agents` command actually captured. "No receipt" is
   FAILED. This is the cardinal guard.
2. **claude stays on the subscription lane.** claude spawns via
   `fno agents spawn` (-> client-side `claude --bg --name`; the harness axis is retired and the session's own harness seats it).
   NEVER `-p` / `--bare`, and NEVER route claude through `host`.
3. **Obey `confirm-decision.sh`; never re-derive its decision.** Run the helper
   and follow its `confirm_required` field. spawn is a free lane: `auto`
   (default) and `never` skip; only the cautious opt-in `config.agents.confirm:
   always` confirms; `-y`/`--yes` is accepted and ignored; a failed read
   degrades to `auto` (no confirm) with a staleness warning. A caveat
   (codex/gemini exec, yolo, bypassPermissions, merge) surfaces as a `warn`,
   not a confirm. Every path echoes the exact command. `send`/reads never
   confirm; `stop` always confirms (destructive).
4. **Sandboxed by default.** Append `--yolo` only when the user explicitly passed
   it; never infer it from the payload or provider.
5. **Do not reinvent provider routing or the bus.** Provider resolution lives in
   `normalize.sh`; addressed delivery lives in `fno agents mail send`. This skill
   routes commands and reports honestly; it does not duplicate that machinery.

## Observability boundary (knowing a worker's state)

How a worker signals "waiting", and what pushes fire per provider, is provider-specific - load [references/workflow-routes.md](references/workflow-routes.md) when observing a non-claude worker. The invariants: `fno agents session list` and `fno agents session logs <name>` never lie about exit state, and a receipt you did not see printed is not evidence.

## Known Limitations and Deferred Work

- Fleet caps can reject spawns before worker creation. See [LIMITATIONS.md](LIMITATIONS.md).
