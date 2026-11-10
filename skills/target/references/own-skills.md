# Run your own skill in a target loop

When a person wants their own skill to run in every target run, read this page. Examples are a security checklist or a house-style pass.

Target has one door for this. Register the skill as a reviewer, then name it in `review.reviewers`. There is no `target.*` key and no phase swap. The registered reviewer is the whole feature.

```toml
[review]
reviewers = ["house-check"]

[review.reviewer_registry.house-check]
kind = "harness-skill"
requires = "skill"
invocation = "/house-check"
asserts = "invocation"
```

## What happens in a run

1. `fno do target init` looks for the skill in `~/.claude/skills` and `<project>/.claude/skills`. When it finds the skill on no root, init refuses and names each root it searched. A skill that init cannot check (a `plugin:skill` name, an unknown harness) gets one note, and the run goes on.
2. On the final HEAD, before the promise, the target session runs the `invocation` text. Then it runs `bash skills/review/scripts/emit-attestation.sh house-check`. The steps are in [ship-and-promise.md](ship-and-promise.md).
3. The stop gate refuses `DonePRGreen` until `house-check` attests at the current HEAD. A new commit makes the attestation stale, so the skill runs again on the new HEAD.

## Two rungs

- `asserts = "invocation"` proves that the skill ran at this HEAD. It claims nothing about the result. footnote does not parse the skill's output.
- To make a real gate, let the skill file its findings with `fno backlog note <node> "<finding>" --blocking`. An open blocking finding holds the loop until someone resolves it with `fno backlog note --resolve <finding-id>`.

When the check is a command with an exit code, a plan can name it under `done_probes` instead. The stop gate runs each probe and needs no skill.

## What this does not cover

- A skill that must run at an earlier moment, for example after the first wave. The attestation is pinned to the head, so an earlier run goes stale at the next commit.
- A swap of fno's own execute, review or ship phase. Those phases are fixed ([phase-invocations.md](phase-invocations.md)).

The reviewer-side contract is in the review skill: [Your skill can be someone's ship gate](../../review/SKILL.md#your-skill-can-be-someones-ship-gate).
