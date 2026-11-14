<!-- style-exception: mechanical command rename preserves pre-existing prose -->
# Spawn flow

The full `spawn` body, one hop from [SKILL.md](../SKILL.md#spawn-default-command). The root keeps the CONFIRM table, the receipt-echo invariant and the hard rules.

## Inputs (dashless grammar - phone-first)

The documented grammar carries **zero dash tokens** so a phone keyboard's smart
punctuation (which rewrites `--` into a long dash and mangles a lone `-`) can
never corrupt it. The user types barewords or natural language; you map them to
the structured fields. The canonical form:

```
<payload> [provider] [drive] [as <name>] [merge]
```

The angle brackets are placeholder notation, not literal syntax. Infer the
fields from the user's unquoted text (a free-form phrasing maps the same way -
see below):

- **payload** (required): a backlog node id (`ab-XXXXXXXX`, builds via `/target`), an explicit slash command (`/target ...`, `/fno:ship pr check 42`, dispatched per harness), or any other free text - sent **verbatim as the session seed** (a live pane), no `/target` wrap. (: `spawn "fix the login bug"` seeds a session; it does NOT build. To build free text, write `spawn /target <text>` or pass a node id.)
- **one-shot Q&A**: append the `headless` substrate (: it replaced the
  old one-shot ask and `bare`) - `spawn "<question>" headless` returns a
  single reply. (Messaging an EXISTING worker for an inline reply is the
  `ask` command, not a spawn.)
- **provider** (optional, trailing bareword): any non-claude harness in the
  supported set - today `codex`, `gemini`, `agy`, `opencode`, with more coming.
  `spawn ab-X codex` -> provider `codex`; `spawn ab-X opencode` -> `opencode`.
  Default resolves from config -> `claude`. "on codex" / "with gemini" map here.
  The set is not hardcoded here: normalize matches any bareword in its
  `VALID_PROVIDERS` (which mirrors the Rust `KNOWN_PROVIDERS` source of truth),
  so a new harness needs no grammar edit. `docs/harness-command-matrix.md` is
  the per-provider capability truth. Quotes protect a trailing word that is
  ambiguously a provider. (megawalk drivers like `hermes`/`openclaw` are a
  different axis, not `spawn` providers.)
- **`drive`** (alias `interactive`, optional): route codex/agy to a drivable
  `host` session instead of an autonomous `spawn`. No-op for claude. "drive it"
  / "interactive" map here.
- **`as <name>`** (optional): explicit agent name. Default is derived
  (`<command>-<node-id>-<slug>` / `<command>-<slug>`). "call it X" / "name it X" map here.
- **`merge`** (optional): do NOT inject the no-merge intent. Default injects it
  so a fire-from-phone autonomous worker lands a PR for review, not an
  auto-merge. (`merge` only omits the no-merge intent; true auto-merge stays a
  separate opt-in.) "let it merge" / "can merge" map here.

The model, effort, provider, harness, substrate, yolo and permission-mode
postures are RETIRED: the spawn axes died and each lane template owns its own
launch line (docs/HARNESSES.md names the native form per harness). A model
pin rides the route; the lane default owns the permission posture.
- **`-r`/`--role <role>`** (optional): per-spawn model-routing role
  (`coordinate|tidy|orient|consolidate` route to the secondary GLM; production
  roles stay primary). Model routing ONLY - it does not affect pane/layout.
- **`-t`/`--timeout <secs>`** (optional): per-spawn timeout, passed to `fno agents
  spawn --timeout` (CLI default 600).
- **`--here` / `--fresh`** (optional): a spawn with NO cwd source defaults to the
  canonical main root, so the same command behaves identically wherever
  the launcher stands. `--here`/`--in-place` is the explicit opt-in to keep the
  caller's cwd (extend my WIP right here); `--fresh` is an accepted no-op alias
  (the default is already canonical). The move is never silent: a redirect note
  names the canonical dir and the spawn receipt carries the effective `cwd` when
  it differs from the caller. A code payload still auto-isolates to a fresh
  worktree regardless (that lands as an explicit `--cwd`, which wins).
- **Tier-3 harness passthrough** (all optional): `--add-dir`, `--agent`,
  `--tools`, `--deny-tools` forward straight to `fno agents spawn`, opaque to
  the skill; the CLI maps or fails closed per provider, and a no-equivalent
  provider cell is rejected before spawn. Per-flag detail:
  [references/workflow-routes.md](workflow-routes.md).

