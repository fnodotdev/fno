//! The recipient witness. fno types every mail turn under a `@sender · fmail-id` header and logs it on the bus, and the mux logs every Enter a person presses in a pane. A user turn with none of the three came from a writer fno cannot see: a person typing through `claude attach`, or a foreign script that holds Claude's control key. fno cannot refuse the script, since the socket and the key are Claude Code's, so it records the turn and tells the operator.

use std::path::Path;

use super::provenance::{classify_text, BusIndex, Verdict};

/// Only turns this recent are witnessed: a first pass over a long transcript must not page the operator about last month.
pub(crate) const WITNESS_WINDOW_S: f64 = 86_400.0;

/// One user turn that landed with no header, no bus row and no mux submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Headerless {
    pub(crate) session: String,
    pub(crate) at_ms: i64,
}

/// The witness read for one received turn. `submitted` answers whether the mux saw a person press Enter for it; it runs only for a turn the text rules leave as a person's.
pub(crate) fn headerless(
    session: &str,
    text: &str,
    at_ms: i64,
    bus: &BusIndex,
    submitted: impl FnOnce() -> bool,
) -> Option<Headerless> {
    match classify_text(text, Some((bus, session))) {
        Verdict::Operator(_) if !submitted() => Some(Headerless {
            session: session.to_string(),
            at_ms,
        }),
        _ => None,
    }
}

/// Record `found` once in `store` and say whether this call recorded it. The first turn a session gets on a UTC day also sends one operator notice; later turns that day are recorded only.
pub(crate) fn record(found: &Headerless, store: &Path) -> bool {
    let key = format!("headerless_turn:{}:{}", found.session, found.at_ms);
    if !super::operator_notice::mark_once(store, &key, "recorded") {
        return false;
    }
    let at = chrono::DateTime::from_timestamp_millis(found.at_ms).unwrap_or_default();
    let day = at.format("%Y-%m-%d").to_string();
    let notice_key = format!("headerless_turn_notice:{}", found.session);
    if super::operator_notice::mark_once(store, &notice_key, &day) {
        super::operator_notice::notify_operator(
            "Turn with no mail header",
            &format!(
                "Session {} got a turn at {} with no mail header, no bus row and no mux submit. A person typed through claude attach, or a script wrote to its control socket.",
                found.session,
                at.to_rfc3339()
            ),
            None,
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn witness_flags_a_headerless_turn() {
        let bus = BusIndex::empty();
        let found = headerless(
            "s-1",
            "please merge the PR now",
            1_700_000_000_000,
            &bus,
            || false,
        );
        assert_eq!(
            found,
            Some(Headerless {
                session: "s-1".into(),
                at_ms: 1_700_000_000_000
            })
        );
        // A mail turn, and a turn the mux saw a person submit, are not flagged.
        assert_eq!(
            headerless("s-1", "`@zenobia · fmail-1a2b`\nbody", 1, &bus, || false),
            None
        );
        assert_eq!(
            headerless("s-1", "please merge the PR now", 1, &bus, || true),
            None
        );

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("witness.json");
        let turn = found.unwrap();
        assert!(record(&turn, &store), "the first scan records the turn");
        assert!(!record(&turn, &store), "a second scan records nothing new");
        let text = std::fs::read_to_string(&store).unwrap();
        assert!(text.contains("headerless_turn:s-1:1700000000000"), "{text}");
    }
}
