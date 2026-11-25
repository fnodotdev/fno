# The sandbox host: a headless worker runs in its own container

A worker can damage shared state. A test worker once wrote over the registry and the claims. The `[sandbox]` table runs each headless worker in a container of its own. The container gets no mount: no worktree bind, no daemon socket, no agents home, no host login and no engine socket. It reaches the host only through one relay that the host owns.

## The keys

```toml
[sandbox]
provider = "docker"   # none (default), docker, podman, apple, devcontainer
image = "my/image:1"  # global config only; unset builds the embedded recipe
host = "ssh://me@vm"  # global config only; unset runs the engine locally
```

`none` launches every worker on the host, as before. Any other value sandboxes each `--as headless` spawn of claude, codex, gemini, opencode or agy. `docker` covers OrbStack and Docker Desktop on a Mac and Docker on Linux. `devcontainer` reads `.devcontainer/devcontainer.json` from the default branch as an image source and runs it on docker. `apple` refuses by name: apple/container has no passing journey yet. `fno config set sandbox.provider <value>` writes the key, and an unknown value refuses and names the legal ones.

The layer rule keeps a repository from turning the sandbox off. Any layer can raise `none` to a sandbox. A project layer cannot lower or switch a provider that global config sets. The spawn prints a notice and keeps the global one. `image` is read from global config only. An unparsable layer or an unknown value refuses the spawn. A sandboxed spawn on a pane or thread substrate refuses with exit 2, because that spawn runs on the host.

## The lifecycle

The lane hands its composed command to `fno-agent hook sandbox-run`. That wrapper owns the whole lifecycle, so the registry row's pid is the wrapper's pid. Its argv carries env key names only. The values stay in its own env and cross to the guest over exec stdin.

1. Resolve the image. A devcontainer is exported from the default-branch commit to a clean folder, never read from the worktree.
2. Start a container labelled `dev.fno.sandbox=<id>` as uid 1000, with `--network none`, `--cap-drop ALL`, `--security-opt no-new-privileges` and no bind.
3. Start the guest (`fno-agent hook sandbox-guest`) with `engine exec -i`. One status round trip proves it before any harness starts.
4. Bundle committed HEAD on the host and clone it at `/work`. The receipt names the dirty host paths the clone does not carry.
5. Rewrite the harness argv. Worktree paths move to `/work`. Route `--settings` content crosses stdin to a guest copy. Host `--add-dir` grants drop. Any other host path refuses by name. Codex runs with its own sandbox off, because bwrap cannot start without user namespaces. The container is the boundary.
6. Run the harness on a second `exec`, with its stdin, stdout, stderr and exit code passed through.
7. Export a commit bundle, a dirty patch and an archive of untracked files.
8. Fetch the bundle into `refs/sandbox/<id>/head` with hooks off and object checks on. No host branch moves. The receipt prints the inspect command.
9. Remove the container. A failed export keeps it and names it.

At the next spawn the wrapper sweeps labelled containers whose wrapper pid is dead: it exports each one, then removes it. A container with no export record, or whose export fails, is kept and named.

To read a worker's work, run `git log -p refs/sandbox/<id>/head`. The dirty patch and the untracked archive sit in `<agents home>/sandbox-exports/<id>/`. The host never applies or extracts them.

## What the sandbox can ask the host

The guest listens at `FNO_SUPERVISOR_SOCKET` (`/tmp/fno/supervisor.sock` inside), so a client inside needs no change. It turns each request into one typed frame. Everything else is refused inside, by name, and never crosses the stream.

| Frame | The sandbox sends | The host does |
|---|---|---|
| status | nothing | answers a local status |
| report | `working`, `blocked` or `done` | stamps the session and seq, then calls `agent.report` |
| session-report | the start source | builds `agent.session-report` from the spawn's own name, session and host cwd |
| transcript | offset and bytes | appends to this sandbox's one file |
| connect | host and port 80 or 443 | resolves once, checks every address, dials the checked address |

The host stamps identity from the pipe a frame arrived on. A frame never carries an argv, env name, host path, session id, node, recipient or credential that the host trusts. `agent.spawn` no longer accepts an `argv` field from any client.

## A sandboxed worker in the target loop

A spawn with `--node <id>` is a target worker. The spawn guard clears an inherited `FNO_NODE`, so only the flag binds a node. Before the clone, the relay runs the normal host start, `fno do target start <node> --no-merge`, for the sandbox session. That gives a host worktree off the default branch, the node claim held on the host, the manifest and the bound plan. The clone comes from that worktree, and its HEAD stays at the intake base. A spawn with no node binds nothing, and every loop op refuses.

Inside, `fno` sees `FNO_AGENT_SANDBOXED` and opens no store. The loop's commands forward as one `sandbox.op` request each, and every other command refuses by name before anything runs. The guest checks the op again and sends it as one typed frame with a sequence number. The relay drops a repeated number, so a replay never runs a write twice. Each op runs on its own thread, so transcript and proxy frames never wait behind a push or a `gh` call.

| Op | The sandbox sends | The host does |
|---|---|---|
| node get | nothing | `fno backlog get` for the bound node and its parent |
| node note | text | a note on the bound node, labelled sandbox origin |
| target start, target status, claim status | nothing | the binding, the plan as a read-only copy, and the existing reads |
| plan settle | `U<n>` or `route:<n>`, one line | edits only that open Unknowns line or adds one chosen line, never frontmatter |
| mail | kind and text | mails the spawner only, labelled |
| push | nothing | exports the live container, refuses a CI edit, then pushes the sandbox head to the bound branch, fast-forward only, hooks off |
| PR create, status, comments, reply | title, body, comment id, text | the PR door with the sandbox head and the bound branch; guest text made inert |
| loop stop | the final message | exports, then runs the one host Stop decision |

