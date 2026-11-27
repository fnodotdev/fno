//! The held history: every row the last fold landed, oldest first, and the
//! count of rows that arrived above a reader who scrolled away from the top.
//! No rendering, no IO - `apply_fold` lands each fold here and re-anchors
//! the view around it.

use crate::feed_overlay::FeedItem;

/// The rows the panel holds. The whole history, so sort, filter and group
/// read every row rather than a loaded page.
#[derive(Debug, Default)]
pub(crate) struct FeedWindow {
    /// Ascending: oldest first, the projection's order.
    pub items: Vec<FeedItem>,
    /// Rows newer than the newest one held when they landed, while the
    /// reader sat away from the top. `g` and the footer marker clear it.
    pub new_count: usize,
}

impl FeedWindow {
    /// Replace the rows with a fresh full read. With `detached`, the rows
    /// newer than the previous newest add to `new_count`; at the top they
    /// are simply on screen and the count clears.
    pub fn land(&mut self, rows: Vec<FeedItem>, detached: bool) {
        let newest = self.items.last().map(|r| super::table::ts_key_of(&r.ts));
        self.new_count = match (detached, newest) {
            (true, Some(newest)) => {
                self.new_count
                    + rows
                        .iter()
                        .filter(|r| super::table::ts_key_of(&r.ts) > newest)
                        .count()
            }
            _ => 0,
        };
        self.items = rows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(minute: usize) -> FeedItem {
        FeedItem {
            ts: format!("2026-09-01T00:{minute:02}:00Z"),
            kind: "node_created".into(),
            ..Default::default()
        }
    }

    /// A land replaces the rows whole; rows above a scrolled-away reader
    /// count, and a land at the top clears the count.
    #[test]
    fn land_replaces_rows_and_counts_new_ones_while_detached() {
        let mut w = FeedWindow::default();
        w.land((0..3).map(item).collect(), true);
        assert_eq!(w.items.len(), 3);
        assert_eq!(w.new_count, 0, "a first land counts nothing");
        w.land((0..5).map(item).collect(), true);
        assert_eq!(w.items.len(), 5, "the rows are the fresh read");
        assert_eq!(w.new_count, 2, "two rows arrived above the reader");
        w.land((0..6).map(item).collect(), true);
        assert_eq!(w.new_count, 3, "counts accumulate while detached");
        w.land((0..6).map(item).collect(), false);
        assert_eq!(w.new_count, 0, "a land at the top clears the count");
    }
}
