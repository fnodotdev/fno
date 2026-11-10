---
description: A premise that a grant is safe should get the withhold arm and the rollback rule, not a pass from a test that only proves the grant works.
max_turns: 12
timeout_seconds: 300
allowed_tools: [Read, Glob, Grep, Skill]
---

Before I start the build, use fno:verify-footnotes on this premise from the node: "The reporting role is read-only. test_reporting_role_can_select passes, so the grant is safe." Tell me how you will measure it against our dev Postgres. Do not run anything.
