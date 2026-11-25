# One authorized merge operation

`crates/fno-agent/src/gate/authorized_merge.rs` answers one question for every path that lands a PR. Is this exact head authorized to merge or to arm, and what happens next.

## Why it exists

Two callers used to answer that on their own. They answered it differently.

`fno do pr merge` ran a long guard chain. The plan hold, the in-flight review hold, the posture fold, the automerge floor, the coverage gate, the checks verdict, the base lineage. Then it merged.

`fno-agent finalize` armed GitHub's native auto-merge queue at a green terminal. Its chain was shorter and different. It never read the in-flight review hold, so a queue armed at the terminal shipped the code a review was still fixing. When it cannot read the covered head, it also dropped `--match-head-commit`. A racing push then landed an unreviewed head through the queue.

A guard on one of two reachable merge paths is decorative. The two chains are one chain now.

## The shape

A caller builds a `Request` and reads one `Outcome` back.

```rust
let outcome = authorized_merge::run(
    &authorized_merge::RealProbes,
    &Request { cwd, pr, effect: Effect::Arm, approved, auto_merge_source, .. },
);
```

`Effect::Merge` merges now. `Effect::Arm` hands the PR to GitHub's queue. The decision is identical. Only the effect differs.

The receipt keeps them apart. `Armed` is a queue entry, a promise GitHub keeps later. `Merged` is a merge that landed. A caller that reads one as the other stops watching a PR that never merged.

| Outcome | Means | Caller does |
|---|---|---|
| `Merged` | the merge landed. `note` says how. `cleanup_failure` names a post-merge step that failed | post-merge follow-ups |
| `Armed` | the queue owns it now | nothing; the queue merges on green |
| `Authorized` | a `decide_only` pass cleared; nothing ran | its own pre-effect step, then ask again |
| `Held` | retryable: a hold, a pending check, an already-armed PR | retry later |
| `Refused` | needs an operator: no grant, below the floor, a stale base, an unbound PR | escalate |
| `HeadChanged` | the head moved between validation and the effect | re-evaluate |
| `Unknown` | an instrument could not answer | retry; never read it as clear |
| `Failed` | the effect ran and failed | report |

By default the gate merges a green, mergeable PR as it is. Only a GitHub conflict pulls main into the branch. `auto_merge.require_fresh_ci` (default false) turns on the freshness gate this section and the next describe. With it on, the gate merges a PR only after its CI tested the current main. The same key arms the `base_overlap` hold, which holds a behind PR while parallel lanes run and the base move touches its files. The test is ancestry, not a timestamp. The gate compares the PR against its base (`compare/{base}...{head}`). A `behind_by` above zero holds as `ci_base_stale` only on file overlap: the newer base commits touch a file the PR changes. A behind head with no shared file merges (user ruling, 2026-10-07). A behind head whose merge has no conflicts also merges (user ruling, 2026-10-18). An unreadable file set holds as retry-later and takes no slot. The run-timestamp heuristic this replaced answered the wrong question. A run can start after the base tip moves yet still run at a head cut before it. An unreadable freshness read holds with `retry later`, never a merge. The gate covers `Effect::Arm` too. Main has no branch protection, so GitHub merges an armed green PR at once.

After a main merge, every open PR with older CI is stale. A stale PR can prove its merge result clean: the merge tree into the current base computes clean. The repo-wide static step also passes on it. That PR merges on the current settle, with no fold and no slot. Main CI and the main-red hold catch a combined break. Folding on every stale settle restarted a full CI run each time. On a base that moves faster than CI, the holder chased the fold forever, and green PRs starved behind the slot. A stale PR whose merge result cannot verify instead walks the fold. While the slot is free, `decide` holds it with `ci_base_stale` and gives it the slot. The holder then has GitHub merge main into its branch (`PUT pulls/<n>/update-branch`, pinned to the head it read). That is a merge commit, never a rebase, so CI reruns against the current main. The slot claim then records `retested` metadata, so the retest runs once per slot hold. The mark lands in whichever store the slot lives in: the rootless primary, or the legacy repo-root store a migration window can still hold. If the base moves again, that settle still merges on the real `gh pr merge` call. The review coverage carries over as `carried_base_sync`. A conflict or a failed call holds with the manual remedy: merge `origin/main` into the branch and push. The receipt says `merge_slot_held`. While a PR holds the slot, all others are `Held`, even with fresh CI. This keeps another merge from invalidating the holder's retest. When the holder merges or closes, turns red, takes a dispatch hold, or its 60-minute lease expires, the slot releases. A held holder can satisfy none of the other three, so a hold releases the slot. An admitted holder that arms keeps the slot until its merge lands. The read-only preview reads the same two exits. A proven merge, or a self-held slot with its retest spent, reads no stale blocker. pr status never holds a merge the gate will take. The 60-minute TTL covers one retest, the measured 31-minute rust-ci maximum, one 10-minute sweep tick, and margin. The lease uses `<canonical repo>/.fno/claims`, keyed `merge-slot:<base_ref>`.

