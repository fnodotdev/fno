# New-user persona limitations

## Known Limitations and Deferred Work

- The labeled set holds six seed cases written with the skill, not cases from recorded reviews. Accuracy on six cases moves in steps of about 17 points, so one case decides an edit. Grow the set from real verdicts the user checks in the weekly slice.
- The persona reads the change as text. It does not install fno on a clean machine, so a step that fails only on a fresh install (a missing binary, a PATH gap) shows up only when the text names it.
- Nothing scores the persona's verdict files against the judge rows yet. The weekly report counts the judge rows only, so persona accuracy comes from the labeled set alone.
