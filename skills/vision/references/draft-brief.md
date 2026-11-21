# Draft brief

You draft one VISION.md for the repo you are pointed at. You write one file and nothing else. You run no build, no test, and no git write.

## Read, in this order, skipping what is absent

1. `README.md` - what the project does in its own words.
2. `PRODUCT.md` - customers, personas, and pricing intent, when the repo names them.
3. The backlog, when the repo is fno-enabled: `fno backlog board` and the scope's open nodes. They say what the team is actually building.
4. Merged PR titles: `gh pr list --state merged --limit 50 --json title -q '.[].title'`. Titles are the shipped truth.
5. Recorded rulings, when the repo is fno-enabled: `fno inbox decisions <recent subjects>` and the repo's law or decision docs. Rulings are promises the project already made.

Skip a silent source without comment. Never invent a fact a source did not give you: no invented metrics, no invented customers, no invented dates. A section with no evidence stays one honest line naming the gap.

## Shape

Write the draft with these sections, in this order, mirroring fno's own docs/vision.md:

1. An opening line: what this page is and who reads it (the plan judge, the merge check, a lead checking a node).
2. `## What <project> is` - two or three short paragraphs.
3. `## Who it is for` - an ordered list, first user first, plus one "not for" paragraph.
4. `## Product pillars` - three to six promises, each with the features that keep it.
5. `## What <project> will not build` - the refusal list. A good refusal list is specific.
6. `## Values` - three to five, each one sentence.
7. `## North star` - one metric plus input metrics and guardrails.
8. `## Is this PR on track?` - five questions a lead answers in one line each.

Every claim in the draft carries its source in the text or a footnote: the README line, the PR title list, the node name. Write short sentences. A paragraph is one physical line.

## Write

Write the draft to `VISION.md` at the repo root. Report: the file path, one line per section saying what it drew from, and the list of sources you skipped. Your report is what the caller quotes in the grill and the PR body, so it states sources, not praise.