## The decision, in order

1. Read the PR facts once and reject a terminal PR or an outside pull request.
2. Check merge consent. A per-run refusal, the live merge switch and the scoped head-grant rules retain their authority. The refusal binds only the session that wrote its manifest.
3. Bind the PR to its node. An unknown binding does not manufacture consent.
4. Respect a red main, with the existing declared-repair exception.
5. Respect dispatch holds, per-PR holds, freezes and explicit rules.
6. Pin the exact head from fresh PR facts, independently of review coverage. A moved or unreadable explicit pin never authorizes an unpinned effect.
7. Read lineage and the merge result. A proven code failure retains its outcome.
8. Collect review posture, coverage, in-flight review, looks, docs and plan fidelity as receipt warnings. Also retain overlap, flake, optional-review and eval evidence.
9. Honor a user's recorded visual decline only at the PR head it names. A later head carries that decline as evidence, with no automatic look veto. Read or run the [architect judgment](merge-architect.md). Only grounded plan or project conflicts hold. Status reads the stored judgment without running a reader.
10. Reject an unreconciled contract stub manifest.
11. Respect GitHub's actual merge refusal.
12. Apply the configured CI posture. Positive red CI remains a code failure. Pending or unreadable CI produces warning evidence. A green read still applies the configured fresh-base and merge-slot policy.
13. Apply the selected effect with the head pin and retain evaluated warnings in its receipt.

`fno do pr status` uses the same preview decision. Its CI verdict remains separate from `ready`: pending or unknown CI never becomes a false green. The warning list is present beside both allowed and blocked outcomes. These process demotions need no setting.

The raw merge hook and operator hold-check use the native dispatch-hold reader. Missing looks, review coverage and registered reviews warn without disarming GitHub's queue. An explicit user decline still vetoes and disarms only its named head. An open look question with screens that names the PR in its title holds the merge until the user answers it. The raw hook retains its separate merge-consent and lineage checks. It no longer requires a completed external-review artifact as a permission factor.

## Whose refusal binds

A per-run refusal belongs to the session whose manifest carries it. Every door reads it that way. `fno do pr status` hands the owner no posture. The fold in `authority_refusal` then scopes the refusal through `merge_grant::bound_posture_for_caller`.

`fno do pr merge` reads the manifest of the PR worktree itself. `manifest_posture` scopes that read through the same comparison, `merge_grant::refusal_binds`. The two doors then agree for one caller and one head.

A worker that ran `--no-merge` cannot merge its own PR. Its lead merges that PR on live config, with no grant row and no manifest rewrite. A successor session after a handover reads as the lead does. The stamp of the old session does not bind it.

A spawn brief that says who merges makes the worker pass that flag, because the worker must not merge. A `flag-no-merge` stamp on a lead-merged PR is then the common shape, and the stamp is true.

An unattributed manifest names no session. Init writes `harness_session_id: null` for a session it cannot prove. Such a refusal cannot be scoped, so it binds every caller and fails closed. The durable-grant lane merges for a parked worker. It keeps the refusal through its node-keyed fold, which no session scopes. Against an attributed refusal, a caller with no session id reads as no session, and the refusal does not bind it.

## The head-scoped user merge grant