`normalize.sh` is the deterministic backstop: it recognizes this closed posture
vocabulary as a contiguous TRAILING run (right-anchored), so a posture word that
sits mid-task ("spawn the node that will merge two branches") stays task text and
does NOT silently set the posture. A bare `as` with no name after it is an error.

**Dash-flags still work (undocumented back-compat).** A desktop scripter's old invocations still work: `--yolo`, `-n <name>`, `-m`, `--model <name>`, `-i`, `-y`. They canonicalize to the same fields. The free lanes no longer confirm, so `-y`/`--yes` is accepted and ignored (see CONFIRM). `--model` has no short form at this layer, because `-m` is `--allow-merge`. `fno agents spawn` itself takes `-m` for the model. A genuinely unknown dash-flag fails loud. Dashless is the only form you advertise. Never instruct a user to type a dash.

## Flow: NORMALIZE -> RESOLVE -> VALIDATE -> CONFIRM -> SPAWN -> REPORT

### 1. NORMALIZE (deterministic helper)

Run the normalizer with the spawn payload text exactly as given (pass it as ONE
argument). You may pass trailing posture barewords inside `--input` and let the
helper extract them, or pass the fields you inferred from natural language as the
internal flags below - the helper resolves both to identical fields, so either
path is safe. ALWAYS pass `--handoff` for a `handoff` command, so the verbatim doc
path skips posture parsing.

```bash
bash "${SKILL_DIR}/scripts/normalize.sh" --input "<raw payload [posture barewords]>" \
  [--provider <p>|-P <p>] [-n <n>|--name <n>] [--model <m>] [--effort <e>] [-i] [--yolo] [--handoff] \
  [-m|--allow-merge] [-y|--yes] [-C <project>|--project <project>] [-f|--force]
```

It strips smart quotes, parses the trailing dashless posture run, canonicalizes a
token-initial em/en-dash to `--`,
derives the agent name (`<command>-<node-id>-<slug>` for a node, `<command>-<slug>` for free text),
resolves the provider (explicit -> `config.accounts` via the shipped
`resolve_dispatch_target` -> `claude`), detects the node id, resolves a
`-C`/`--project` target to its work-map `resolved_cwd`, picks the payload
mode, and assembles the provider-aware `message`. Read its `key=value` output
line by line. Never `eval` it. Captured fields: `node`, `node_query`,
`spawn_next`, `next_scope`, `name`, `provider`, `mode`, `yolo`,
`yes`, `allow_merge`, `project`, `resolved_cwd`, `payload_mode`, `message`.
The model-adjacent fields are `model` and `effort`; omit either downstream when
empty so provider defaults remain unchanged.

**Cross-project target (`-C`/`--project`).** By default a free-text spawn launches
in the caller's cwd. `-C <project>` retargets it: normalize resolves the registry
`name`/`short_name` (`config.work.workspaces.*.projects` in config.toml) to that
project's work-map root and emits `resolved_cwd`, which you pass to
`spawn.sh --cwd` (see SPAWN below). A natural-language `in <project>` / `in the <project>
repo` / `as <project>` is YOUR job to translate: when you judge the phrasing is a
project directive (not task prose like "fix the bug in etl"), call normalize with
`-C <project>`; the deterministic layer only ever sees the unambiguous flag.
Unknown project, or a mapped path missing on disk, is a loud `status=error` (no
spawn). A backlog node carries its OWN project, so a node + `-C` is a refused
conflict UNLESS you also pass `-f`/`--force` (flag-win override: run the node's
work in the forced repo). When you re-run normalize after resolving a slug/`next`
to an ab-id (see RESOLVE), carry `-C`/`-f` through so the conflict check fires on
the real node.

`-P` is the **provider** shorthand (`-P <harness>`, identical to `--provider`):
it selects the harness in this layer - the axis `--harness` shares, which spawn
forwards as `--harness`. It is not the project flag: an earlier `-P`=`--project`
mapping silently misrouted `-P zai` to a cross-project hop, so project moved to
`-C`. Note this is the skill-layer provider/harness axis, not the separate
model-vendor `--provider` the `fno agents spawn` CLI exposes (where `-H` selects
the harness and `-P` the vendor); z.ai is a model route selected via
`--model glm-5.2`, so `-P zai` errors here because z.ai is not a harness.

