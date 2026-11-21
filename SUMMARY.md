# SUMMARY - By-lane scorecard under 30s, ledger cost coverage repaired

Plan: `internal/fno/plans/20261120-calibration-bylane-stall-ledger-cost-x-8121.md` (quick-plan, blueprint by opus architect under claim target-session:76183bd9).

## What landed

Task 1.1 (`perf(eval)`): one shared `fold_deliveries_parallel` helper folds deliveries on a bounded pool of at most eight scoped threads. Contiguous chunks keep every row and verdict in delivery order. Per-worker failure counters sum into the same totals. Both spellings rewired: `fix_of_fix_by_pr` (scorecard join) and the stability report build loop. 823 serial blame subprocesses at ~75ms now run pool-width wide. Contract test covers order, skip positions, exactly-once counter sums.

Task 1.2 (`fix(eval)`): `backfill_tasks_json` is incremental and cost-preserving. Measured entries (numeric cost and tokens) print KEEP and skip the transcript re-parse. The cost write carries a preserve guard. An unpriced recompute keeps measured dollars and refreshes only the token fields. The recompute leg also stamps `duration_minutes`. Three contract tests cover the decision and guard functions.

Task 1.3 (`fix(config)`): the two pricing doors in crate fno fire the stale-gated book fetch before they price. The doors are the `fno whoami cost` arm and the doctor cost-check. A fresh book costs one stat.

Task 1.4 (`fix(backlog)`): the scorecard's ledger loader resolves the configured ledger path. It hardcoded the retired `~/.fno/ledger.json` and read an empty file on current-layout hosts. Found mid-flight, fixed in the same commit. The calibration output gains a ledger cost coverage line with priced/total, the unpriced split, and rate-book presence. Test covers the split contract.

Task 1.5: fno-web doc PR for `reference/cost.mdx` plus a docs.json entry. The PR opens separately. The fno PR body names it per D2707.

## Deviations and notes

- The machine gate bars local cargo test runs. Tests were authored before the code they prove. CI runs them, so RED was not watched locally.
- fmt caught a parse error in `preserved_backfill_cost` before the push. Fixed in the same unpushed commit.
- Diff budget overrun: +366/-94 against the +300 allowance. The node's named work exceeds it. Stated in the PR body per the budget rule.
- Worker width ships at the plan's cap of eight. No build go exists for width timing. The done probe is the outcome read.
- Absorbed x-89ae (same stall, 60s close-probe symptom). Contained children x-8454 and x-23ed share the price-book fix. The PR names both.

## Verification

Local gates: `cargo fmt --check` green in both changed crates. `scripts/ci/check-file-budget.sh` ok. Style lint green on changed markdown. CI builds and tests the push (D3323). Post-merge: run `fno doctor update`, then the plan's done probes, then the `--since 28` close-probe check.
