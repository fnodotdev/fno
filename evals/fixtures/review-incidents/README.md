# Review incident set

The bank's reviewer-scoring set, tagged `review-score`: one incident entry and two clean controls. An entry pairs a diff and a metadata file with a bank task and any tree files the catch needs.

## Entry contract

Every entry is four files: `<name>.diff`, `<name>.meta.yaml`, the bank task, and the tree files its catch needs (under `tree/` at their real paths).

- incident: a merge that turned main red or was reverted, replayed as a fixture. When a blocking finding names the known bug site, the grade passes. That is recall.
- clean: a merged PR that passed the merge gate and was never reverted. When no blocking finding lands, the grade passes (that is the false-findings read).

## The reconstruction rule

Real incidents on this public repository vendor verbatim. Incidents from private repositories are reconstructed from the bug class and marked `reconstructed: true` in the meta. The real coordinates live in the filing node's record only, never in public files.

## Adding the next incident

1. When the incident lives on this repository, write the diff to `<name>.diff` verbatim.
2. Write `<name>.meta.yaml`: coordinates, the bug site, and the defect sentence.
3. Vendor under `tree/` only the files the catch needs, at their real paths.
4. Add the bank task and prove both arms of its grade with the grade fixtures.

## Files

| File | Entry | Kind |
|------|-------|------|
| `incident.diff` + `incident.meta.yaml` + `tree/scripts/ci/reachable-paths-baseline.txt` + `evals/bank/capability-review-incident-reachable-paths.yaml` | founding incident | incident (reconstructed) |
| `fno-415.diff` + `fno-415.meta.yaml` + `evals/bank/capability-review-clean-backlog-perf.yaml` | PR 415 | clean |
| `fno-416.diff` + `fno-416.meta.yaml` | PR 416 | clean |
| `grade-fixture-faithful.md` / `grade-fixture-clean.md` | - | probe fixtures |
