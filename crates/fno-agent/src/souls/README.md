# Agent soul seeds

One stance per agent role, five to seven styles a lead picks from. Each
`SOUL.md` carries what the agent holds and how it talks: stance, values,
taste, voice. It carries no rules; rules live in AGENTS.md and the skills,
and a rule always wins over a soul.

Frontmatter follows the Agent Skills shape: `name` equals the style folder,
`description` says when to pick this style, and `metadata` is a flat map of
quoted strings (`role`, `fits`, `default`, `approved`). The body is exactly
four sections and under 40 lines. No worker names, ever.

Seeds copy once to `~/.fno/profile/agents/<role>/<style>/SOUL.md` on first
read and never overwrite: an edited soul is the user's, not ours. Handoff
lessons go to the territory canon doc, never into a soul.
