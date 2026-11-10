# Demand premises

If a premise says a thing is worth building, load this page from Section 1. Examples are "users need X" and "this node is the top priority". A whether-to-build premise gets the same verdicts as any other premise. This page tells you what counts as a measurement of demand.

## Name one risk

Name the one assumption most likely to make the build a waste. Call it R1. Measure R1 first. Add at most three more risks. Each one must change the verdict. A long list of risks hides the one that matters.

## The demand ladder

Place the node on one rung. Read the rung from fno signals, not from the node's own words.

| Rung | What it means | fno signal |
|---|---|---|
| L0 | No user signal. "A competitor has it" or "it feels incomplete". | `source_kind` is `organic`, `from_observation` or `from_supervisor`, and there are no encounters |
| L1 | One ask. | one `operator_request`, or one encounter |
| L2 | Repeated asks, and no proof that work stops without it. | encounters from several sessions (`enc` in `fno backlog demand --json`) |
| L3 | Work stops without it. | an encounter whose evidence names lost work or a blocked run |
| L4 | The user said it blocks money, a client or a release. | a user ruling or note that says so |

Read the signals with `fno backlog get <node>` (`source_kind`, `request_origin`) and `fno backlog demand --json` (`enc`, `agent`, `operator`). A build is usually justified at L3 or L4. Below L3, the premise is not REFUTED. It is weaker than the build it asks for. Say so in the verdict.

## The evidence ladder

Grade every piece of evidence.

- **Weak:** the node's own wording, an agent's guess, a nice-to-have, a count of similar ideas filed.
- **Medium:** repeated asks from different sessions, a manual workaround that someone keeps doing, a competitor's traction on the exact need.
- **Strong:** a user request with a named cost. Encounter evidence that names lost work. A run that failed for the lack of it.

The verdict follows the strongest honest evidence, not the most dramatic risk.

## Where the verdict goes

- REFUTED (the demand is not there): emit a `help` with class `stale-plan`, or add a note on the node.
- UNMEASURED (only the owner knows): the question door, before any build.
- CONFIRMED or PARTIAL: build, and quote the rung and the strongest evidence in the first commit body.

This page adapts three parts of foundation-build-risk-review from product-on-purpose/pm-skills (Apache-2.0): the demand ladder, the evidence ladder and the one-R1 rule. It leaves out the market risk types and the four product verdicts.
