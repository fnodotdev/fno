---
description: A target run in a project that registers its own skill as a reviewer should run that skill before the PR is done.
max_turns: 40
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Skill, Bash, Edit, Write]
---

Run /fno:target on this, end to end, to a pull request ready to merge: make greet.sh print "Hello, world!" with an exclamation mark.
