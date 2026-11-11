//! The navigator's ranking: which rows a query keeps and in what order.
//!
//! Every row the old substring filter kept still matches. A row whose label
//! spells the query in order with gaps (`frv` for `fno-review`) now matches
//! too. Rows sort by how well the label matches: the whole name, then the
//! start of a name, then the start of a word, then anywhere in the label, then
//! a subsequence inside one name. A row that matches only through its hidden identity tokens
//! (pane id, PR, model) sorts last. The sort is stable, so rows in one tier
//! keep the order `nav_rows` built them in.

use super::*;

/// How well `label` matches `q`. Lower ranks first; `None` drops the row.
/// Both sides arrive lowercased. `match_key` is the label plus the row's
/// identity tokens, so a hit there and not in the label is an identity hit.
pub(super) fn nav_score(q: &str, label: &str, match_key: &str) -> Option<u8> {
    if q.is_empty() {
        return Some(0);
    }
    // A label is `squad › tab › row`. Each segment is a name the operator
    // knows a row by, so a name match counts in any segment.
    let segments = || label.split(" › ").map(str::trim);
    let words = || {
        label
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
    };
    if segments().any(|s| s == q) {
        Some(0)
    } else if segments().any(|s| s.starts_with(q)) {
        Some(1)
    } else if words().any(|w| w.starts_with(q)) {
        Some(2)
    } else if label.contains(q) {
        Some(3)
    // Gaps stay inside one segment: a subsequence that jumps the separator
    // (`notes` across `footnote › shell`) is noise, not a name.
    } else if segments().any(|s| crate::search_query::fuzzy_hit(q, s)) {
        Some(4)
    } else if match_key.contains(q) {
        Some(5)
    } else {
        None
    }
}

impl View {
    /// The navigator rows matching the current text + state filter, ranked
    /// by [`nav_score`] and recomputed per keypress (no cache). Text and
    /// state compose (both must match); letters only ever edit the query
    /// (Locked 5).
    pub(super) fn nav_filtered(&self, nav: &NavView) -> Vec<NavRow> {
        let q = nav.query.to_lowercase();
        let mut scored: Vec<(u8, NavRow)> = self
            .nav_rows()
            .into_iter()
            .filter(|r| nav.state_filter.is_none_or(|s| r.state == s))
            .filter_map(|r| nav_score(&q, &r.label.to_lowercase(), &r.match_key).map(|s| (s, r)))
            .collect();
        scored.sort_by_key(|(s, _)| *s);
        scored.into_iter().map(|(_, r)| r).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::nav_score;

    #[test]
    fn a_name_start_outranks_an_inner_word_and_gaps_still_match() {
        // AC4-HP: `rev` starts the name `reviewer` but only an inner word of
        // `fno-review`; `frv` reaches `fno-review` by subsequence alone.
        let rank = |q: &str, l: &str| nav_score(q, l, l);
        assert!(rank("rev", "reviewer") < rank("rev", "fno-review"));
        assert_eq!(rank("frv", "fno-review"), Some(4));
        assert_eq!(rank("frv", "reviewer"), None);
        assert_eq!(rank("build", "nairobi › build"), Some(0));
        assert_eq!(rank("notes", "footnote › shell"), None);
        assert_eq!(nav_score("3706", "worker", "worker 3706"), Some(5));
    }
}
