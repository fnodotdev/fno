/// AC3: an ok attention_ledger reading renders exactly one line starting
/// "attention ledger".
#[test]
fn attention_ledger_ok_reading_renders_one_line() {
    let data = Map::new();
    let payload = crate::backlog::attention_ledger::ReadingPayload {
        week: "2026-W46".into(),
        line: "attention ledger: no merged PRs (2 asks, 0 merged), 1 repeated subject(s), 0 revert(s), 3 guard refusal(s)".into(),
        ledger: crate::backlog::attention_ledger::Ledger {
            week: "2026-W46".into(),
            questions_filed: 2,
            merged_prs: 0,
            questions_per_merged_pr: None,
            repeated_subjects: 1,
            reverts: 0,
            cycle_hours_median: None,
            cycle_hours_p90: None,
            guard_refusals: 3,
            operator_queue_depth: None,
            notes: vec![],
        },
    };
    let readings = vec![Reading::took(
        "attention_ledger",
        serde_json::to_value(payload).unwrap(),
    )];
    let lines = render_lines("x-bbbb", &readings, &data, &None, "", "no change");
    let hits: Vec<_> = lines
        .iter()
        .filter(|l| l.starts_with("attention ledger"))
        .collect();
    assert_eq!(hits.len(), 1, "exactly one attention ledger line");
    assert!(hits.first().is_some_and(|l| l.contains("2 asks")));
}

/// AC4: a failed attention_ledger reading renders the named failure line
/// and nothing panics.
#[test]
fn attention_ledger_failed_reading_renders_the_failure_line() {
    let data = Map::new();
    let readings = vec![Reading::failed(
        "attention_ledger",
        "beat deadline exceeded (reader timed out)".to_string(),
    )];
    let lines = render_lines("x-bbbb", &readings, &data, &None, "", "no change");
    assert!(lines
        .iter()
        .any(|l| l.starts_with("READER FAILED attention_ledger: beat deadline exceeded")));
}
