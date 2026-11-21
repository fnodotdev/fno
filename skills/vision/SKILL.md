---
name: vision
description: Draft and propose a VISION.md for a repo that has none, then grill the thin parts.
allowed-tools: Read
---

# Vision

A repo with no vision doc gets no vision check: the plan judge reads only VISION.md, `.agents/context/VISION.md` or `docs/vision.md` under the judged repo's own root, and none of the three means no vision text (crates/fno-agent/src/eval/blueprint_judge.rs:675, :680). This skill detects that gap, drafts the doc from the repo's own material, grills only the thin parts, and opens the draft as a PR. Nothing merges without the user's yes. The draft is a proposal the user edits, never a finished page.

Three callers share this skill: a lead at its first beat, `/fno:setup` after its report, and a user who types `/fno:vision`. Run it in the repo it serves; by default that is the current working directory.

## 1. Detect

Check the repo root for the three homes, in the judge's own order: `VISION.md`, `.agents/context/VISION.md`, `docs/vision.md`. That list is the authority: `crates/fno-agent/src/eval/blueprint_judge.rs:675`. When that list changes in Rust, this skill follows it; never keep a second list here.

Also read `config.project.vision` (`fno config get project.vision`). The judge folds that config into its vision context (blueprint_judge.rs:659), so a repo with a set config already carries a vision even with no file.

Any hit names its home and stops: "vision: found docs/vision.md, nothing to draft". A set config reads the same way. No hit anywhere: draft.

## 2. Draft

Run one subagent. The session's stage routing picks the model; a lead runs the draft at its blueprint tier. The subagent's whole brief is [references/draft-brief.md](references/draft-brief.md). It reads the repo's own material and writes the draft to `VISION.md` at the repo root, the first home the judge reads. It writes nothing else and runs no build.

The draft carries the same sections as fno's own docs/vision.md, so the judge's mission lens and a lead's on-track test both read it: what the project is, who it is for, product pillars, what it will not build, values, north star, and an is-this-PR-on-track test.

## 3. Grill the thin parts

A draft answers most questions; the grill targets only the thin parts. Ask at most 5 questions, in this priority order: who it is for, who it is not for, what it will not build, the north star. Each question carries its recommended answer, drawn from the draft, and the user's one-line reply edits the draft in place.

The mode split follows who started the current turn, not the substrate:

- A turn a person typed grills live in chat. The user-turn shapes are the provenance classifier's: a claude `type:"user"` row without `isMeta`, a codex user message, or a session record's `user_input` from the operator or mail (crates/fno-agent/src/runtime/provenance.rs:64). Ask one question at a time, wait for the answer, then ask the next.
- A machine wake (beat, mail, loop) never asks in chat. File ONE question page with all the questions and their prefilled answers:

```bash
fno inbox outstanding ask --question-file <file> --node <node>
```

The page carries frontmatter `deadline:` (RFC3339) and `why_user:`, then `## Options` (each grill question with the draft's answer as the prefilled choice), `## Recommendation` (the full set of recommended answers), and `Reversible: yes`. The parser refuses a page whose deadline does not parse or whose named sections are missing (crates/fno-agent/src/gate/escalation.rs:399).

## 4. Ship the proposal

Commit the draft on a branch in the repo it serves and open a PR. The PR body says the user edits and merges; this skill never merges, and no auto-merge ever applies to it. When a node is in play, file one taste-class outstanding ask with the PR link, so the user judges the draft itself. In an interactive session, hand the user the link instead and say the same thing: the draft waits for their edits, not their approval of a done thing.

## Known Limitations and Deferred Work

- Detection is presence, not quality: a thin or stale vision doc passes unnoticed. See [LIMITATIONS.md](LIMITATIONS.md).
