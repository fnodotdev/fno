# fno vision

This page says what fno is, who it is for, and what it will not build. The plan judge and the merge check read it. A lead reads it to check that a node or a PR is on track. Keep it short. If a change to fno does not fit this page, change the page first, in its own PR, with the reason.

## What fno is

fno runs your coding agents as one team. The agents work from one backlog. They message each other and you. They remember what you decided. You make the calls, and they do the rest.

A person with ten agents must feel like a lead with a good team, not a dispatcher with ten terminals. Today that person is the message bus. They pick the next task, copy context between sessions, notice a dead session, and explain yesterday's decision again. fno takes that work off them.

The unit of fno is a team of agents, not one chat. Other tools give one agent a thread that people share. fno gives many agents, on many harnesses, a shared queue, a chain of command, and a record that outlives any one session. People join that team.

## Who it is for

1. **First: a developer who runs two or more agent CLIs at once.** An example is Claude Code with Codex. They work on their own repos, from a terminal on a Mac or Linux. They have more agents than attention.
2. **Next: the same developer on a Linux box over SSH or a cloud dev box.**
3. **Then: a small team of two to five people.** They share one backlog and one inbox across machines and agents.
4. **Later: people who want their fleet in an app.** The phone, web and desktop apps serve them, in two modes.

fno is not for a person who runs one agent. One agent costs less without fno. fno is not for a person who needs a sandbox, because fno runs on your own machine and your own credentials. It is not for large teams with their own QA staff and release process.

## Product pillars

Each pillar is a promise to the user. A feature earns its place by keeping one of these promises better.

### 1. One team, one backlog

Every agent works from one backlog. Work is a node in a dependency graph. One live session claims a node at a time, so two agents never do the same work. Lead agents pick ready work and hand it out.

- `fno backlog`: nodes, dependencies, priority, the board, and the lifecycle from idea to done.
- `fno agents claim`: one live owner per node. Work returns to the backlog after its session dies.
- Leads (`/fno:lead`): an agent that holds a part of the backlog and dispatches workers.
- `fno agents spawn`: start a worker on a node, on any harness. It runs as a thread, a pane or headless.
- Plans and waves (`/fno:blueprint`, `/fno:execute`): split a node into tasks that run in order or in parallel.

### 2. Agents talk to each other and to you

Agents send mail. A worker tells its lead what it found. A lead asks the user a question. Mail is durable, so a message waits for an agent that is not running yet.

- `fno agents mail`: send, reply, and read between any two agents or the user.
- The question queue: a decision an agent cannot make goes to one list for the user.
- Threads and portals: every background worker is a session the user can open and talk to.
- `fno agents fleet feed`: one ordered view of what every agent did and asked.

### 3. You make the calls

The user keeps authority. A ruling is written once and reaches every later agent. A hold or a freeze the user sets stops a merge. Agents bring only the decisions they cannot make.

- `fno inbox decide`: record a ruling with who made it and why.
- Laws: rulings that later agents receive as context, matched to the work in front of them.
- Holds and freezes: user-set gates that stop a merge until the user releases them.
- The plan judge: before a node builds, check that it fits this page. A "will not do" is a good outcome.

### 4. Any harness, any model

fno drives the agent CLIs you already use. Claude Code, Codex, opencode, pi and others run side by side on one backlog. Each step of the work can use a different model.

- Harness drivers: one way to start, resume, read and stop each supported CLI.
- Per-stage routing: one model to plan, another to build, another to review.
- `fno config accounts`: many accounts, with failover and limits per account.
- fnh, fno's own harness: it builds what the vendor CLIs do not have, not a copy of them.

### 5. Done means merged

An agent's word is not proof. GitHub decides that work is done: a PR exists, CI is green, and the review is complete. fno judges the plan before code exists, because a no at plan time costs one plan, not a build.

- `/fno:target`: take a node from idea to a green PR, and stop only on GitHub truth.
- `/fno:review`: a review of the diff against the plan, posted on the PR.
- `fno do pr merge`: the merge door, which obeys holds, freezes and red CI.
- The merge check: at the final head, ask one question: does the diff deliver the plan?

### 6. From any screen

The team keeps working after you leave the desk. You see it and steer it from the terminal, the desktop, the web or the phone.

- `fno mux`: the terminal view of every session, with restore after a reboot.
- Chat mode: the app default, a conversation with rendered tool calls, like a chat app you know.
- Orchestration mode: the command center, with every session, node, plan, and the mail that needs you.
- Machines: one backlog and one mail across several machines on a private network.

## What fno will not build

- **A sandbox.** fno runs on your machine with your credentials. Use a throwaway repo or your own isolation.
- **A full issue tracker.** The backlog exists so agents can coordinate. GitHub Issues and Linear import into it.
- **A copy of a vendor CLI.** fnh builds only what the vendor CLIs lack, or what fno needs to run.
- **A runtime race.** Persistence and remote access stay at parity. fno does not lead with them.
- **Guards the user did not ask for.** Default refusals ship off. Holds and freezes stay on.
- **An editor or an IDE.** fno shows diffs and reviews. It does not replace the place you write code.
- **Silent telemetry.** fno sends nothing home. Any usage report is local or opt-in.
- **A one-agent product.** Every surface assumes more than one agent.
- **Paid hosted pieces ahead of users.** Sync relays and push services wait until the apps have real users.

## Values

- **Honest about done.** A PR, green CI and a review, never an agent's word.
- **The user keeps authority.** Agents decide what they can. The user rules on the rest, once.
- **Harness neutral.** No vendor is the default for the user. Each one is a member of the team.
- **Fewer, larger PRs.** One feature in one PR is easier to review than five fragments.
- **Say the limits.** No sandbox, your credentials, your bill. State them in the same breath as the pitch.

## North star

**Weekly active teams:** people who, in one week, run agents on two or more harnesses from one fno backlog. They also merge at least one PR from a node that a lead agent dispatched.

Input metrics:

1. Minutes from install to the first mail between two agents on two harnesses. The target is 10 or less.
2. Share of new users who dispatch a node to a second harness in their first week.
3. Share of merged PRs that a lead dispatched, not the user.
4. User questions per merged PR.
5. Rulings and holds written per active user per week.

Guardrails: false refusals per session, reverts within 7 days of a merge, and spend per merged PR. A fourth is the time to a first reply on an outside issue.

## Is this PR on track?

A lead runs this test on a node before it builds and on a PR before it merges. Answer each question in one line. Two or more "no" answers mean stop and ask.

1. **Which pillar does it serve?** Name one. If no pillar fits, the work is off-vision.
2. **Who notices?** Name the user from "Who it is for" whose day gets better, and how.
3. **Does it move the north star?** Name the input metric or guardrail it moves.
4. **Is it on the "will not build" list?** Then stop. A change to the list comes first.
5. **Is it the smallest whole change?** One feature in one PR, with no speculative parts.

A health fix (a red test, a crash, a false refusal, a security hole) passes without a pillar. Say so in the PR.