The per-run no-merge refusal used to name only out-of-band escapes: merge by hand, or re-dispatch. That door is closed. The sanctioned remedies are decision rows at a subject that names the exact head:

```
merge-grant:<owner/repo>#<pr>@<40-hex head>
```

The PR URL supplies the subject's `owner/repo`. The PR number and head SHA complete the identity. An unreadable repository identity leaves the per-run refusal in place.

An operator records one in their own terminal, attended:

```
fno backlog decide 'merge-grant:owner/repo#2131@abc...' 'merge authorized for this head' --authority operator
```

A harness session records the other: the user's own chat approval, through the decision door, with the user's exact words as the rationale:

```
fno inbox decisions set 'merge-grant:owner/repo#2131@abc...' 'merge authorized for this head' --rationale 'user in chat: <the exact words the user wrote>'
```

The reader lives in `merge_grant.rs` (`head_grant_status`). When `decision_index::is_user_ruling` holds, the row counts: an `operator` row, or a `chat_attested` row carrying the user's words (a `quote`, or a rationale that starts `user in chat:`). A harness session cannot mint an operator row, because `fno backlog decide` refuses `--authority operator` from any agent session. A bare `chat_attested` row mints no words, so it proves nothing and stays invisible. The decision must equal `merge authorized for this head` exactly: row existence carries no polarity, identical duplicates read granted once, and disagreeing rows read conflicting.

Head invalidation is structural. A push changes the head, the subject no longer matches, and the new subject has no rows: the refusal stands until the user grants again. No manifest is rewritten.

The grant supersedes only the per-run layer. Live config, the posture floor, the holds, the pin, and every later guard still run. A merge that passes them carries `"merge_grant": "user head grant <short head>"` in its receipt.

## Outside pull requests and the owner's per-PR hold

The predicate lives in `pr_admission.rs` (`outside_reason`). When the head repo is not the base repo, the PR reads outside. A deleted fork reads outside. When the `author_association` is not `OWNER`, `MEMBER` or `COLLABORATOR`, the gate reads the author's permission on the base repo (`collaborators/{login}/permission`). An `admin`, `maintain` or `write` permission reads inside. Any other permission reads outside, and an unreadable one leaves the origin unknown. The association depends on the token: the App installation token reads a private org member as `CONTRIBUTOR`, and a user token reads `MEMBER`. The permission is the same under both tokens. The origin cache keeps the permission next to the association, and both re-read after 15 minutes, so a changed association or permission reaches the gate. Bots read outside like any other non-member author.

No outside PR is merged, healed, bound or reviewed by fleet lanes, and no decision row, flag or config admits one. The refusal names the rebuild path, never an admit command. Worth-pursuing work enters through its linked issue as a backlog node. Normal triage decides. The fleet builds accepted work on its own branch and credits the contributor with a `Co-authored-by: <login> <numeric-id>+<login>@users.noreply.github.com>` trailer. The PR steward below is the contributor's way in.

The decision reads the two gates in the order above: `outside_pr` refused (and `Unknown` on an unreadable origin read), `pr_hold` held. The preview walk carries the same two as blockers, so `fno do pr status` never says ready where merge refuses. `Effect::Merge` and `Effect::Arm` both pass through the decision, so finalize's auto-merge arm and the daemon's grant queue obey them with no change of their own.

The per-PR hold is a decision row at `pr-hold:<owner/repo>#<n>` whose decision text starts with `do not merge this pull request`. A rationale can follow after a colon. The specimen rows D3376 and D3379 carry one. The hold is not head-scoped, so it survives pushes. It applies to every PR, the owner's own included. Any lane and any authority holds. The specimens are coord-lane role rows, the subject is the identity, and the release is the supersede door. A row the store cannot prove closed still holds: `unscoped`, or `unknown` after a failed graph read. A row whose PR merged (`expired`) does not. Neither does one superseded, retracted or retired. Hold:

```
fno backlog decide 'pr-hold:<owner/repo>#<n>' 'do not merge this pull request' --authority operator
```

Release by superseding it:

```
fno backlog decide 'pr-hold:<owner/repo>#<n>' 'release' --authority operator --supersedes <decision id>
```

