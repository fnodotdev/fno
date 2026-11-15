# Subtask containment

`contained_in` is the field that says: this node ships inside another node's PR. It has no PR of its own and never dispatches alone. The owner's merge closes the row only with evidence it shipped. The evidence: a declared surface in the changed files, a shared PR ref, or the owner PR naming it in the body or a commit. Without that evidence the child stays open. Reconcile notes the owner PR (see Surfaces). A PR that built none of its passengers must not erase them.

## Readers

Three surfaces read the field, and all three refuse or hide the contained node:

- `selection_guards` (`crates/fno-agents/src/backlog/backlog_ready.rs`) reads it before every ancestor walk and returns `contained:<owner>`, which holds the node out of autonomous dispatch.
- `redirect_if_contained` (`crates/fno-agents/src/gate/target_start.rs`) is the named-dispatch half: naming a contained node exits 2 with `ships inside <owner>'s PR` and claims nothing.
- When the owner's PR merges, `cascade_close_contained` (`crates/fno-agents/src/backlog/closures.rs`) closes each contained child the merge evidence names, stamping `shipped inside <owner>` on it. A child with no evidence stays open (see Surfaces).

A fourth rule rides the same field. Only a child that is NOT contained in its parent makes that parent a container, in `_container_ids`. An owner whose every child is contained is a delivery unit, so it drains instead of sitting undispatchable forever.

## Writers

Two commands stamp the field, both through the shared guards in `cli/src/fno/graph/_contain.py`:

- `fno backlog decompose <epic> --groups '[{adopt: [...]}]'` folds named nodes into a group child while authoring its plan fragment. A plan-less epic is refused there and pointed at the direct command.
- `fno backlog contain <owner> <id>...` folds existing nodes into any not-done, live owner, with no plan and no group scaffolding. It stamps `contained_in` and `parent` in one locked mutation, atomically across the batch. The command name matches the field: `contain` stamps `contained_in`. An earlier `adopt` command was a retired alias for `backlog intake`, so the name stays retired.

`fno backlog contain --release <id>...` is the direct inverse. It lifts each named node out of its owner in one locked mutation, and refuses a node that is not contained. `fno backlog update <id> parent=null` also un-contains a node, as it moves it away. The release also drops the PR refs the containment bind inherited from the owner and records `released_from`. The owner's `Fixes` line then no longer closes the node. Containing it again clears the marker.

## Surfaces

`fno backlog contain <owner> <id>... --surface <repo-relative-path>` stamps `containment_surfaces` on each contained child. Pass the flag once per path. A child closes on its owner's merge only with evidence its work rode the owner PR, and any one of three kinds closes it. The first: the merged PR changes a file that matches a declared surface. The second: the child's row shares a PR ref with the owner. The body's closure line writes that ref at create time. A later bind heals it. The third: the owner PR names the child outright, in its body closure line or in a commit message. A declared child with no evidence at all releases the containment. The row drops `contained_in` and the owner's PR refs, keeps `parent`, and records `released_from`. The row stays open and dispatches again. An undeclared child with no evidence also stays open. The merge holds it, and reconcile reports it with the owner PR. A PR that built none of its passengers must not erase them. The stranded sweep runs the same gate with no file evidence: a bind closes, a declared child releases, an unnamed child holds. Name an unnamed child in the owner PR body's `Fixes` line, bind its PR, or close it by hand.

## Guards

`contain_into` runs its guards in order. A live worker on the target refuses the fold: a node someone is building is a delivery unit in practice. Then a cycle check. Then the one-level check: a target with children leaves those children dispatchable while their parent closes. Then the mid-flight check: an open PR or accrued cost is independent delivery evidence, and a node carrying either is refused while not done. A done node with a PR or cost is parented but containment is withheld, loudly: it already shipped on its own.

A dead owner, deferred or superseded, is refused outright by `refuse_dead_owner`: a dead unit's merge never closes its children, so only `contain --release` ever frees them.

## Contain, not defer

Defer and containment answer different questions and are released by different hands. Defer is a judgement any groom pass can make, it hides the row from the board, and a contained row stays visible. Containment is released only by the owner's merge cascade or by moving the node away. Never defer a node to mean "this ships with that PR": contain it, then undefer, so the node is never armed for dispatch in between.
