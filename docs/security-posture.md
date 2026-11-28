# Security posture

## The loop is for plans you trust

footnote's autonomous loop (target) is a delivery harness. When you point it at a plan, it executes the plan with your credentials, on your machine, against your repos. By default there is no sandbox beyond standard git worktrees. An optional [sandbox](architecture/sandbox-host.md) runs headless workers in a devcontainer, and it is not a security boundary.

This is intentional. The default stays sandbox-free. A solo founder running plans they wrote fears a mistake, not malice. Mistake recovery is what git, atomic commits, and human review are for.

This is also a constraint when footnote is shared with others.

## The trust boundary

You must run the loop only against plans you have read or trust the author of. Specifically:

- **Plans you wrote:** safe by definition (modulo your own mistakes).
- **Plans someone you trust wrote:** safe to the degree you trust them.
- **Plans from the footnote repository's `examples/` folder:** reviewed and safe.
- **Plans from random contributors, bug reports, gists, or untrusted sources:** read them carefully before running. The loop will execute them with your credentials.

## What a sandboxed worker cannot do

`[sandbox] provider` runs each headless worker in a container with no mount and no network ([sandbox host](architecture/sandbox-host.md)). The attacker in that model is the sandboxed process: it controls its container and forges every byte it sends. The host user, the daemon, the fno binary and the engine are trusted.

- It cannot see the host login, keychain, home, engine socket or another spawn's route key. Nothing is mounted, and keys cross exec stdin to this sandbox only.
- It cannot spawn, ask, stop, remove, rename or list other workers. The guest passes status, report, session report and the typed loop ops only. The host stamps who sent them.
- In the target loop it cannot merge, attest its own review, decide or update the backlog. It cannot name another node, branch, repo or recipient: the host binds them at spawn. Mail goes to its spawner only, labelled sandbox origin.
- It cannot push to a default branch, rewrite the bound branch or push a workflow or action edit. The push is fast-forward only, with hooks off.
- Its PR text cannot act as authority. Before the host posts, it breaks markers, mentions, closing words and other node ids. The host writes the owners line and the closure trailer itself.
- Its Stop cannot end on a forged fact. The host runs the one Stop decision from GitHub and the export. A sandbox run is no-merge and ends `DoneUnreviewed` at best, so a person or an outside reviewer reviews and merges.
- It cannot write the host worktree or `.git`. Its commits land under `refs/sandbox/<id>/head`, fetched with hooks off. No host branch moves, and the host never applies its dirty work.
- It cannot reach host loopback, the LAN, a tailnet, cloud metadata or a host-owned address. The proxy refuses every private range and every port but 80 and 443.
- It cannot turn the sandbox off from a project file. A project layer can only raise the sandbox.
- Its transcript cannot become a user turn, a law witness, operator capture or an account cap. Only display readers read it, labelled as sandbox origin.

What remains:

- A person who checks out a sandbox ref runs untrusted code. Read it first.
- The sandbox can send its own key and the source out through a public host.
- Transcript prose, notes, mail and PR text can still mislead a person who reads them.
- Host-side gates act with your GitHub scope, on the bound branch and PR only. Branch code that CI runs still runs under your repository's CI policy.
- With `sandbox.host`, the VM's owner is trusted. The VM holds no GitHub token and no route back to the fno host.
- Engine and kernel escapes, multi-tenant isolation and resource quotas are out of scope.

## What the loop WILL do, given a plan

- Read and write files in the worktree.
- Run shell commands as the user invoking footnote.
- Make API calls to the configured model providers with the user's credentials.
- Open pull requests on GitHub with the user's credentials.
- Push branches to remotes the user has access to.
- Edit `~/.fno/` state files.

## What the loop WILL NOT do, by design

- Run with privileges higher than the user invoking it.
- Bypass git hooks, branch protection, or pre-commit checks.
- Skip review gates configured in the plan.
- Send your code to anyone other than the configured agent runtime.

## What the loop CANNOT prevent, given an adversarial plan

- A plan that asks the agent to leak credentials.
- A plan that asks the agent to delete files outside the repo.
- A plan that asks the agent to push to a third-party remote.
- A plan that constructs adversarial prompts for downstream skills.

If you are evaluating an footnote plan from an untrusted source, treat it as code review, not as configuration. Read the plan content, not just the title.

## The effect guard for hooked tool calls

For Claude-hosted workers whose PreToolUse hooks run footnote's plugin, there is a second layer between the tool list and the destination. The effect guard (`fno agent internal harness hook effect-guard` and the Bash chain's last guard) classifies each hooked tool call into an effect class. Reads, searches, and local edits map to no effect and are never blocked by it. GitHub PR and issue comments also map to no effect: they are review traffic in the repo, like `gh pr create`. Sends and posts map to `external.communication`, publishes and deploys to `external.publication`, remote deletes to `infrastructure.destructive` (denied outright), and payments to `financial.payment` (denied outright). An approval-requiring class is refused unless an approved, unexpired decision by a principal named in `config.approvals.authorized_principals` matches it. The refusal names the request digest and the `fno inbox approvals decide <digest> --as <principal> --approve` line, and the worker can retry the call unchanged once approved.

What the guard does not cover: an effect hidden inside a script or a raw `curl` (the Bash rows match leading command tokens). It also does not cover a harness whose PreToolUse hooks footnote does not wire (codex, opencode, pi, agy). The merge effect stays with `authorized-merge` and git-protection. The tool list remains the outer boundary. The effect guard narrows what a hooked tool call can do inside it.

