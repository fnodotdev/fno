---
name: talk
description: Talk a rough idea through, keep one living brief per idea, and end each talk with the next cheapest test.
argument-hint: "<idea | node-id>"
disable-model-invocation: true
metadata:
  requires:
    binaries:
      - "fno >= 0.1"
---

# Talk

A conversation, not a research run and not a plan. The user brings an idea, organized or half-formed. You talk it through with them. The idea keeps one living brief, bound to its node, and every talk ends on one **Next test**.

`/fno:think` researches a question into cited findings. `/fno:blueprint` plans a slice. Talk comes before both: it decides which guess to test next, and how cheaply.

## The process

1. **Open the idea.**
   - Free text: file it with `fno backlog idea "<short title>" --details "<the user's own words>" --json` and read `id` from the receipt.
   - A node id or slug: use it.

   Run `fno backlog session open <node> --phase think --json`. A nonzero exit is the answer: relay the refusal line and stop.
2. **Find the brief.** `fno backlog get <node>` prints `plan_path` and `cwd`. When `plan_path` is set, read the brief, its test log and its Next test line. When it is empty, run `fno do plan path --slug "<slug>" --node <node>` from the node's `cwd`, write the template below to that path, and bind it: `fno backlog update <node> plan_path=<path>`.
3. **Talk.** Ask one question at a time. Start from the brief's open guesses, or, on a new brief, from the problem and who has it. Push on the riskiest guess: the one that kills the idea if it is wrong. When the user brings a test result, write it into the test log and move each guess it touches to `supported` or `refuted`, with the evidence (a quote, a number, a source row). A guess with no evidence stays `open`, however sure the user sounds.
4. **Pick the next test.** Pick the cheapest test that can refute the riskiest open guess. One of three:
   - **Talk to people.** Write a short script: five to eight open questions drawn from the open guesses, about the person's past behavior, never "would you use this?". When asked, draft one first-touch message. Draft only: the user sends it. An agent send goes through the effect gate and waits in `fno inbox approvals`. When the user brings notes, keep quotes, call people P1, P2, and never write a name or a contact into a brief that lives in git.
   - **Look it up.** Frame the question in one line and run `fno do research "<question>"`. Cite the rows from its `sources.jsonl` in the test log.
   - **Show a quick prototype.** Name the one slice that tests the guess. The user runs `/fno:blueprint quick <brief path>`, then `/fno:target` on it.
5. **Write it down.** Update the brief: guesses, test log, and the `Next test` line. The line names the test, the guess it tests, and what result would refute the guess.
6. **Close the talk.** Run `fno backlog session close <node> --phase think --summary "Next test: <the line>" --launch "/fno:talk <node>" --json`. The node now carries one session row per talk, and the newest row names the next test.

End the talk with the Next test line and the brief path, then stop. When the user wants to drop the idea, record it with `fno backlog defer <node> -K wont_do -R "<why, in the user's words>"` instead of a Next test.

## The brief template

Frontmatter `status: design` keeps autonomous workers off the node until the user plans a slice.

```markdown
---
title: "<idea in one line>"
node: <node-id>
status: design
kind: talk-brief
---

# <idea in one line>

## Problem
<what hurts, in the words of the people who have it>

## Who has it
<the narrowest group that has it most>

## Bet
<what we would build or do, and why it beats what they use today>

## Guesses
| # | Guess | Status | Evidence |
|---|---|---|---|
| G1 | <a claim that, if false, kills the bet> | open | |

## Test log
| Date | Test | Guess | Result |
|---|---|---|---|

## Next test
<test> - tests G<n> - refuted if <result>
```
