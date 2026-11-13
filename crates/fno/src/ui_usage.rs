//! The `ui_click` row: one per click or tap on a UI control, with the
//! control's stable id and the screen the click landed in. The usage events
//! arm ships it (docs/telemetry.md); law D3283 keeps it ids-only, never a
//! label, a path or free text.
//!
//! The emitters are the mux TUI client, the desktop app (`fno-app`), and the
//! web bridge on behalf of a served page (the phone surface). A click the
//! caller never resolved to a control ships nothing, so telemetry fails open
//! and never blocks input: the writers ignore [`click`]'s outcome.

/// An id is a code constant, so its charset is narrower than the journal's
/// identifier gate: lowercase words joined by `_`, digits allowed, at most
/// 64 characters.
fn id_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The row for one click on `control` while `screen` was up.
pub fn envelope(screen: &str, control: &str) -> serde_json::Value {
    serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "type": "ui_click",
        "source": "ui",
        "data": {"screen": screen, "control": control},
    })
}

/// Write the row. A dropped id (a label leaked in, an overlong value) or a
/// failed write lands nowhere and tells nobody: clicks are a count signal,
/// not an audit trail.
pub fn click(screen: &str, control: &str) {
    if !id_ok(screen) || !id_ok(control) {
        return;
    }
    let Some(journal) = crate::cli_usage::agents_journal() else {
        return;
    };
    let row = envelope(screen, control);
    drop(crate::event_store::append_envelope(
        &journal,
        &row.to_string(),
        None,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id gate takes lowercase constants and refuses labels, paths and
    /// overlong values, so a `data.screen` typo can never ship prose.
    #[test]
    fn the_id_gate_takes_constants_only() {
        assert!(id_ok("mux"));
        assert!(id_ok("web_hq"));
        assert!(id_ok("tab_row"));
        assert!(id_ok("answer2"));
        assert!(!id_ok(""));
        assert!(!id_ok("Tab Row"));
        assert!(!id_ok("Needs input"));
        assert!(!id_ok("tab-row"));
        assert!(!id_ok("/Users/bb16"));
        assert!(!id_ok(&"x".repeat(65)));
    }

    /// A click lands one row with the two ids and nothing else.
    #[test]
    fn a_click_lands_one_ids_only_row() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _env = ENV_LOCK.lock().unwrap();
        let saved = std::env::var_os("FNO_AGENTS_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_AGENTS_HOME", dir.path());
        click("mux", "tab_row");
        click("app", "Needs input");
        let journal = dir.path().join("events.jsonl");
        let rows = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery {
                types: vec!["ui_click".into()],
                ..Default::default()
            },
        )
        .unwrap();
        match saved {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
        assert_eq!(rows.len(), 1, "the refused label ships nothing: {rows:?}");
        let row = rows.first().expect("one row");
        #[derive(serde::Deserialize)]
        struct Line {
            r#type: String,
            source: String,
            data: LineData,
        }
        #[derive(serde::Deserialize)]
        struct LineData {
            screen: String,
            control: String,
        }
        let line: Line = serde_json::from_str(&row.line).unwrap();
        assert_eq!(line.r#type, "ui_click");
        assert_eq!(line.source, "ui");
        assert_eq!(line.data.screen, "mux");
        assert_eq!(line.data.control, "tab_row");
        let raw: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&row.line).unwrap();
        let data = raw
            .into_iter()
            .find(|(key, _)| key == "data")
            .map(|(_, value)| value)
            .and_then(|value| value.as_object())
            .expect("the envelope carries a data object");
        assert_eq!(data.len(), 2, "the row carries the two ids only: {data:?}");
    }
}
