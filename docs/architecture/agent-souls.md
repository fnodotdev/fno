# Agent souls

Every fno agent role that needs a stance has one: lead, archer, code-reviewer, design-critic, growth, persona-reviewer. A SOUL carries stance, values, taste and voice. It carries no rules. Rules live in AGENTS.md and the skills, and a rule always wins over a soul.

## What it is and is not

A skill says what to do. An agent declaration says which tools, model and trigger answer the spawn. Nothing said how the agent holds itself. The SOUL fills that gap: the posture a reviewer takes, the taste a lead applies, the voice a growth piece is written in. It names no worker, so a SOUL survives every handoff, and a successor can pick a different style.

## The path and the catalog

Seeds live in this repo at `crates/fno-agent/src/souls/<role>/<style>/SOUL.md`, 5 to 7 styles per role. They embed in the binary at compile time. The crate publishes to crates.io, and a package cannot read outside its own directory. Each seed carries Agent Skills frontmatter. `name` equals its folder. `description` names the moment a lead picks the style. `metadata` is a flat map of quoted strings (`role`, `fits`, `default`, `status`). The body is exactly four sections (Stance, Values, Taste, Voice) and under 40 lines.

On first read the seed copies to `~/.fno/profile/agents/<role>/<style>/SOUL.md` and never overwrites afterwards. An edited soul is the user's. A user can also add their own style directories. `style list` shows them beside the seeds.

## The pick

A lead runs `fno agent hq chart style list`, picks the style that fits the territory, and records it with `fno agent hq chart style <style>`. The pick writes `style:` on the scope manifest, which is the team chart. `fno agent hq --json` carries it. A lead with no pick gets its role default. At handoff the successor reads the pick with `style list` and can pick again. Lessons from a term go into the territory canon doc at the retro, never into a SOUL.

A brief names a worker style with `soul: <role>/<style>`. The minion clause tells the worker to read it before the first edit.

## Load order

The agent declaration (tools, model, trigger), then AGENTS.md and the skill (rules), then the canon doc (territory stance), then the SOUL (personal stance). When a SOUL and a rule disagree, the rule wins.

## The split from PERSONA.md

`~/.fno/profile/customers/<project>/<persona>/PERSONA.md` (D3440) is who fno builds for: customer data a reviewer reads. The SOUL is how the agent doing the work holds itself (D3441). Two different files, two different resolvers. The soul resolver copies only the `FNO_HOME` root rule from `audience.rs`.
