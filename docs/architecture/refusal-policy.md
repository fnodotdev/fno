# Refusal policy

A refusal stops an agent's command. Each one costs a retry turn, and often a wrong statement to the user. This page sets the test a refusal must pass. A guard that fails the test warns instead. A new or kept guard cites this page.

## The four-harms test

A guard refuses only to stop one of four harms. The ruling is d-714aeea7 (2026-11-07).

1. Destroyed work or data.
2. A leaked secret.
3. Spent money.
4. An irreversible public act.

Every other guard warns once and lets the command run. Style, clutter, a missing doc line, an extra PR and an unread canon doc are not harms. The warning names the rule and the fix, so the next call is right.

## Take the obvious action

When a refusal has one obvious right action, the guard takes it and reports it. `claude --bg` with no `--name` gets a derived name: the node the line names, else its working directory. The hook rewrites the call and says the name it used. A harness that cannot take a rewritten input runs the line as typed, with a warning.

A cancelled CI run produced no result. `fno do pr status` reads it as pending, never as a fail. A false red is a confident answer nobody checked (law d-504b027a).

## Guards that still refuse

| Guard | Why it refuses |
|---|---|
| `target start` over a live claim | Two sessions on one git index corrupt each other's uncommitted work. |
| Generated-artifact edit | The next regen discards the edit, so the agent's own work is lost. |
| graph.db, target-state.md and generated-copy writes | The edit is lost or corrupts shared state. |
| `--source-kind operator_request` with an empty turn queue | The stamp claims the user asked. With no user turn waiting, that is false provenance. |
| `outstanding clear` of a user question by an agent | It records an answer the user never gave. |
| `--authority chat_attested` without `--quote` | The quote is the provenance. |
| Mail to an unknown or wrong recipient | Mail to the wrong agent is lost or misread. |
| Mail with a raw command body sent without `--raw` | Text that reads as a command can run as one. |
| Dispatch hold and the merge approval box | A merge is an irreversible public act. |
| `pr push` with a dirty tree | The push leaves uncommitted work behind the PR. |
| `pr create` with no cut and no `Audited owners:` line | The test suite is shrink-only (AGENTS.md). |
| Review round cap | The two-round review law. |
| `fno doctor lint style` and `fno do pr status` exit codes | A check command's job is its exit code. These are contracts, not refusals. |

## Guards that now warn

- Mail style: the mail is delivered, and stderr prints the rule and the sentence.
- Tag charset: `release:0.4.8` is one tag. A space still refuses, because it splits the field.
- `backlog idea` with no `--difficulty`: the node files at medium, stamped `defaulted`, and the warning names the flag.
- Check-in recall word: the check-in completes and names the canon doc.
- Near-law lines on `law set`: a neighbor needs two shared topic words, not one role word such as "lead".
- A spawn payload that names a different command than the node derives: the named command runs, with a notice.
- `--route anthropic/<model>`: the native lane runs with that model.
- `pr create` docs line and sibling PR: the PR opens, and a notice names the doc rule or the sibling. The merge gate still reads the `Docs:` line.
- State-root, Claude config dir and config.toml writes: the write lands, and the note names the right place or command. An fd dup (`2>&1`) and the body of a quoted heredoc are not writes.
- Recursive `grep -r` beside a Cargo cache: the search runs. The note names `rg` and the Grep tool.

## How a PreToolUse refusal reads

Claude shows a hook deny as its own permission denial. So every fno PreToolUse refusal names fno and gives a way out. It reads in three parts:

```
[fno <guard>] <why it stopped>
Way out: <the safe form of the same intent>
Override: <how the person runs it anyway>
```

The contract table in `crates/fno-agents/src/runtime/hook/refusal.rs` holds the two last lines for each guard. A guard with no row fails the render test.

fno owns no permission layer, so the harness decides how a stop lands. On a Claude session that a person attends, a stop returns `ask`. The harness prompt is then the person's key to run the call once. Everywhere else the same text returns `deny`. Codex parses `ask` but runs the call anyway. An unattended worker (`CLAUDE_CODE_SESSION_ATTENDED=0` or `FNO_AGENT_SELF` set) has nobody to answer a prompt, and the agy and pi guard host has none to show. A call that got `ask` and then ran writes a `guard_decision` row with decision `override`. A guard with many overrides is in the way.

## Adding a guard

Name which of the four harms it stops. If it stops none, write a warning. If its fix is obvious, take the fix and report it. The refusal fold in [self-improvement loops](self-improvement-loops.md) counts refusals by command, so a noisy guard shows up there.
