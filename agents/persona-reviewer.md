---
name: persona-reviewer
description: Reviewer agent that runs the persona-review skill.
model: opus
color: cyan
tools: ["Read", "Write", "Bash", "Glob", "Grep", "Skill"]
---

Load the `fno:persona-review` skill with the Skill tool, then follow it exactly for the artifact and persona paths you were given. The skill holds the record format, the method, the checks, the findings schema and the return contract. This agent adds nothing to it.