Recording a hold acts at once, at any authority, through either `fno backlog decide` or `fno inbox decide`. A lead holds a PR with this row, never with a mail hold. Mail reaches one worker. This row stops the merge door, auto-merge and the queue. The decide door disables an armed auto-merge queue with `gh pr merge <n> --repo <owner/repo> --disable-auto`. It converts the PR to draft with `gh pr ready <n> --undo`. If the repo lacks the `pr-hold` label, it creates the label, then labels the PR. A worker who marks the PR ready again does not lift the hold: the row decides, not the draft state. Every call is best-effort: a failed call never fails the record, and stderr names the manual command. The draft stops the UI and GraphQL merge paths server-side the moment the hold lands, even for a merge typed outside fno. The label makes the `ci-required` aggregate fail at the head (scripts/ci/ci-required.sh). Once D3383 requires that context in main's ruleset, every merge endpoint, a direct REST call included, is gated by the hold. A `release` row marks the PR ready again and drops the label. It reruns a failed `ci-required` run at the head, so the check re-reads the dropped label instead of waiting for the next push. The release acts only from an operator or role row. An agent release records the ruling and touches nothing on GitHub. The release rerun covers `ci-required` only. A red `ci-required` verdict can leave sibling runs cancelled at the head. Those stay cancelled until a hand rerun. Rerun a cancelled run whole: `gh run rerun <run id>`. The `--failed` form reruns failed jobs only.

The any-authority promise above needs the installed binary to carry the 2026-11-15 read fix and the 2026-11-16 record fix. An older binary still records the row and still runs the read. Its read demands the law lane, an operator row and the exact bare phrase. Its record fires hold effects only on an operator or role row. An agent-recorded inbox hold is invisible to that binary, and it fires nothing there. `fno doctor` reports the installed lag. `fno doctor update --rust` is the remedy. The specimen is D3472 (2026-11-18). An agent recorded it as an inbox hold at `pr-hold:fnodotdev/fno#357`. The deployed probe of the day did not read it. The lead re-set the hold from their own terminal. The release then needed a hand rerun of the cancelled sibling runs.

The origin facts never change for a PR: head repo, base repo, author association, login. The outside probe caches them per PR at `cache/pr-origin` inside the state root. One GitHub call per PR, ever. No TTL. Deleting the file forces a fresh read. That is also the remedy for an author whose association changed after the first read.

Heal skips every outside PR with a `skip_outside` receipt, fail closed on an unreadable origin read. Heal pushes to origin, the base repo, so it can never fix a fork's branch anyway. `guarded_push` refuses the protected branches (`main`, `master`, `develop`, `dev`) even under a force flag.

Both open-PR binding legs and the board's mergeable queue skip a row whose `isCrossRepository` reads true. Reconcile never binds a fork PR. Pr-watch, the grant queue, the review lanes and the board never see one. Only a positive true skips. A row without the field classifies as before.

Uncovered cases, kept on purpose: a hand bind of a fork PR with `fno backlog update <id> pr_number=<n>` still cannot merge, but nothing gates a review-fix dispatch onto it. The owner's raw `gh pr merge` is the owner's call. A same-repo branch from a non-member bot is caught by the merge and heal predicate but not by the binding legs, which read cross-repo only. A member who lost access keeps reading inside until the cache file is removed. A useful bot PR can never merge through fno. The owner merges it by hand.

### The PR steward

Heal's apply pass ends with one steward walk over the same open-PR pages it already read. The walk is `run_pass` in `pr_steward.rs`, called from `heal.rs` before the tick event. The gate is an operator decision row under the `outside-contributions` subject with a `global` scope, read through `fno backlog decisions outside-contributions --lane law --state live --json`. Any read or parse failure turns the steward off (`steward_off_no_law`), never on. A GitHub budget backoff also turns it off.

