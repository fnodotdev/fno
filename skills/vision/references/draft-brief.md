# Draft brief

You draft one VISION.md for the repo you are pointed at. You write one file and nothing else. You run no build, no test, and no git write.

## Read, in this order, skipping what is absent

1. Read `README.md`. It says what the project does in its own words.
2. When the repo names customers or pricing intent, read `PRODUCT.md`.
3. When the repo is fno-enabled, read the backlog: `fno backlog board` and the content of its open nodes. They say what the team is actually building.
4. List merged PR titles: `gh pr list --state merged --limit 50 --json title -q '.[].title'`. Titles are the shipped truth.
5. When the repo is fno-enabled, read recorded rulings: `fno inbox decisions` over the repo's recent subjects. Rulings are promises the project already made.

## Shape

Write the draft with these sections, in this order, mirroring fno's own docs/vision.md:

1. An opening line: what this page is and who reads it (judge, merge check, lead).
2. `## What <project> is` - two or three short paragraphs.
3. `## Who it is for` - an ordered list, first user first, plus one "not for" paragraph.
4. `## Product pillars` - three to six promises, each with the features that keep it.
5. `## What <project> will not build` - the refusal list. A good refusal list is specific.
6. `## Values` - three to five, each one sentence.
7. `## North star` - one metric plus input metrics and guardrails.
8. `## Is this PR on track?` - five questions a lead answers in one line each.

Every claim in the draft carries its source: the README line, the PR title, or the node name. Write short sentences. A paragraph is one physical line.

## Write

Write the draft to `VISION.md` at the repo root. Report the file path, one line per section saying what it drew from, and the sources you skipped. Your report states sources, not praise.

