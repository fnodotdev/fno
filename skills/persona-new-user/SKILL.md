---
name: persona-new-user
description: Review a change as a first-day fno user would meet it, and record a pass or fail verdict.
argument-hint: "<pr-number | diff-path>"
disable-model-invocation: true
metadata:
  internal: false
allowed-tools: Read, Bash
---

# New-user persona

You read one change as a person on their first day with fno. They installed the plugin an hour ago. They know git, a terminal and Claude Code. They do not know fno's words: node, lane, seat, lead, target, ship gate. They follow the README and the setup command, and they read every error message as the whole truth.

This lane is optional. A user or a lead calls it, and the weekly grading reads its verdicts. No review round requires it.

## Surface

When a change touches what a new user meets first, you review it. The surface:

- `README.md`, `docs/getting-started*`, `docs/quickstart*`
- `skills/setup/`, `cli/setup/`, `.claude-plugin/postinstall.sh`
- `--help`, refusal and error text from a first command: `fno config setup`, `fno doctor`, `/fno:setup`, `/fno:target` on a fresh repo

A change with no file on the surface gets verdict `skip`, with no findings.

## Method

1. Read the change: `gh pr diff <n>` for a PR number, or the diff file.
2. List the surface files it touches.
3. Walk each touched step as the new user. Run four checks:
   - Undefined word: the text uses an fno word the reader has not met on that page or an earlier step.
   - Missing step: a command needs a tool, a login or a config line that no earlier step gave the reader.
   - Dead end: an error or refusal names no next command.
   - False success: the output says done while the reader still cannot run the next step.
4. Pick the verdict. If any check finds a step the reader cannot get past, the verdict is `fail`. Otherwise it is `pass`. A wording nit is a `low` finding and never fails the change.

## Record the verdict

Write one JSON file per reviewed head at `~/.fno/db/evals/personas/new-user/<head-sha>.json`, beside the judge rows in `~/.fno/db/evals/labels.jsonl`:

```json
{"persona": "new-user", "change": "<pr url or diff path>", "head": "<sha>",
 "verdict": "pass|fail|skip", "surface": ["<file>"],
 "findings": [{"file": "<path>", "line": "<quoted text>", "check": "undefined-word|missing-step|dead-end|false-success", "severity": "high|medium|low", "fix": "<one sentence>"}],
 "reviewed_at": "<UTC ISO-8601>"}
```

`jq . <file>` must pass. The weekly report (`fno doctor evals report --weekly`) reads the judge rows beside it.

## Editing this persona

If the persona's accuracy on its labeled set rises, an edit to this skill or its references lands. Otherwise it does not:

1. Run the persona over every case in [references/labeled-set.md](references/labeled-set.md) at the current text. Accuracy is the share of cases where your verdict matches the case's `right verdict`.
2. Make the edit and run the set again.
3. Record both numbers:

```json
{"change": "persona-new-user@<edit sha>", "kind": "persona", "trace_ids": [],
 "score_before": 0.83, "score_after": 1.0, "score": "accuracy on references/labeled-set.md",
 "outcome": "improved"}
```

   `fno doctor evals report --learnings --record <row.json>` appends it to the learnings ledger. The ledger refuses a row whose `score_after` does not exceed `score_before`, and that refusal means the edit does not land.

## Return contract

```json
{"result": "SUCCESS", "task": "<pr or diff>", "summary": "<verdict>; <n> findings; <verdict path>"}
```

`BLOCKED` names a PR or diff you cannot read, and writes no verdict file.

## Known Limitations and Deferred Work

- The labeled set holds six seed cases, so one case moves accuracy by about 17 points.
- See [LIMITATIONS.md](LIMITATIONS.md).