Per outside, non-bot PR the steward takes one action per pass, tracked in stage files at `cache/pr-steward/<owner>/<repo>--<pr>.json` inside the state root. With no stage: it reads the PR comments once. When no earlier steward marker is there, it posts the issue-first comment asking the contributor to open an issue and link it with `Closes #<n>`. It then reads the PR body for the issue link: a URL, an `owner/repo#<n>` shorthand, or a bare `#<n>` that is not a self-reference. It files the issue as an intake node with `fno backlog idea --source-kind from_observation`. The origin evidence is the issue URL, and the details carry a `Co-authored-by` credit trailer. Later passes hand the pull request diff, the linked issue, the contributor identity and a capped backlog-find view to a tool-less reader model. The view arrives as `UNTRUSTED` nonce-fenced blocks. The reader returns one verdict: `pursue`, `decline`, or `needs_clarity` with a sanitized question. The note lands on the intake node and, for `needs_clarity`, the question is posted to the PR. Every comment carries an HTML marker, so a rebuilt or wiped stage file costs at most one comments read and never a duplicate comment.

A pull request can close a claim issue instead. The issue title is `Claim <id>`, its author is the pull request's author, and a maintainer applied both labels. The steward then links the pull request to node `<id>` and mints no `Outside issue` node. It tags the node `claim-delivered`, releases the contributor's claim and adds a note. The pull request still never merges. The fleet builds the node from its own plan and credits the contributor with a `Co-authored-by` trailer.

Each pass also checks every outside pull request that has a node. When the node is done and its merge status reads merged, the steward posts one comment. The comment holds the merged pull request link and the `fno-pr-steward:merged` marker. The steward then closes the outside pull request. A marker already on the pull request means the comment is not posted again and only the close is retried. This is the close step the outside-contributions decision asks for.

## The merge freeze

A merge freeze holds a release window. The team sets it, and only the user lifts it. The code is `merge_freeze.rs`. With `store.remote_url` set, the record lives in the primary's `merge_freeze` table, so a freeze set on one machine holds a merge made from any other. Without it, the record lives in `merge-freeze.json` in the agents home, beside the fleet breaker. If the primary holds no record, a machine with a primary still reads its local file, so a freeze an older build wrote keeps holding. An unreachable primary holds the merge. The lift is a ruling, so it reaches another machine only through the shared backlog (`store.share_backlog`).

The record names a subject, a `set_by`, a reason, a repo and a pin. The pin is a candidate sha, a list of PR numbers, or both. A set with no pin refuses, because a total stop belongs to the fleet breaker. The team sets it through the `authorized-merge` command:

```
fno-agent authorized-merge '{"op": "freeze-set", "subject": "rc 0.4", "set_by": "lead", "repo": "owner/repo", "sha": "<40 hex>", "prs": [3170]}'
```

The pin key is `sha:<first 12 hex>` and `prs:<n>+<n>` in ascending order, joined by a comma. A PR on the pin's PR list merges. A PR of another repo merges. Every other PR of the frozen repo holds. Version 1 records (an `allow` list with no repo) still read, as a PR pin over every repo.

Five doors read the freeze. `fno do pr merge`, the pr-watch merge arm and the finalize auto-merge arm all reach `authorized_merge::run`, which reads the freeze as the `merge_freeze` probe beside the per-PR hold. The decision answers `held`, and `fno do pr status` lists a `merge_freeze` blocker. The fourth door is GitHub's own queue. A freeze set disarms every open armed PR of the repo that is off the pin, and the receipt lists them under `disarmed`. A failed disarm never fails the set. The receipt names the PR and the `gh pr merge <n> --repo <owner/repo> --disable-auto` command. The probe also disarms an armed off-pin PR at its next gate read. The pr-watch pre-check sends no repo, so it reads every PR as inside the frozen repo. That is an early skip, never a leak. The fifth is the target stop gate. A frozen PR is the `merge_hold` async wait. The worker holding on a freeze idles on `<watching reason="merge_hold">` instead of being re-invoked into work the user paused. The gate reads the freeze through `merge_freeze.rs::holds_pr_line` beside the merge-slot hold in `loopcheck/pr_read.rs`.

Only a live `operator` ruling at the lift subject lifts the freeze:

```
fno inbox decide 'merge-freeze:<owner/repo>@<pin key>' 'lift' --authority operator
```