## Installing a pack from a URL

`fno config plugins install <git-url>` runs third-party content, so it stages before anything runs. The clone points hooks at `/dev/null` and fetches no submodules. The pack must verify on the staged tree, and it can carry only mods and layouts. Roles, skills, agents, adapters, workflows and evaluators install from a local path only. A sha256 over every file binds the consent. An attended terminal says yes, or a principal approves the `local.plugin-install` request for that digest with `fno inbox approvals decide`. A mod that runs its own program starts off, and every launch hashes the pack again and refuses on a change. The gallery lists repos and never installs one. Every row reads "not reviewed by fno". Full contract: [mods-and-layouts](architecture/mods-and-layouts.md).

## The web bridge and the attention door

The assets here are node data, pane frames, transcripts, answer authority, board acts, the secrets that sink rows name, and click activity. fno trusts three things: your OS user, your global config, and an absolute `.toml` pin in the process environment. It does not trust the folder it serves, a tailnet login, a revoked invitee who kept the URL, or a page on another site.

- **A folder grants nothing.** `web.invites` and `attention.sinks` come from [the trusted file](architecture/attention-items.md#the-trusted-file) only.
- **A fault grants nothing.** An unreadable file, bad TOML or a wrong type admits no invitee and no sink.
- **A revoke is live.** The next request refuses. An answer or a look reads again before it writes.
- **An open feed follows a revoke.** It closes in about a second.
- **A click needs your consent.** Only your own direct local request writes a row.

These limits remain:

- A tool such as direnv can set `FNO_CONFIG` for a folder. Read an `.envrc` before you allow it.
- A raw TCP or SSH forward looks like this machine. Whoever holds it and the token counts as you.
- A read that is already in flight can finish after a revoke.
- An open chat socket outlives the removal of its sink.
- A cloned folder can hold invite rows that force the bridge closed at start. It cannot open it.
- `fno config get` and the settings page show merged values, so they can show rows that grant nothing.
- An invitee's request can start an `fno` subcommand. Its usage row names the command, never the caller.

## Code scanning dispositions

CodeQL runs through advanced setup (`.github/workflows/codeql.yml` with `.github/codeql/codeql-config.yml`). The analyze job is skipped until the repository switches from default setup to advanced setup. The gate is the `CODEQL_ADVANCED_SETUP` repository variable. Between merge and the switch, default setup keeps its weekly scan and the skipped job keeps main green. Rule classes judged not to apply are excluded in the config or dismissed per alert, with the reasons below.

- **`rust/cleartext-logging` (404 alerts, excluded in the config).** False positive. Every source expression is an identifier printed to the user's own terminal: account arguments, session ids, uuids, account names. The twelve hits naming a secret, password, certificate or key are test assertion messages or counts. No hit writes key material to a log file. The same config ignores test trees: `paths-ignore` covers `crates/*/tests/**`, `cli/tests/**` and `**/fixtures/**`. That keeps `py/clear-text-storage-sensitive-data` (1 alert, a test fixture) out of every run.
- **`actions/cache-poisoning` (10 alerts, dismissed).** False positive. Both workflows trigger only on `workflow_dispatch` and `workflow_call`. The one caller passes a pinned main sha or rc tag as `ref`. No fork or pull-request code reaches a cacheable step.
- **`js/xss` (2 alerts) and `js/xss-through-dom` (1 alert), dismissed.** False positive. The mux web pages set `innerHTML` only from `renderRow` output: text passes the `esc()` escape helper and colors are numeric. The backlog link href is built from server-rendered data and `encodeURIComponent`.
- **`js/polynomial-redos` (1 alert) and `py/redos` (1 alert), fixed in code.** The lazy regexes went quadratic on nested-unclosed input. Both promise readers and the graphql inventory gate now use a linear scan or first-character-disjoint value tokens.
- **`actions/missing-workflow-permissions` (25 alerts), fixed in code.** The five workflows the alerts name carry top-level `permissions:` blocks now.
- **`rust/hard-coded-cryptographic-value` (8 alerts, 447-454).** False positive. The query reads the parameter name `nonce` in `verify_child_promise` as key material. The value is a session promise token that the code compares by equality with a stored journal string. No cipher, key or MAC consumes it. All eight hits are test literals (`crates/fno-agent/tests/verify_evidence_parity.rs`, and the `#[cfg(test)]` module in `crates/fno-agent/src/gate/verify_evidence.rs`). Dismissal reason: `used in tests`.
- **`rust/command-line-injection` (1 alert, 446).** False positive. The attention API clears an item with an argv vector, never a shell. The `id` must match an open item from `fno agent fleet needs --items --json`. The `--answer` value is taken verbatim by the clear command. The route needs a bearer sink token and binds to 127.0.0.1. Dismissal reason: `false positive`.

Critical-severity alerts are held for the maintainer's decision and stay out of these dispositions.

## Future considerations

Two horizon items are tracked in the backlog with explicit "do not scope" tags:

- **Cost-anomaly alerts.** A real horizon need; will be planned when an incident creates concrete requirements.
- **APFS-snapshot phase rollback.** macOS-specific recovery capability; will be planned when an incident demands it.

Neither is a current capability. Do not assume either exists.

## Reporting issues

Security issues should be reported via a GitHub security advisory at https://github.com/<your-org>/footnote/security/advisories or by emailing the maintainer (see repository owner).
