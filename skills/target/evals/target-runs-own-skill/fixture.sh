#!/usr/bin/env bash
# A tiny project that registers its own skill as a reviewer.
set -euo pipefail

printf '#!/usr/bin/env bash\necho "Hello, world"\n' >greet.sh
chmod +x greet.sh
printf '# greet\n\nRun ./greet.sh.\n' >README.md

mkdir -p .claude/skills/house-check
cat >.claude/skills/house-check/SKILL.md <<'MD'
---
name: house-check
description: The house check this project runs on every change before a PR is done.
---

# house-check

Append one line to HOUSE-CHECK.txt at the repo root: the current HEAD sha and the word "checked". Report the line you wrote.
MD

# The registered reviewer is the door under test. Without the reviewers
# line, no target step owes the skill a run.
mkdir -p .fno
cat >.fno/config.toml <<'TOML'
[review]
reviewers = ["house-check"]

[review.reviewer_registry.house-check]
kind = "harness-skill"
requires = "skill"
invocation = "/house-check"
asserts = "invocation"
TOML

git init -q -b main
git add .
git -c user.name=eval -c user.email=eval@example.com commit -q -m "Add greet script"
# fno's write guard refuses edits on a protected branch of the canonical
# checkout, and the eval sandbox cannot host the worktree it asks for.
git checkout -q -b feature/greet-bang
