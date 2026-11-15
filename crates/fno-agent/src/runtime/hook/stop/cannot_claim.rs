//! The stop hook's cannot gate (verify-footnotes section 2 at the turn end).
//!
//! A final report that claims cannot and names no route tried goes back to
//! its author once, with the remedy in the block reason. One detector serves
//! both surfaces: the mail lint (rule 12 in `backlog::style_check`) and this
//! gate, so the rule cannot drift between them. The gate reads only the last
//! assistant message; quoted tool output and fenced logs are masked by the
//! same pass the mail lint uses.

use crate::backlog::style_check;

/// The block reason a gated report receives. It names the remedy, so one
/// retry passes: name the routes tried, or state the tool fact that refused.
pub(super) const GATE_REASON: &str = "This report claims cannot without naming the routes it tried. \
Before a cannot claim, try the other routes, then name each one tried in the claim, \
or state the tool fact that refused (\"that API is rate-limited\", never \"this cannot be read\"). \
If the routes are already tried, name them. skills/verify-footnotes/SKILL.md section 2 holds the rule.";

/// Some when the final report claims cannot and names no route tried.
pub(super) fn gate(message: &str) -> Option<String> {
    // A help emission already names its class and evidence - the route list
    // the rule asks for - and the distress router owns that stop before this
    // gate ever runs.
    if message.contains("<help") {
        return None;
    }
    style_check::has_unrouted_cannot_claim(message).then(|| GATE_REASON.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_blocks_an_unrouted_cannot_report_and_names_the_remedy() {
        let reason = gate("Blocked. Cannot reach the graph store today.\n").expect("gate fires");
        assert!(reason.contains("routes"), "{reason}");
        assert!(reason.contains("verify-footnotes"), "{reason}");
    }

    #[test]
    fn gate_passes_routed_claims_and_plain_reports() {
        // Route language clears the rule.
        assert!(gate("Cannot reach the store: tried the socket, then the reader.\n").is_none());
        // A report with no cannot claim never gates.
        assert!(gate("Shipped: the lint lands as a style note on the mail surface.\n").is_none());
        // A help emission names its class and evidence; the distress router
        // owns that stop.
        assert!(gate("<help class=\"stuck\" reason=\"r\" evidence=\"e\">x</help>").is_none());
        // A fenced log quoting a cannot-shaped line is not a claim.
        assert!(gate("```\nerror: cannot read file\n```\nThe build finished green.\n").is_none());
    }
}
