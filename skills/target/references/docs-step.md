# The docs step

Law D2707: every user-facing change ships its user doc in the same change set. The doc is written with the fno-doc-writing skill and checked with the asd-ste100 skill. It lands in fno-web at `fno-sh-docs/` (guide or reference). The fno PR names it in a `Docs:` line. This step makes the implementing worker write the doc before review. No docs lead catches up after merge.

## Trigger

After validate, before review:

```bash
fno do docs surface --diff-base origin/main
```

An empty list means the change is internal: skip the step. A non-empty list means the change is user-facing: run the step below.

## The fork writes the doc

On the claude harness the doc step is a fork of this session. A fork inherits the parent's system prompt, tools and conversation history exactly. Its first request reads the parent's prompt cache. It starts with the full change context: what shipped, why, which commands changed, what the user types. The docs tool output stays out of the worker's context.

Launch it with the Agent tool, `subagent_type: "fork"`, and this mission brief:

```
Write the user doc for the change this session just built (D2707).

1. Load /fno-doc-writing and /asd-ste100.
2. Pick the page type and whether it is a new page or an edit to a live page.
3. Work in an fno-web worktree, never a shared checkout. Create a docs branch docs/<node-or-topic>.
4. Write the page in fno-sh-docs/ following fno-doc-writing's page types.
5. Stamp it: `fno do docs bind <page> --nav <nav-group>`. Add --release <tag> when the derived tag is wrong for this window.
6. Rerun the gate: `fno do docs gate --apply`.
7. Run the reader test as two fresh probes, page text only. Fix and rerun.
8. Commit and open the paired PR in fno-web with gh. The body names the fno change it documents.
9. Report the paired PR number and URL.
```

Limits: a fork exists only on claude. Other harnesses launch a plain subagent instead. Hand it a written change summary (what shipped, user-visible behavior, exact commands, what a user sees or types now). The subagent works the same steps from that summary. If the fork fails mid-step, fall back to the summary route or finish the doc inline with the two skills loaded.

## Reader test stays fresh

The reader test never runs inside the fork. A fork carries the whole change context and reads the page like the author. The probes are fresh subagents. They get the page text only, inline or from a neutral path outside both repos. The fork launches the probes. Fix what they report, then rerun. STUCK, guesses, NOT IN PAGE, or a confident wrong answer is a page defect.

## Bind and the gate

`fno do docs bind` decides new-page stamp or live-page twin. A new page gets `since:`. Pass `--nav` to stamp `nav:`. The release tag comes from `--release` first. Without the flag, bind derives the newest `since:` among pages whose tag is not yet live. With no derivation, bind refuses and names `--release`. An edit to a live page becomes `<page>.next.mdx` with `since:` and `replaces:`. The live page is never touched. Both forms rerun `fno do docs gate --apply`. The gate script is the sole writer of the generated `.mintignore` block and the docs.json nav.

The release-tag window: after a tag exists but before the docs flip lands, the derivation can name the shipped tag. Read the printed `since:` line. When it is wrong for this change, re-run bind with `--release <tag>`.

## The Docs: line

The step returns the paired PR number. The ship phase writes it into the PR body this session drafts for `fno do pr create`:

```
Docs: fno-web#<n>
```

When a user-surface PR carries no such line, the merge door holds docs_line_missing. The escape is `Docs: none, <reason>` with a reason. A held merge is not an escalation. Add the line and re-run the merge command.
