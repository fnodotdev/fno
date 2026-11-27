# Scheduled runbooks

A scheduled runbook is a scheduled spawn whose prompt is a runbook file. The agent runs the steps, and fno runs each step's command itself, so every receipt records what ran. The schedule half is [scheduled spawns](scheduled-spawns.md). This page covers the runbook half.

## The command

```bash
fno agent spawn --when 20:00 promote --runbook ~/ops/promote.runbook.md
fno agent spawn --when 20:00 promote-dry --rehearse --runbook ~/ops/promote.runbook.md
```

`--runbook <file>` makes the file the prompt. A positional prompt, `--message` or `--prompt-file` beside it refuses: a spawn has one prompt source. `--rehearse` skips every step marked irreversible. A skipped step still gets its receipt. `--dry-run` was taken already, so the rehearse run has its own word.

At schedule time fno parses the file. A bad runbook refuses before any row exists, and the error names the field. The row records the absolute path. At fire time the spawn seam parses the file again, writes the run file header and gives the agent a fixed prompt. Without a schedule flag the same seam runs at once.

## The format

The YAML frontmatter is the contract. The body below it is prose for the agent.

```yaml
runbook: promote-local-to-prod
cwd: ~/code/readyrule/regready-ccld-pipeline
preflight:
  - id: local-db
    run: psql -h localhost -p 54322 -U postgres -d postgres -tAc "select 1"
steps:
  - id: facdetail-dry
    run: python scripts/promote_local_to_prod.py --step facdetail --dry-run
    check: the dry-run line reports a facdetail count above zero
  - id: facdetail
    run: python scripts/promote_local_to_prod.py --step facdetail
    check: the run reports rows upserted equal to the dry-run count
    irreversible: true
stop_when:
  - any check fails
rollback: none - every upsert is COALESCE or ON CONFLICT DO NOTHING; a re-run is safe
```

Each step takes `id`, `run`, and optionally `check`, `irreversible` and `timeout_s` (default 3600). Step ids are unique. A preflight check is never irreversible. `rollback` is a list of `{id, run}` or the text `none - <why>`. An unknown key refuses.

## The receipt door

The agent runs every step through `fno do runbook`:

| Action | What it does |
|---|---|
| `step <run-file> <id> [--yes <question-id>]` | Runs the step's command under `/bin/sh -c` in the runbook `cwd`, and appends a receipt with the command, exit, and output tail. |
| `check <run-file> <id> --read <text> --verdict pass\|fail [--count name=value]...` | Records the agent's read of a step's check, with its counts. |
| `rollback <run-file>` | Runs the rollback after a failure, or records the `none` reason. |
| `close <run-file>` | Writes the close record and mails the scheduler the run file path. |

The door refuses these steps:

- a step out of order
- a step after a failed check or a nonzero exit, until the rollback runs
- a step whose earlier check is not read yet
- a step after the runbook file changed (the header holds its sha256)
- an irreversible step in a live run without a valid `--yes`

## The irreversible pause

A live run stops at an irreversible step. The agent asks the user with `fno inbox outstanding ask`, and the question names the run id and the step id. When the user answers, the agent runs the step again with `--yes <question id>`. The door accepts the yes on three facts:

- the question names this run and this step
- the user answered it on their own answer path, not a delegate or a sink close
- the answer starts with "yes"

The receipt records the question, the answer and who closed it.

A schedule cannot pre-approve an irreversible step yet. That waits on an open question about what proof of pre-approval counts. Until it is answered, every irreversible step in a live run pauses.

## The run file

One JSONL file per run lives at `~/.fno/agents/runbook-runs/<run-id>.jsonl` (`FNO_AGENT_HOME` overrides the agents home). The records are:

- `header`: run id, runbook path, sha256, rehearse, scheduler, cwd, the declared steps
- `receipt`: one per step, with `check` set to `pass`, `fail`, `pending` or `skipped`
- `check`: the agent's read for a pending check
- `rollback`: one per rollback command, or the `none` note
- `close`: `steps_declared`, `receipts`, `stopped_at`, `rolled_back`, `verdict`, `mailed`

A complete run has `receipts == steps_declared` on its close. The file is a permanent audit record, and nothing sweeps it. A fire that the spawn gate refuses after the header is written leaves a header-only file. The next retry starts a new run file.

## Limits

- A check that needs judgment rests on the agent's read. The receipt records that read beside the command output, so a person can audit it later.
- One runbook runs on one machine. A runbook whose steps span two machines is out of scope.
