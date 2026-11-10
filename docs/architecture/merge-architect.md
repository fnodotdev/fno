# Final architect judgment

The merge door judges whether the pinned change solves its bound plan and fits the project's principles and vision. `merge.architect.enabled` defaults to `true`. Set it to `false` to skip the pass.

## Inputs and judgment

The reader receives the bound node, plan, named acceptance criteria, working principles, vision sources, PR body, diff and process warnings. The diff defaults to 80,000 characters. The changed-file list remains complete. The reader treats all supplied text as quoted evidence.

The diff and filenames come from an immutable base/head comparison through GitHub REST. An incomplete comparison produces a warning. When other plan sections lag behind, approved product criteria still bind the judgment. PR rationale cannot override those criteria.

`fits` allows the decision to continue. `misses_plan` requires quotes from each named unmet acceptance criterion. `off_vision` requires a quote from its named principle or vision source. The parser checks each quote against that source. A quote from another criterion or from the diff cannot create a hold.

The holding codes are `architect_misses_plan` and `architect_off_vision`. Missing inputs, unavailable readers, timeouts and invalid replies produce `architect_unavailable` warnings. Process evidence alone cannot create an architect hold.

## Store and concurrency

The owner is `crates/fno-agents/src/gate/merge_architect.rs`. Worktrees share `<space>/state/merge-architect/<hash>.json`. The hash covers repository, PR and exact head. A native per-head claim admits one reader. Publication is atomic and never overwrites an existing record. Unavailable attempts also persist, so another caller does not repeatedly spend on the same head.

Merge and Arm can run the bounded reader. Status and the raw merge hook read the stored record only. An absent record produces `architect_not_run`. A new head cannot inherit an old judgment. A live user grant for the exact head clears a negative judgment, and the warning receipt names that grant.

## Reader configuration

`merge.architect.timeout_secs` defaults to 900. The bound includes claim waiting and reader execution. `merge.architect.diff_chars` defaults to 80,000. `merge.architect.routes` selects named routes in order. `merge.architect.vision_paths` adds project-specific vision files, with no shipped project path.

The merge transport adds the reader bound to its existing guard budget. CI and authority are read again after the model wait. A newly recorded stop or code failure prevents authorization.

The reader reuses the blueprint judge's bounded subprocess and tool-free Claude arguments. Routes resolve through the native inventory. The default order tries `claude-opus-5-5`, the stock `claude-opus` route and `codex-sol`. A route without a supported tool-free reader warns and is skipped. It never launches an unrestricted reader to judge untrusted PR text.

## Manual calibration

The ignored calibration runs twelve bounded judgments against captured inputs. Its first sample is the restart-popup change, PR 4620. The other positive samples are PRs 4677 and 4642. Replace the node placeholders with each PR's bound node. `fno backlog list pr:4620 --json` reads the first binding.

```bash
python3 tests/py/capture_merge_architect.py --sample 4620:NODE_ONE --sample 4677:NODE_TWO --sample 4642:NODE_THREE --output /tmp/architect-input.json
FNO_BIN=/path/to/fno FNO_ARCHITECT_CALIBRATION_INPUT=/tmp/architect-input.json cargo test --manifest-path crates/fno-agents/Cargo.toml --lib merge_architect::tests::live_calibration -- --ignored --exact --nocapture
```

The capture verifies merged heads and complete file lists. Calibration adds an unmet product criterion, a conflicting Python runtime and an instruction attack in the diff. Each case prints its expected verdict, actual verdict, rationale and elapsed time. These fixture changes never enter the production verdict store.
