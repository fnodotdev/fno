# Review incident set

The bank's reviewer-scoring set, tagged `review-score`: one incident entry and two clean controls. Each entry pairs a diff with a metadata file, the tree files its catch needs, and one capability-tier bank task whose grade reads a findings file.

## Entry contract

Every entry is four files: `<name>.diff`, `<name>.meta.yaml`, the bank task, and the tree files its catch needs (under `tree/` at their real paths).

- incident: a merge that turned main red or was reverted, replayed as a fixture. The grade passes only when a blocking finding names the known bug site (recall).
- clean: a merged PR that passed the merge gate and was never reverted. The grade passes only when no blocking finding lands (false findings).

## The reconstruction rule

Real incidents on this public repository vendor verbatim. Incidents from private repositories are reconstructed from the bug class and marked `reconstructed: true` in the meta; the real coordinates live in the filing node's record only, never in public files.

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