The user records that row at a terminal (an `operator` row), or says lift in chat and a session records their words. The chat row is a `chat_attested` decision row whose rationale starts `user in chat:` or that stores a `quote` (`decision_index::is_user_ruling`). A bare `chat_attested` row does not lift it. Any process a harness session started can mint one, so only the user's words show the user ruled. The merge grant keeps the operator-only shape. A team or agent row at the subject does not lift it either. A row at another pin's subject does not lift it. A retracted lift row freezes the window again. An unreadable record or decision store holds the merge.

The `freeze-clear` op only tidies the record. It removes the primary row and the local file. Its `evidence` must be the decision id of a live `operator` ruling at the current lift subject. Free text refuses, and the refusal names the user's terminal door.

A new pin re-arms the freeze. While the current pin is unlifted, a set refuses. After the user lifts it, a set with the same pin refuses, and a set with a new pin writes the record. The new pin has its own lift subject and no ruling there, so it holds again.

A `release-pin` on the release record moves the sha of a live freeze on the same repo to the new pin. Only the sha changes: the sha admits no PR, so the move never widens the freeze. The new sha gives a new lift subject, so a lifted freeze re-arms. The record is in [distribution](../distribution.md#the-release-record).

The pr-nudge ladder pauses on a frozen PR, so its owner gets no merge retry while the freeze holds.

The freeze does not cover a merge through the GitHub web page or a bare `gh pr merge` outside fno. The fleet breaker's total stop (`fno agent incident stop --hold merges`) stays separate.

## Red main and the revert lane

While main's latest settled run is red, the gate refuses every merge to main. One lane is exempt: a PR whose node carries the `main-repair` tag. That lane is how main turns green again.

A cancelled run is not a settled verdict. A timeout, a hand cancel and a concurrency supersede all stamp `cancelled`. A cancelled head sha never re-tests until a rerun lands. So the gate reads a cancelled head run as pending and names the newest settled run as the floor. Specimen 2026-11-11: four CodeQL runs died at their own 30-minute job timeout. GitHub stamped those jobs cancelled. The gate then held every merge until a human reran one by hand.

The `main_revert` daemon arm opens that lane on its own. It is off by default. Turn it on per repo:

```toml
[main_repair]
auto_revert = true
revert_after_minutes = 10   # default 10
```

Each minute the arm reads main's verdict through the gate's own cached reading. If a red head stays red past the window, the arm walks main's first-parent chain. It follows parents while the workflow reads red and stops at the first green ancestor. The oldest `Merge pull request #N` merge in that red stretch is the culprit, so two merges that land before main's red settles name the older one. The arm then files a node tagged `main-repair`. It opens a PR that reverts that merge (`git revert -m 1`) and names that node. It asks the author of PR N to re-land the change with the fix. The revert needs one CI run. A fix forward can wait behind a flaky test and a CI queue: on 2026-10-26 main stayed red about 3 hours that way.

The arm refuses to guess. A walk that ends on a red-unproven verdict journals `ambiguous` and opens nothing. That verdict is a cancelled, pending or missing run, or a stretch deeper than one commits page. The same refusal covers a red stretch with no PR merge at all. A red whose failing checks all sit on heal's flake ledger journals `flaky`. Every outcome is one `main_revert_check` row per red head, so a daemon restart never opens a second revert. An opened revert for one culprit PR also stops a later red head over the same culprit from opening a second one: that head journals one final `deduped` row and closes. The arm never merges the revert. The owner merges it through this gate.

### A check for every merge

Every push to main runs `guards` and `rust-ci` in a group keyed by the commit sha, so no merge's run replaces another's. Together they take about 14 runner minutes. `fno do ci status <sha>` reads their push rows into `merge_check`: `green`, `red`, `pending`, or `unverified` for a cancelled or missing run. On 2026-11-03, 10 of the 10 newest failed `cli-ci` push runs had both lanes green. So `merge_check: green` means the commit compiled and passed the guards. It never means the suite passed, and it does not allow a release. The coalesced `cli-ci` run stays the batch and release gate, and `verdict` still reads it.

To list rollback candidates, walk the first parents and print the field:

```bash
for sha in $(git log --first-parent --format=%h -n 20 origin/main); do
  echo "$sha $(fno do ci status "$sha" | jq -r .merge_check)"
done
```

## The merge queue

A PR green on its own can still break main. On 2026-10-26 PR 3958 was green at 14:23Z, merged at 16:17Z over 48 newer main commits, and turned the `guards` job red on main. The merge queue tests each PR on top of current main before it lands. It is off by default. Turn it on per repo:

```toml
[merge_queue]
enabled = true
provider = "auto"   # auto | github | fno | off, default auto
gate_workflow = "merge-queue-gate.yml"
max_batch = 6   # default 6, the CI tier width
```

`provider` decides who lands the batches. `auto` (the default, so a repo needs no setting beyond `enabled`) picks GitHub's own queue when the repo can have one: a public repo owned by an organization (a free org is enough), or a private repo under an Enterprise Cloud org, and some workflow in the repo listens on `merge_group` so the required checks run on queue batches. Everything else - a personal-account repo, a private repo without Enterprise Cloud, a repo with no merge_group listener - runs fno's own queue. `github` forces GitHub's queue, `fno` forces fno's, and `off` disables queueing while `enabled` stays true. With `github`, `gh pr merge --auto` is the enqueue primitive: the arm path already arms it, and the fno arm skips the repo, so only one queue runs. Enabling GitHub's queue itself is a repo-admin ruleset change (a `merge_queue` rule with parameters like `grouping_strategy`; the legacy branch-protection PUT body has no merge-queue field), so setup prints the path and the operator runs it. `gate_workflow` names the file fno's own queue dispatches per batch; `fno config setup run` writes that file on repos without GitHub's queue, and the CI tier recognizes the configured name. With the knob on, the effect (next section) does not merge a PR whose base is `main`. Every gate before it still decides: review coverage, holds, the freeze, red main and the head pin. The effect writes one `merge_queue_entry` row for the cleared head and answers `armed`. A head the queue already ejected answers `held`, with the reason.

The `merge_queue` daemon arm reads the entries every 2 minutes. It takes up to `max_batch` of them in queue order: a `main-repair` entry first and alone, then node priority, then age. It creates the ref `fno-merge-queue/main/<batch>` at main's tip, adds one merge commit per PR through the REST merges API, and dispatches `merge-queue-gate.yml` on that ref. The gate runs the `guards` jobs once for the whole batch, then posts `stacked-base-guard` on the batch commit. The ruleset requires that context, so main can move only to a commit the gate passed. On green the arm moves `main` to the batch commit with a non-force ref update. GitHub marks each PR merged, because its head is now on main.

The queue never writes to a PR branch and never merges main into a green PR. The one exception is a PR that conflicts with the batch. The arm ejects it and asks for a merge of main.

On a red gate:

- When every failing check is on heal's flake ledger, the arm reruns the gate once.
- A red batch of several PRs splits. The next batch takes only the left half, and the right half follows on top of it.
- A red batch of one PR ejects it. The arm comments on the PR with the failing checks. A new push queues it again.

When main moves under a built batch, the arm drops the batch and builds again. After 3 rebuilds in a row it holds with `main_moving` until main stays still for one tick. A PR whose head moved, or that closed, drops out. The arm reads the heads again just before it moves main, so a push during the gate run rebuilds the batch. An entry has no expiry. It stays queued until it lands, is ejected, or its PR closes. While main is red, only a `main-repair` entry builds. Each act writes one `merge_queue_step` row, so a daemon restart resumes the batch.

The queue lands merge commits and ignores `auto_merge.merge_strategy`. A PR reads merged on GitHub once main contains its head. A squash drops that head. It does not cover a break that only the full suite finds. The revert lane above catches that on main. A human merge from the web button bypasses the queue, and the batch then rebuilds. The CI tier never cancels a `merge-queue-gate` run.

## The effect

`gh pr merge <n> [--auto] --<strategy> --match-head-commit <head>`.

The pin is never optional. It is the last guard between the decision and a racing push. If the head moved, gh refuses the whole call.

No `--delete-branch`, on either path. Its LOCAL delete fails from inside the worktree that holds the branch. That made the best-disciplined merge the one most reliably reported failed. Remote cleanup is a separate step.

A non-zero gh exit is never taken at face value. The PR's own state is re-read. When a post-merge step fails after the server-side merge landed, gh also exits non-zero. A branch another worktree holds recovers through the REST endpoint, which carries the same `sha` pin.

The recovery and the cleanup failure ride separate fields. A recovery that worked is how the merge landed, not trouble around it. Folded together, every worktree-held merge reported partial.

## Who calls it

- **`fno do pr merge`** (`cli/src/fno/pr/_merge.py`) asks twice. The `decide_only` pass comes first, so a merge about to be refused never publishes its coverage status receipt. The second pass re-runs the whole chain and closes the window the first one opened.
- **`fno do pr verify --kind merged`** (`crates/fno-agent/src/gate/pr_verify.rs`) asks once, then re-reads the PR before it reports. This command's name is its contract, so the receipt is not its last word.
- **`fno-agent finalize`** arms at a green terminal. What stays in finalize is what only the terminal knows: the merge-gating opt-out claim, and the optional-App review evidence.

Python reaches it through `fno.rust_binary.command_call("authorized-merge", payload)`, the single door. An unreachable binary answers `unknown`, never a clear merge. A merge whose authorization nobody read is not an authorized merge.

That includes a binary too OLD to know the command. A deployed `fno-agent` from before this landed answers `unknown`, so `fno do pr merge` holds at exit 2 rather than merging unauthorized. The remedy is `fno doctor update --rust`, and `fno doctor` reports the lag. Hold, never break, was the point of routing the door through one named refusal.

## After a queue-armed merge

When the queue lands a merge later, no fno process is in the loop. The remote ref used to stay behind forever, because the merge command's cleanup step never runs on that path.

The PR watcher tick already detects the confirmed merge. It pays the step there, through the merge command's own `_post_merge_remote_delete`. One cleanup implementation. No second watcher, no second store. Only the MERGED arm reaches it, so a pending or unreadable state never deletes anything. It is warn-only: cleanup never fails the merge it follows.

Local branches and worktrees keep their own lifecycle. See [worktree-mechanics](worktree-mechanics.md).

## Merge provenance

Every merge hop writes one `decision_span` row to the project journal beside the repo. The row carries `trace.actor_session` and `trace.actor_kind`, so "who merged PR N" has a durable answer that survives the session.

The merge owner (`authorized_merge::run`, via `crates/fno-agent/src/gate/merge_provenance.rs`) writes `merge_landed` or `merge_armed`. The `path` attr names the lane: `pr_merge`, `pr_watch`, or `finalize`. The gh proxy's draft door writes `merge_requested` with `path: gh_proxy` for every delegated gh merge argv. A PostToolUse Bash hook (`hooks/merge-capture.sh`, through the `graph-get` stdin door) writes `merge_requested` with `path: hook`. It is the backstop for gh that skipped the proxy.

When reconcile closes a node whose PR merged with no fno record anywhere, it writes `merged_outside_fno` with `merged_at` and `merge_sha`. No record means the user in the GitHub UI or another machine. The Rust close also stamps `closed_by: {session, actor_kind}` on the node row, and the daemon close adds `path: reconcile`. The close sweep and the detached finalize run with no ambient session. Their launcher stamps `FNO_ACTOR_KIND=sweep` on the child, so their rows read `sweep`, not `user`.

GitHub alone cannot tell a fleet merge from a user merge while both share one token. The query that answers the node's own record is `jq 'select(.type=="decision_span" and .data.pr==N)' <repo>/.fno/events.jsonl`. A GitHub App identity for fleet merges is the standing question that will make GitHub itself show the difference.

## A PR waits on its node's open blockers

A PR whose bound node carries an open `blocked_by` entry does not merge. The `blocked_by` probe reads the nodes the PR binds to, then the status of each blocker. An open blocker holds the PR as `blocked_by` in `fno do pr status` and in every merge door. When the blocker closes (done, or superseded and verified), the hold lifts. It needs no decision row, no question and no operator release. A blocker the graph no longer holds reads closed. An unreadable graph holds. Use a `pr-hold` row only for a judgment hold, such as a user look.
