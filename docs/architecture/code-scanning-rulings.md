# Code-scanning rulings

Code-scanning alerts on this repo are ruled at the code level first. A finding whose pattern is gone from the tree closes by the scanner's own re-analysis. A finding that lingers past a fresh analysis takes a dismissal from a seat with the scope to file one. This page records the constraint every ruling run hits. It records the per-instance verdicts of the 2026-11-25 pass over the 16 surviving alerts. It records the house pattern for the workflow class. It records the commands the scoped seat runs. It is a record and a procedure, not a discussion.

## The seat constraint

A worker seat cannot read or dismiss code-scanning alerts, and the wall is scope, not flake. The gh CLI's OAuth token (scopes: delete_repo, gist, read:org, repo, workflow) is refused on every route. The alerts list and the analyses list answer 403 "Resource not accessible by integration". An unauthenticated curl of the same endpoint answers 401. The public security tab answers 404 to a plain fetch. The Actions artifacts of a successful CodeQL run carry no SARIF, because the upload goes through the code-scanning API itself. The repo's Actions variables, which hold `CODEQL_ADVANCED_SETUP`, answer 403 too. GitHub's docs set the bar the token misses: alert reads want the `security_events` scope. A ruling run stops re-deriving these refusals and hands the close-out to the scoped seat at the bottom of this page.

Which scanner owns the analysis decides what "ingested" means. This repo's CodeQL workflow is advanced setup and skips until the `CODEQL_ADVANCED_SETUP` variable flips true (.github/workflows/codeql.yml:3-5, gate at :22). Until then the default-setup scanner owns scanning, and its runs are the workflow-file-less "dynamic" runs the CI-tier arm refuses to cancel (docs/architecture/pr-heal.md:85). Default setup re-analyzes on pushes to main. A remediation that lands on main closes its own alerts. A lingering alert past a fresh main analysis is a scanner artifact, not a finding.

## The 16 alerts of the 2026-11-25 pass

The enumeration below is instance-keyed exactly as the filing left it after the earlier orphan-dismissal pass. The 16 instances break into four classes. Ten are cache-poisoning alerts on the release workflows. Three are hard-coded-cryptographic-value alerts on `verify_evidence.rs:1555-1559`. Two are js/xss alerts on `web_page.html:339,401`. One is a js/xss-through-dom alert (GitHub alert number 38) on `web_backlog.html:2044`. Every verdict was measured on the tree at main `1b0977ce4f` on 2026-11-25. The evidence cites the command or file range that produced it. The id-keyed open list itself is unreadable from a worker seat (see above). The verdicts therefore key on these instances, and the scoped seat keys its dismissals onto them by id.

| Instance | Ruling | Evidence | If it lingers, dismiss as |
|---|---|---|---|
| 10 cache-poisoning, release workflows | fixed on main, auto-close expected | the plain-git pin is landed: release-binaries.yml:51-55, release.yml:722-737; neither release workflow takes an untrusted `ref:` checkout | `mitigated` |
| 3 hard-coded-crypto, verify_evidence.rs | removed by the 2026-11-05 test cut, auto-close expected | the flagged strings were test vectors: `"n1"`, `"n1"`, `"wrong"` inside a `#[cfg(test)]` test at d6af0e583f:1550-1561 (1,561 lines then); today's crates/fno-agent/src/gate/verify_evidence.rs is 1,151 lines, holds no test module, and reads the nonce from args and event data at :82-135 | `used in tests` |
| 2 js/xss, web_page.html | removed by the rewrite, auto-close expected | the page now at crates/fno/src/serve/web_page.html builds rows as DOM by design (:206) and its sink sweep over `innerHTML`-class patterns finds comments only (:144-146) | `mitigated` |
| alert 38 js/xss-through-dom, web_backlog.html | no instance remains in the tree | today's web_backlog.html:2044 is an `appendChild` builder; the file holds zero HTML-string sinks; the one `href` assignment (:1330) builds on `URL` + `searchParams`, never card text (:447-449), PUBLIC-gated (:1328) | `mitigated` |

Two of these deserve their finding notes. The cache-poisoning class was the untrusted-checkout shape. A workflow `checkout` step took its ref from `needs.resolve.outputs.sha` or a workflow-call input. A crafted ref then decides what a cache-writing job builds. One other workflow still writes a checkout `ref:`. Review-coverage-gate.yml:109 takes `github.event.pull_request.base.sha`, the GitHub-computed base commit. The workflow pins the base version on purpose, so a PR cannot edit its own gate script (:102-104). Read that intent before calling it a finding. The fix commit that named it ("the container proof pins its ref through plain git", 3e8376b9 on the pre-migration history) exists on this remote only through old refs. Its diff landed on main under the rewritten history, so the sha is dead but the remediation is not. The crypto class was never a secret. CodeQL's query reads the literal `"n1"` in a test's nonce comparison as a hard-coded key. That is why the dismissal vocabulary has "used in tests".

## The cache-poisoning house pattern

A workflow job that needs a resolved commit takes no `ref:` argument on its checkout. A pin step applies the sha through plain git instead: `git fetch origin "$REF"` with the ref in an `env:` block, then `git checkout --detach FETCH_HEAD`. Standing specimens: .github/workflows/release-binaries.yml:51-55 (the workflow-call input case) and .github/workflows/release.yml:722-737 (the resolve-outputs case). Any new job that checks out a sha an input or another job's output produced follows this shape. A checkout whose `with: ref:` names anything not literally written in the workflow file is the flagged one.

## What a seat with security_events runs

The commands below settle the pass in one sitting. Read the open list, dismiss each alert the table above rules closed, and re-read the list to confirm the keyed instances read closed or dismissed. The reason words are GitHub's `dismissed_reason` enum, read off its OpenAPI description on 2026-11-25: `false positive`, `won't fix`, `used in tests`, `mitigated`.

```bash
# The read a worker seat cannot do: the id-keyed open list.
gh api "repos/fnodotdev/fno/code-scanning/alerts?state=open&per_page=100" --paginate

# One dismissal per lingering alert. PATCH is the "Update a code scanning
# alert" operation; the comment carries the evidence so the alert thread
# keeps its footnote.
gh api --method PATCH repos/fnodotdev/fno/code-scanning/alerts/<n> \
  -f state=dismissed \
  -f dismissed_reason="mitigated" \
  -f dismissal_comment="ruled closed 2026-11-25: pattern absent at 1b0977ce4f; see docs/architecture/code-scanning-rulings.md"
```

A dismissal is a judgment with a name on it, so run the table, not the mood. The reason follows the instance's row, and the comment cites the same evidence the row cites. When the list reads empty for every keyed instance, the pass is done and this page needs no edit. When a new finding class appears, rule it here in a new row before closing it.
