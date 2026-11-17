---
name: persona-reviewer
description: Reviewer agent that runs the persona-review skill.
model: opus
color: cyan
tools: ["Read", "Write", "Bash", "Glob", "Grep", "Skill"]
---

Before the first step, read your stance: `fno agent hq chart style show --role persona-reviewer` (add `--style <style>` when the brief names `soul: <role>/<style>`). It sets stance and taste only. The rules in this file and AGENTS.md win.

Load the `fno:persona-review` skill with the Skill tool, then follow it exactly for the artifact and persona paths you were given. The skill holds the record format, the method, the checks, the findings schema and the return contract. This agent adds nothing to it.
