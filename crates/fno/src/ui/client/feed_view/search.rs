//! The feed's search surface: the one shared grammar (`crate::search_query`)
//! parsed once per gesture and matched over [`event_fields`] for every row
//! the panel holds. The panel holds the whole history, so a query reads all
//! of it and no part of it is pushed into the projection. The keys overlay
//! and the search bar's completion read the same shared table, so the feed
//! never carries its own copy of the grammar.
//!
//! [`event_fields`]: crate::feed_overlay::event_fields

/// Parse one query for the feed surface; `Err` carries the shared refusal
/// (an unknown key, a node-only key) verbatim for the footer.
pub(crate) fn parse_query(text: &str) -> Result<crate::search_query::Parsed, String> {
    crate::search_query::parse(
        text,
        crate::search_query::Surface::Event,
        crate::search_query::now_secs(),
    )
    .map_err(|e| e.to_string())
}

/// The `?` overlay: the panel's keys, then every key the shared table marks
/// answerable on feed rows - read from `search_query::KEYS`, never a local
/// copy, so the overlay and the grammar cannot drift.
pub(crate) fn feed_keys_popup() -> crate::popup::Popup {
    use crate::popup::{Popup, PopupRow};
    let pick = |label: &str| PopupRow::Entry {
        glyph: " ".into(),
        label: label.into(),
        hint: String::new(),
        enabled: true,
    };
    let kind_word = |k: crate::search_query::Kind| match k {
        crate::search_query::Kind::Value => "value prefix",
        crate::search_query::Kind::Text => "text",
        crate::search_query::Kind::Date => "date",
        crate::search_query::Kind::Number => "number",
        crate::search_query::Kind::Age => "age",
        crate::search_query::Kind::Flag => "exact word",
        crate::search_query::Kind::Sort => "sort",
    };
    let mut rows: Vec<PopupRow> = vec![
        PopupRow::Header("keys".into()),
        PopupRow::Rule,
        pick("up/down row or group - enter details or fold"),
        pick("o sort - W group - f filter - x clear filters"),
        pick("z fold all - r retry"),
        pick("g first - G last - arrows pan"),
        pick("/ search - ? keys - esc close"),
        PopupRow::Header("query keys".into()),
        PopupRow::Rule,
    ];
    for def in crate::search_query::KEYS.iter().filter(|d| d.event) {
        let names: Vec<String> = def.names.iter().map(|n| format!("{n}:")).collect();
        rows.push(pick(&format!(
            "{} - {}",
            names.join(" "),
            kind_word(def.kind)
        )));
    }
    Popup::new(rows, crate::popup::Anchor::Center)
        .title("feed keys")
        .footer("esc close")
}

/// One Tab press on the bar text: complete the token before the cursor.
/// A key prefix completes to the full key names the event surface takes; a
/// value after a value key completes from the distinct values the panel
/// holds. Repeated Tab cycles. `None` leaves the text.
pub(crate) fn complete(text: &str, items: &[crate::feed_overlay::FeedItem]) -> Option<String> {
    let (prefix, token) = match text.rsplit_once(' ') {
        Some((p, t)) => (format!("{p} "), t),
        None => (String::new(), text),
    };
    if !token.contains(':') {
        let mut cands: Vec<&str> = Vec::new();
        for def in crate::search_query::KEYS.iter().filter(|d| d.event) {
            for name in def.names {
                if name.starts_with(token) && !cands.contains(name) {
                    cands.push(name);
                }
            }
        }
        cands.sort();
        if cands.is_empty() {
            return None;
        }
        let next = cands
            .iter()
            .position(|c| c.to_string() == token)
            .map(|i| i + 1)
            .unwrap_or(0);
        return cands
            .get(next % cands.len())
            .map(|c| format!("{prefix}{c}"));
    }
    let (key, value) = token.split_once(':')?;
    let field = match key {
        "id" | "n" => "id",
        "sid" | "session" => "session",
        "a" | "agent" => "agent",
        "h" | "harness" => "harness",
        "k" | "kind" => "kind",
        "l" | "lead" => "lead",
        _ => return None,
    };
    let ctx = crate::feed_overlay::EventCtx::default();
    let mut vals: Vec<String> = Vec::new();
    for it in items {
        for v in crate::feed_overlay::event_fields(it, &ctx)
            .get(field)
            .into_iter()
            .flatten()
        {
            if v.starts_with(value) && !vals.contains(v) {
                vals.push(v.clone());
            }
        }
    }
    vals.sort();
    if vals.is_empty() {
        return None;
    }
    let next = vals
        .iter()
        .position(|v| v == value)
        .map(|i| i + 1)
        .unwrap_or(0);
    vals.get(next % vals.len())
        .map(|v| format!("{prefix}{key}:{v}"))
}

/// The bar's text settled: parse the query and re-slot the held rows under
/// it. A refusal shows in the footer verbatim and leaves the rows as they
/// were.
pub(crate) fn apply_query(o: &mut super::FeedOverlay) {
    if o.query_text.is_empty() {
        o.parsed = None;
    } else {
        match parse_query(&o.query_text) {
            Ok(parsed) => o.parsed = Some(Ok(parsed)),
            Err(e) => {
                o.parsed = Some(Err(e));
                return;
            }
        }
    }
    super::reslot(o);
    o.sel = super::first_item_slot(&o.slots);
}
