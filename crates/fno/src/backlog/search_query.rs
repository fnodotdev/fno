//! The one search grammar lives in `fno_proto::feed::grammar`, shared with
//! the fno-agent reader (ruling d-faf6a93b: one grammar, one meaning per key
//! on every surface). This module adds what only fno knows: the registry join
//! and the board's sequential-id setting. The static page's JS twin is still
//! held to this grammar by `search_query_cases.json`.

use crate::agents_view::RegistryAgent;
pub use fno_proto::feed::grammar::*;

/// The sequential-id test, read from this machine's board settings.
fn sequential_tail(head: &str, tail: &str) -> bool {
    super::id_mode::sequential_tail_matches(super::id_mode::facts(), head, tail)
}

/// Parse one query. Installs the board's sequential-id test first, so a
/// `<prefix>-<n>` id reads as a node on a sequential board.
pub fn parse(input: &str, surface: Surface, now: i64) -> Result<Parsed, ParseError> {
    set_sequential_tail(sequential_tail);
    fno_proto::feed::grammar::parse(input, surface, now)
}

/// The session directory over registry rows: every id a row answers to.
pub fn session_directory(rows: &[RegistryAgent]) -> SessionDirectory {
    SessionDirectory::from_entries(rows.iter().filter_map(|r| {
        let ids: Vec<String> = [
            r.harness_session_id.as_deref(),
            r.session_id.as_deref(),
            r.attach_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::to_string)
        .collect();
        (!ids.is_empty()).then(|| SessionEntry {
            name: r.name.clone(),
            account: r.account.clone(),
            model: r.model.clone(),
            harness: r.harness.clone(),
            spawned_by_session: r.spawned_by_session.clone(),
            exited: r.exited,
            ids,
        })
    }))
}