- If `status=error`, STOP. Report the `error=` line. Do NOT spawn.
- Otherwise capture every field for RESOLVE/VALIDATE/CONFIRM/SPAWN.

**Payload modes** (chosen deterministically by normalize;):

- **seed** (default for free text): sent VERBATIM as the session opening seed on
  the default pane - no `/target` wrap, no `no-merge`, no build inference. This is
  what `spawn "fix the login bug"` produces (it seeds a session; it does NOT
  build). To build free text, pass an explicit `/target <text>` (passthrough) or a
  node id.
- **build** (a resolved node id): the ONE surviving implicit `/target`, config-
  driven not shape-inferred - dispatch_command resolution (node > config > builtin
  `/target`). Normalized per harness : claude/agy get `/target <id>` (+
  `no-merge`); opencode `/fno:target <id>`; codex `$fno:target <id>` - all run the
  real pipeline on the node. gemini is deprecated (successor: agy) and refused.
- **handoff** (`--handoff`): a doc path becomes a continuation seed (read the doc,
  continue from where it left off, do NOT re-derive a plan) + a standing
  guardrail against autonomous outward/irreversible actions. Supports Claude
  and Codex; NO `/target`, NO `no-merge`. See [message-lanes.md](message-lanes.md#handoff-doc---continue-a-doc-without-re-deriving).
- **passthrough** (leading `/`): the explicit command, normalized per harness for
  ANY footnote command - `/command` verbatim on claude/agy, `/fno:command` on
  opencode, `$fno:command` on codex (so `/agent spawn /blueprint -H codex` runs the
  real skill). gemini is deprecated and refused (route to claude/codex/opencode/agy).

### 1b. RESOLVE (id-free entry modes -> a concrete node id)

A backlog node id is opaque and matches `^[a-z][a-z0-9]{0,7}-[0-9a-f]{4,8}$`, so
a repo's configured `id_prefix`/`id_hex_width` classifies exactly like `ab-`.
Two id-free ways to name a NODE resolve to a concrete id here, in order. Resolve
to ONE id, then **re-run normalize** with `--input "<that id>"` (carrying the same
`--provider`/`--name`/posture flags) so `message`/`name`/`node` are rebuilt as a
node build. Skip this whole step when `node` is already a concrete id. Free text
that names no node is NOT resolved here (: it is a verbatim seed, never a
fuzzy node search) - when `node`/`node_query`/`spawn_next` are all empty, take the
`seed` payload straight to SPAWN.

1. **`node` non-empty** (tier 1 exact id) -> use it as-is.
2. **`spawn_next=1`** (tier 5) -> `fno backlog next $( [[ "$next_scope" == all ]] && echo --all || true )`
   (it already emits JSON; there is no `-J` flag on `next`). Take `.id`. If it is
   `null`/empty, report **"no ready node"** and launch nothing (AC3-ERR).
   Otherwise that id is the node.
3. **`node_query` non-empty** (tier 2 slug candidate) -> `fno backlog get "$node_query" --field id`.
   On exit 0 that is the node (exact slug -> id): re-normalize with it for a
   node build. A bare hex token arrives here too - the resolver is
   format-agnostic and accepts it, so normalize never guesses a prefix. On a
   non-zero exit the slug missed - do NOT fuzzy-search; the token stays a
   verbatim `seed` (its `payload_mode`/`message` from normalize), so proceed to
   SPAWN with the seed as-is.

**Termination invariant:** a canonical id returned by the resolver MUST classify
as tier 1 on the re-normalize pass. Re-normalize exactly ONCE per resolution; if
the second pass still yields `node_query` (a repo whose ids fall outside the
shape), fall through to `seed` rather than resolving again.

After resolving to an id, re-normalize and continue. A resolution that names
a node in another project will boot the worker in that node's `_resolved_cwd`
(see SPAWN below); the REPORT receipt names the resolved `slug (ab-id) + project + cwd`
so even a delegated `next`/`all` is never a silent surprise.

### 2. VALIDATE (refuse bad input before any billed launch)

- **Node must resolve, and a miss is judged by `node_bare`.** If `node` is
  non-empty, run `fno backlog get "$node"`. On a miss (non-zero exit or no
  `.id`):
  - **`node_bare=1`** - the payload was nothing but the id, so the user named it
    deliberately. STOP and tell the user. Do NOT spawn.
  - **`node_bare=0`** - the id was inferred from a payload carrying other text
    (prose whose first word merely fits the id shape - `a-f` are letters, so
    `re-added` and `dead-beef` match it - or a `/target <prose>` line the
    extraction grep'd a word out of). Degrade to `node=""` with a one-line note
    and spawn on the message as typed; only the receipt loses its node suffix.

  `/target <id>` with nothing else reports `node_bare=1`, so an explicit command
  naming a node that does not exist still refuses rather than burning a worker
  on it.

  The asymmetry is about what the user *typed*, not which tier matched. Refusing
  a spawn because ordinary prose happened to look like an id would break the
  free-text-is-always-a-verbatim-seed rule above.
- **Collision pre-check (read-only), with a self-handoff exception.** If `node`
  is non-empty, run `fno agents claim status "node:$node" --json` and inspect `.state`
  and `.holder`. If `live`, decide whether the holder is a FOREIGN worker or
  YOURSELF handing off your own node:
  - **Self-handoff** (holder == your own claim): read your own holder from the
    session manifest - `sed -n 's/^target_claim_holder: *"\?\([^"]*\)"\?/\1/p'
    .fno/target-state.md` (empty if you hold no target claim). If it equals
    `.holder`, this is YOUR node, not a foreign collision - pass `--self
    "<holder>"` to `spawn.sh` so it emits a `self-handoff` receipt instead of a
    confusing `already-running`. `/agent` does NOT reassign the node from here:
    a node claim can be released only by the two sanctioned sites (`handoff.sh`
    or `fno backlog unclaim`, holder-verified - a helper subprocess release is
    an authority violation), and a thread spawn cannot emit the `delegated` event a
    clean takeover needs. For an immediate clean handoff use `/target`'s
    self-handoff (it archives state, emits the delegated event, and releases the
    claim atomically); or run `fno backlog unclaim <node>` and re-dispatch.
  - **Foreign collision** (holder is someone else, or you hold no claim): a
    worker already holds this node - tell the user (point at `fno agents session logs
    $name`) and do NOT spawn a second loop.

  (`spawn.sh` re-checks atomically too: it treats a `live-claim` whose holder
  matches `--self` as a self-handoff and refuses any other.)

A free-text seed (`node` empty) skips the node checks.

### 3. CONFIRM (free lane: no confirm by default)

`spawn` is a **free, reversible lane**: an autonomous worker lands a PR for
REVIEW (it never auto-merges; `merge` only omits the no-merge intent), so there
is nothing billed or destructive to gate. It therefore does **not** confirm by
default. An unattended/headless caller always skips. Otherwise let the helper
compute the decision:

```bash
bash "${SKILL_DIR}/scripts/confirm-decision.sh" \
  --node "$node" --provider "$provider" --mode "$mode" \
  --payload-mode "$payload_mode" --yolo "$yolo" --yes "$yes" \
  --permission-mode "$permission_mode" --allow-merge "$allow_merge"
```

Pass `--permission-mode "$permission_mode"` so a gate-bypass launch still surfaces
its caveat: normalize maps `--yolo` on claude to `permission_mode=bypassPermissions`
and clears `yolo`, so the yolo caveat alone would go silent - the helper instead
caveats on the effective `bypassPermissions` mode (whether it came from `--yolo` or
an explicit `--permission-mode`).

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

### 4. SPAWN (genuine execution + honest receipt)

Run the spawn helper with the normalized fields - reached straight from a
`confirm_required=0` skip, or after a yes when `confirm_required=1`. It re-checks
for a live duplicate, picks the command from `provider`/`mode`/`payload_mode`, runs
the real `fno agents` command (name POSITIONAL, `--provider`, NEVER `-p`/`--bare`),
and parses the receipt deterministically:

```bash
bash "${SKILL_DIR}/scripts/spawn.sh" --name "$name" --provider "$provider" \
  --message "$message" --mode "$mode" --payload-mode "$payload_mode" \
  [--model "$model"] [--effort "$effort"] [--substrate "$substrate"] [--yolo] [--node "$node"] [--self "$self_holder"] [--cwd "<cwd source, see below>"] \
  [--permission-mode "$permission_mode"] [--role "$role"] [--timeout "$timeout"] [--fresh] [--here]
```

Pass `--self "$self_holder"` for a confirmed self-handoff only. That means your `target_claim_holder` matched the live `.holder` in the collision pre-check. The flag makes the atomic re-check emit a `self-handoff` receipt that routes you to the sanctioned handoff, instead of a confusing foreign `already-running`. It neither spawns nor releases the claim. If normalize emitted a non-empty value, pass `--role "$role"` and `--timeout "$timeout"`. If `fresh=1`, pass `--fresh`. If `here=1`, pass `--here`. spawn.sh forwards each to `fno agents spawn`, which validates values fail-closed. A normalized `substrate=headless` rides `--once` (the one-shot). Thread and pane are the native lane's own defaults. If normalize emitted a non-empty value, pass `--model "$model"`, `--effort "$effort"` and `--permission-mode "$permission_mode"`. spawn.sh forwards each to `fno agents spawn`, where it is shorthand for the harness's own flag. `--yolo` stays retired.

An empty `substrate` is the built-in default: `thread` where the harness seats one (else `pane`), injected explicitly by the spawn seam. A spawn with no placement flag opens no view. A default never creates one. When the work needs a screen, the mux thread door tunes one.
Pass `--node` whenever `node` is non-empty. The spawn derives the command from the node, so a build message need not name one; a typed `/fno:target` or `/fno:blueprint` that disagrees with the node's command is refused. Choose the `--cwd` source in this priority order, so launch cwd
follows the work-map root:

1. normalize's `resolved_cwd` when non-empty (a `-C`/`--project` target, including
   a `-f`/`--force` override of a node's own cwd) - it wins over the node's cwd;
2. else the node's `_resolved_cwd` (from `fno backlog get "$node"`);
3. else the caller's `cwd`.

**Auto-worktree.** When the payload writes code and the resolved `--cwd` is a repo's MAIN checkout, `spawn.sh` deterministically creates a worktree on a fresh feature branch and launches the worker THERE - born isolated, location verdict `ok` from line one, no reliance on the worker self-creating a worktree. It forwards the payload's harness to `fno agents workspace worktree ensure`, so the location is whatever the per-project policy resolves to (a claude payload lands harness-native under `<repo>/.claude/worktrees/`); never assume a fixed base. "Writes code" is keyed off `payload_mode`, not the message text: a node `build` dispatch (claude `/target <id>`, opencode `/fno:target <id>`, or codex `$fno:target <id>`) and an explicit claude `/target`|`/execute`|`/fix` passthrough all isolate; a `seed`/`handoff` and a non-code claude slash command (`/think` writes a design doc) stay in repo root. An already-isolated worktree cwd is not re-isolated; any creation error fails safe to repo root. This is in `spawn.sh` (deterministic), so you do nothing here except relay the receipt - its `cwd="<worktree>"` field on the launched line surfaces the real launch dir.

**Delegation for a self-isolating payload.** One case skips the pre-creation above: a **claude** `/target` carrying a **resolved node**. That worker isolates itself at cold-start - `fno do target start <node>` runs `fno worktree ensure` and then the harness `EnterWorktree` tool - so `spawn.sh` launches it at the repo ROOT and lets it own the worktree. Doing both would be worse: `EnterWorktree` moves the session's cwd but leaves its PROJECT at the launch dir, and claude keys `~/.claude/projects/` off that launch dir with no rename hook, so pre-creating binds the project to the worktree and mints a throwaway project dir per spawn that is orphaned when the worktree is reaped. Isolation itself is unchanged - still a worktree off `origin/main`, and both paths run the same `fno agents workspace worktree ensure`, so the per-project `worktree` policy is honored either way. Only the timing, the owner, and the branch name move.

Three conditions, all load-bearing. **claude**, because `EnterWorktree` is a
Claude Code harness tool and a codex/opencode `/fno:target` worker cannot move
its session into a worktree it creates. **A node**, because `fno do target start`
has nothing to resolve without one - a free-text `/target ship it` never
isolates, it hits the location-refusal backstop. **A literal `/target`
message**, because `payload_mode` defaults to `build`, so a caller passing a
prose task without `--payload-mode` would otherwise be assumed to run `/target`
when it never will. Anything short of all three keeps pre-creation.

A delegated launch prints `auto-worktree: delegated to the worker cold-start
(launching at <root>)` on stderr and carries NO `cwd=` receipt field, since no
worktree was made here.

### 5. REPORT (echo ONLY what actually happened)

**Receipt-echo invariant (every skip path).** When the confirm was auto-skipped
or bypassed, the receipt MUST echo the exact `fno agents` command that ran. No
launch path is ever invisible. If `confirm-decision.sh` emitted a `warn`,
include it here too.

**Resolved-handle echo (Locked Decision 5: echo always).** When the
launch carries a node, the receipt MUST lead with the resolved
`slug (ab-id) + project + cwd` - read them from the `fno backlog get "$node"`
JSON (`.slug`, `.id`, `.project`, `._resolved_cwd`), falling back to `(ab-id)`
when the node is not yet slugged. This holds on EVERY exact-tier + `next` path,
including the posture=auto silent-launch lane, so even a delegated `next`/`all`
or a cross-project resolution is visible post-hoc. (describe-it already confirmed
the handle before launch.)

**Cross-project echo on the free-text path.** When the launch carried no node but
normalize emitted a non-empty `resolved_cwd` (a `-C`/`--project` target), the
receipt MUST lead with `project=<project> cwd=<resolved_cwd>` (both already in
hand from normalize - no extra lookup). A free-text cross-project hop is then just
as visible as a node one; no launch silently lands in a different repo.

Read `spawn.sh`'s single outcome line and relay it faithfully:

- `result=launched ... mode=exec ...` -> a background worker launched. Quote the
  **real** `short_id` and give `fno agents session logs <name>` + `fno agents fleet trace <name>`.
  When the line also carries `pane="<session>:<pane_id>"` (a mux-pane worker), it
  has no log_path - relay the pane ref and give `fno mux attach <session>` (the
  line's own `hint=`) instead of `fno agents session logs`, which would miss.
- `result=launched ... mode=spawn ...` -> an autonomously seeded handoff worker
  launched through the provider's spawn lane. The default Rust substrate is a
  drivable pane; this is not a one-shot reply or a refuse-to-stop loop.
- `result=launched ... mode=seed ...` -> a verbatim free-text seed is running in
  a provider-native pane (the opening turn was sent as-is). Give `fno agents grid
  <name>` / `drive <name>` for live interaction and `fno agents mail send` for
  asynchronous follow-up.
- `result=launched ... mode=interactive ...` -> a drivable session **staged, NOT
  running yet**. Give `fno agents grid <name>` (or `fno agents drive <name>
  --mode interactive`). Note: no proactive push when it waits on input.
- `result=replied ...` (an explicit `headless` substrate one-shot) -> the one-shot returned its
  answer synchronously; the **reply follows the outcome line** and IS the
  deliverable. Relay it (preview ~15 lines; full reply in `fno agents session logs <name>`).
- `result=pending ... pane="<session>:<pane_id>" ...` -> a Codex pane was created
  and is running, but its rollout has not appeared yet so it has no addressable
  thread identity. This is a **normal outcome, not a failure**: report the pane as
  launched-but-not-yet-addressable, relay the pane ref and `fno mux attach
  <session>` (the line's own `hint=`), and say the identity binds on the next
  `fno agents registry reconcile`. The `short_id` on this line is the requested **name**,
  not a session handle, so do NOT present it as one and do NOT offer `resume`
  or a session-keyed peek until it is bound.
- `result=already-running ...` -> a worker already exists for this node/name and no second loop was created. Point at its logs. To replace a stuck worker, run `fno agents session stop <worker>`. Then spawn again with `--node <node>` (add `--resume <old session id>` on claude to carry the old transcript). The stop frees the node at once, so neither step needs `rm --force`.
- `result=self-handoff name=<n> reason="<...>"` -> **you** already hold the claim on
  that node, so `/fno:agent` cannot reassign it from here and nothing was launched.
  Relay the reason and the two real exits it names: hand off via `/fno:target`
  self-handoff (which archives state, emits the delegated event, and releases the
  claim atomically), or `fno backlog unclaim <node>` and then re-dispatch. Do not
  report this as a failure and do not retry the spawn.
- `result=failed reason="<...>"` -> report **FAILED** with the real reason. NEVER
  emit a short-id, fabricate a reply, or claim a worker launched.