There is no merge, attest, spawn, decide or backlog update op. The refusal names the lead.

### Stop

The harness Stop hook inside runs `fno-agent hook stop`, which sends loop stop. The host exports the live container into `refs/sandbox/<id>/head`, then runs `fno-agent hook stop --sandbox <id>` on the bound worktree with the sandbox session and the sandbox transcript. If the export record names another worktree, that flag binds no sandbox origin and the Stop blocks. Under that origin, the HEAD and tree reads take the sandbox head and the export's dirty reading. The correction gate records nothing, because the transcript's user turns are forgeable. The self-review floor is off, because the sandbox has no attest op, and review stays required, so a green PR ends `DoneUnreviewed` and never `DonePRGreen`. Done probes run inside through the engine. A probe that starts with `gh`, `fno`, `fno-agent`, `curl` or `ssh` blocks with its name, because it reads host state. Finalize makes no WIP commit on a sandbox's host worktree. If the relay cannot answer, the Stop blocks and names the relay. It never allows.

### GitHub

The GitHub login never enters the sandbox. The push takes the exported head and refuses a net change to `.github/workflows/` or `.github/actions/` against the merge base with the default branch. PR create reads every gate against the sandbox head and the bound branch. Before anything posts, the host breaks `<!--` markers, `@` mentions, closing words, `Audited owners:` lines and other node ids with a word joiner. The host writes the origin label, its own `Audited owners:` line and `Fixes <node>`. The run is no-merge: a person or an outside reviewer reviews and merges.

## Your own VM

Set `sandbox.host = "ssh://[user@]host[:port]"` in global config to run the engine on a Linux VM you control, with docker or podman. Every engine call carries the host, as `docker -H ssh://...` or `podman --url ssh://...`. The engine CLI moves each argv over its own API, so no remote shell quotes anything. Before first use, the relay checks the host. It refuses a host that `ssh-keygen -F` does not find in known hosts. It also refuses an `ssh -G` config that forwards the agent or a local, remote or dynamic port. The relay, the claim and the GitHub login stay on the fno host, and the VM gets no route back. The proxy still dials from the fno host, so it also closes the VM's own resolved addresses.

A dropped ssh link ends the guest stream and the harness exec. The export then fails or succeeds on its own, and a failed export keeps the container. The next spawn's sweep exports it once the link is back, because the wrapper pid it tests lives on the fno host. A write in flight never replays, because each op carries its own sequence number.

## Network

The container has no network. The guest serves an HTTP and CONNECT proxy on its own loopback, and the harness gets `HTTPS_PROXY` and `HTTP_PROXY` pointing at it. Claude, codex and opencode send every request through it (measured on CLI versions 2.1.331, 0.172.0 and 1.19.6). The host closes private, loopback, link-local, metadata, CGNAT, IPv6 ULA, multicast and host-owned addresses, with their IPv4-mapped and NAT64 forms. One closed address in an answer refuses the whole answer, so a rebinding name gets nothing. Only ports 80 and 443 open.

## The transcript

The guest tails the harness file (claude and codex) and flushes to EOF after the harness exits. The host writes one 0600 file per sandbox under `<agents home>/sandbox-transcripts/`, outside every harness store, and stops at 64 MiB with a marker line. The registry row names the file only in `sandbox_transcript`. `transcript_path` is never set for a sandbox, because its readers trust user turns, laws and account caps. `fno agent status --peek` and the truth probe read the new field and label it `origin=sandbox`. A sandbox row never joins an account's cap lane. Gemini, opencode and agy keep their host stdout tee.

## The recipe

`fno-agent` embeds `crates/fno-agent/assets/sandbox/Dockerfile`. With no `image` set, the wrapper builds `fno-sandbox:<rev>` from it. The Linux binaries for the host rev and engine arch come from bin-cache, checked by sha256. It copies in the staged plugin whose source head matches that rev. A dev or dirty rev has no bin-cache asset: build an image from the recipe with a local `bin/` directory and set `image` to it. CI does exactly that with binaries built from the PR head.

## Refused devcontainer keys

`dockerComposeFile` refuses, because compose runs services on the host engine. `initializeCommand` refuses, because the devcontainer CLI runs it on the host at every `up` (measured on CLI 0.91.0). The wrapper never calls the devcontainer CLI. It builds or pulls the image with the engine.

## What it does not protect

The threat model and its residuals live in [security posture](../security-posture.md). In short, a sandbox ref is untrusted code. The sandbox can still send its own key and source out through a public host. Engine and kernel escapes are out of scope.

## Proof

`.github/workflows/sandbox-e2e.yml` runs `tests/sandbox/e2e.sh` on docker and podman. One headless spawn runs a stub harness that probes every forbidden reach from inside. Each probe has a positive control on the host. A second spawn is bound to a node and runs the loop journey. It starts the target, reads the node, pushes to a local origin and opens a PR through a recording `gh`. Its merge, attest, spawn and workflow push are refused, and a Stop with a forged promise blocks. A third leg runs the same journey on docker over the runner's own sshd and drops the link mid-run. The job prints `forbidden reached: 0` and `required failed: 0`, and fails otherwise.
