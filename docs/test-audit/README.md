# Test-audit campaigns

Running home for footnote's test-cut campaigns: one campaign PR per campaign key, one ledger per campaign in this folder. Method and junk patterns live in the test-audit skill (`skills/test-audit/`). Footnote campaigns run the keep rule below, which replaces the skill's value bar, retention bar and ledger shape. Target: at most 4,895 collected cases, half the 9,793 collected at 7ed2466c8c (2026-11-05). The declaration census reads 9,834 there, so its proxy target is 4,917. The Python ports met the earlier target of 15,447 declarations, half the 30,893 measured at 325d7e4ddb.

## Census

The count of record, re-run at each campaign base and after each merge. Command for Python (the naive `^def test_` count misses the class-method tests):

```sh
git ls-files -z '*.py' | xargs -0 grep -hcE '^[[:space:]]*(async )?def test_' | awk '{s+=$1} END {print s}'
git ls-files -z 'crates/*.rs' | xargs -0 grep -hcE '#\[(tokio::)?test' | awk '{s+=$1} END {print s}'
```

At 7ed2466c8c: 533 Python declarations and 9,301 Rust, 9,834 total. At 325d7e4ddb: 19,570 Python declarations in 1,115 files. 11,323 Rust in 772 files. 30,893 total, before parametrize expansion. The Rust command is the prefix form `'#\[(tokio::)?test'`: the `\]`-terminated form misses 92 `#[tokio::test(flavor ...)]` declarations.

## Keep rule

One contract, one keeper. The burden sits on the test. A deletion owes no per-test evidence.

Decision D2147 adds these rules:

- Measure collected cases and CI job minutes, not declarations. A fold that keeps every row counts as zero.
- A parametrize table keeps at most three rows per contract: the happy path, the refusal and one edge.
- A fail-closed guard keeps one refusal case, not one case per branch or lexer token.
- When a surface has a real-command or integration test, delete the unit tests of the internals beneath it.
- Each campaign scope cuts at least half of its collected cases. A ledger can stop short only by naming each kept test's sole contract and its credible failure.

- When its ledger row names the one contract a test alone guards, the test stays. It must also name a credible failure that turns it red. A test that cannot name both is deleted.
- When several tests guard one contract, keep the strongest boundary (the real command, store or transport). Delete the rest.
- When the text is a user-facing byte (a refusal string, config key, or path), the grep stays. This rule covers greps of docs, skills, workflows and source files. A behavior test must not cover the same text.
- When the dual-implementation inventory (docs/architecture/dual-implementation-inventory.md) marks the port complete, delete the test that forces both legs to agree. Principle 9: delete a leg. Never keep a harness forcing two legs to agree.
- Per-flag, per-key and per-default tests fold into one table test per surface. One row covers each distinct branch, not each value.
- When a second campaign scope uses a test helper or CI selftest double, its test stays. Otherwise the helper test is deleted.
- Slow, static or flaky is still not a reason to keep.
- Parametrize conversion is not a cut. The scoped pytest collected-case count must fall by the same half as the declaration count.
- The skill's contract families (public API, config, storage and the rest) are kinds of contract a test can name. They are not a reason to keep a test another test already covers.
- Never delete a test that is the only guard for a fail-closed path.

## Campaigns

Four groups, G1 to G4, one PR each (ruling D2992). G1 runs first and rebuilds the map. G2 to G4 then run together. The map (`campaign-map.tsv`) is the scope contract: a group edits only files its key names, plus its own ledger. Each group re-baselines on the origin/main it starts from.

The path rule lives in one place, [`scopes.tsv`](scopes.tsv). It classes each `*.py` and `crates/*.rs` file with a declaration. A file belongs to the first row whose path regex matches. The order is G4 (`^crates/fno/`), G1 (any `.py` file outside `cli/src/fno/`), G3 (the orchestration words), G1 (the graph words), then G2 (every other `crates/fno-agent` path). A file no row matches stays unmapped, like `cli/src/fno/paths_testing.py`.

## Regrowth cap

A scope row with a `max_net` caps the net test declarations one branch can add to that scope. The guards workflow runs `fno agent internal target test-delta --scopes docs/test-audit/scopes.tsv`, and a scope over its cap fails the check. G3 carries `max_net 0` (ruling D3448): a new G3 test must be paid for by a deleted G3 test. The repo-wide `--max-net 22` still applies on top.

Two gaps stay open by design. A pure rename into a capped scope emits no declaration lines in `git diff`, so it moves tests without counting. A test that drives G3 code from a path outside G3 counts in its own scope.

| Key | Scope | Collected at 7ed2466c8c | Target | Declarations | Round-two decls at `fa4eb5d2189` | Ledger |
|---|---|---:|---:|---:|---:|---|
| G1 | graph, backlog, claims and the Python remainder | 1,845 | 922 | 1,838 | pending | [g1-campaign.md](g1-campaign.md) |
| G2 | spawn, harness adapters, providers, hooks and the rest of fno-agent | 2,704 | 1,352 | 2,752 | 2,020 | g2-campaign.md |
| G3 | loopcheck, finalize, daemon, lead, PR, review, merge, mail, events, plan and target | 2,511 | 1,255 | 2,499 | pending | g3-campaign.md |
| G4 | the fno mux crate | 2,733 | 1,366 | 2,744 | pending | g4-campaign.md |

The K1 to K10 campaigns of 2026-09-28 are retired. K1, K2, K8 and K9 kept their ledgers in this folder.

## Ledgers

Each group writes `docs/test-audit/g<N>-campaign.md`. Only G1 edits this README. Parallel groups share no file. Ledger shape: a header with the base sha, declarations before and after, and collected cases before and after. Then one row per in-scope test file: `file | before | after | contracts kept`. Name each kept contract once with its credible failure. The preservation spot-check (five kept contracts, one mutation each, keeper goes red) is recorded in the ledger.
