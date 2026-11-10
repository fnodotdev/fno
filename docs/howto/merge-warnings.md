---
created: 2026-11-08T21:36
updated: 2026-11-08T21:36
audience: user
feature: merge-warnings
---

# Read merge warnings

Learn how to separate merge permission from review and CI evidence. Allow about two minutes to read status, plus any time needed to resolve a real code failure. You need an existing PR and access to its repository.

## Quick start

1. Read `fno do pr status <pr-number>`. Replace `<pr-number>` with the PR number. `verdict` describes CI. `ready` describes the current merge decision.
2. Read `merge_decision.warnings`. Missing looks, running reviews, missing coverage and pending or unreadable CI are evidence. They do not independently veto a merge.
3. Read `ready_blockers`. Resolve real code failures and explicit authority holds. If you have merge authority and the decision allows it, use `fno do pr merge <pr-number>`.

## Respect an explicit stop

A user's decline of a named PR head remains a hold at that head. Status reports `visual_declined`. When a new head is pushed, the old decline remains evidence but does not hold that new head. Explicit PR holds, freezes, merge-consent settings and head pins remain enforced.

## Read the architect judgment

When merge or arm needs a judgment for the current head, the final architect pass runs by default. `architect_misses_plan` names an unmet acceptance criterion. `architect_off_vision` names a project conflict. Each negative judgment quotes its source. Fix the change or use a user-authorized grant for that exact head.

Status only reads the stored judgment. `architect_not_run` and `architect_unavailable` are warnings. To disable the pass, set `merge.architect.enabled` to `false` in your configuration.

## Troubleshooting

### CI is pending but the merge decision is ready

The two fields answer different questions. A pending or unknown CI verdict remains visible and never becomes a false green. The native merge decision can allow the action while that evidence is pending. GitHub can still enforce its own required checks.

### A look is missing

The missing look produces warning evidence. It does not cancel CI or automatically file another approval question. Screenshots and design critiques remain useful for the work and for any explicit user request.

### A warning appears beside a refusal

Read the refusal separately. A warning never clears a user's hold, grants missing merge authority or authorizes a different head.

## Quick reference

| Field | Meaning |
|---|---|
| `verdict` | The CI result, including pending and unknown reads |
| `ready` | Whether the native merge decision currently allows the action |
| `warnings` | Evidence that does not independently veto merging |
| `ready_blockers` | Retained code, authority or integrity reasons |
