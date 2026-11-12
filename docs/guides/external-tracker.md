# Bring your own tracker: GitHub Issues or Linear as the backlog

## Is this page for you?

You already track work in GitHub Issues or Linear, and you want fno to consume that backlog instead of its built-in graph store. This page connects one tracker to fno, binds a real item to a checkout, and ships a PR that closes the issue. The built-in store stays the default. Nothing changes until you select one.

Not for: the design contract between fno and its trackers. That is [the external tracker design page](../architecture/external-tracker.md).

## Pick the backend

One key selects the backend for every backlog read and write.

```toml
# .fno/config.toml (project) or ~/.fno/config.toml (global)
[tracker]
backend = "github"   # or "linear", or "graph" (the built-in store, the default)
```

A project key answers for that repo. A global key answers everywhere a project does not set its own. The project file wins.

The flag selects for one invocation and beats the config key:

```bash
fno backlog next --tracker-backend github
```

The env var `FNO_TRACKER_BACKEND` selects for one shell or one CI job. The whole chain reads, in order: flag, env, config, and the built-in store. This page assumes the config key or the flag. CI jobs that export the env var keep working unchanged.

The flag rides the `fno backlog` verb group, before or after the verb. An unknown value refuses and names the available backends, and `--tracker-backend graph` pins the built-in store over an inherited env value or a config key.

## Scope and credentials

Scope and auth ride env vars by design: a config file never carries a secret.

| Env var | Meaning |
|---|---|
| `FNO_TRACKER_GITHUB_REPO` | `owner/repo` of the repo whose issues answer the backlog. |
| `FNO_TRACKER_LINEAR_TEAM` | The Linear team key, for example `ENG`, that scopes the listings. |
| `FNO_TRACKER_LINEAR_API_KEY` | The Linear key. It reaches curl through a mode 0600 header file. |

Two id shapes work: `owner/repo#123` for GitHub and `TEAM-123` for Linear. An id carrying `:` refuses everywhere, because that character partitions claim keys.

## Bind an item and ship it

The journey the live gate records, at human speed. Export the scope vars, then:

```bash
fno backlog get owner/repo#123                  # read one issue through the backend
fno backlog next --tracker-backend linear       # see what the tracker offers
fno backlog update owner/repo#123 cwd="$PWD" plan_path=plans/my-plan.md
```

The update binds the checkout and the plan to the external id. Under an external backend it takes exactly those keys, plus `pr_number` and `pr_url`. Every other key refuses and names the four that bind. A missing issue refuses with `not found`.

When the PR exists, bind it:

```bash
fno do target start owner/repo#123
fno do pr bind-created --url <pr-url> --node owner/repo#123
```

From there the normal loop applies. The node reads `in_review` while the PR is up, and the merge closes the issue in the tracker. The board refreshes from the backend on a bounded clock. If the backend is down, the board answers its cached snapshot. An outage reads as a stale board, never a blank one.

## What fno keeps, and what it leaves to the tracker

The partition rule: the tracker stores what any backlog stores, and fno stores only what no tracker can express. Titles, state, parents and blockers stay in the tracker. fno's sidecar keeps `cwd`, the plan pointer, the PR number, cost, and session provenance. A field named on both sides is the failure the CI partition gate (`scripts/ci/check-tracker-partition.sh`) exists to catch. So nothing mirrors.

GitHub parents and blockers read degraded: they stay empty, because sub-issue and dependency reads cost two REST calls per open issue per refresh.

## Run the live gate

The gate proves one disposable issue through the full journey on real credentials. It never runs by accident: you point it at a scratch repo and a scratch Linear team, and it refuses real work.

Create the scratch surfaces once:

1. A new, empty, private GitHub repo with a README and a label named `fno-live-gate`.
2. A fine-grained GitHub token limited to that repo: Contents, Issues and Pull requests, read and write, short expiry.
3. A Linear team used only by the gate, with the default workflow. A separate free workspace is best.
4. A Linear personal API key. If the form offers a team limit, take it.

Set the five names and run the workflow, or the tests directly:

| Name | Meaning |
|---|---|
| `FNO_LIVE_GITHUB_REPO` | `owner/repo` of the scratch repo. Both journeys need it: the Linear journey opens its PR there. |
| `FNO_LIVE_GITHUB_TOKEN` | The fine-grained token. It reaches child processes as `GH_TOKEN` and git as `GIT_ASKPASS`, never argv or a remote URL. |
| `FNO_LIVE_LINEAR_API_KEY` | The Linear key. It reaches curl through a mode 0600 header file. |
| `FNO_LIVE_LINEAR_TEAM` | The scratch team key, for example `FNOGATE`. |
| `FNO_LIVE_FNO` | The `fno` binary under test. The default is `fno` on PATH. |

```bash
cargo test --manifest-path crates/fno-agents/Cargo.toml --test tracker_live -- --ignored --nocapture
```

The `tracker-live` workflow on GitHub runs the same journeys. When the secrets are not configured, it writes "not configured; skipped" and passes. Each run writes a receipt under the state dir's `live-gates/` and closes only the items it created.

## Where the design lives

The design contract lives on [the external tracker design page](../architecture/external-tracker.md). It covers the partition rule, the CI gate, and the backend list (Jira ships on demand).