//! parity-stage: characterization
//! parity-oracle: fno.agents.provider_outage

use fno_agent::watchdog::outage::{
    fold_provider_outages, measure_and_persist, OutageEvidence, OutagePolicy,
};
use serde_json::{json, Value};
use std::path::Path;

fn golden(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/watchdog")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Compared as text: Value equality ignores key order, and the order is part of
/// the contract.
fn same(actual: &Value, expected: &Value, label: &str) {
    assert_eq!(actual.to_string(), expected.to_string(), "{label}");
}

fn records_of(value: &Value) -> Vec<OutageEvidence> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| OutageEvidence::from_json(item).unwrap())
        .collect()
}

#[test]
fn the_fold_answers_the_frozen_reports_and_journal_states() {
    let frozen = golden("outage_fold.json");
    let now_s = frozen["now_s"].as_f64().unwrap();
    for case in frozen["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let records = records_of(&case["records"]);
        let prior = Some(&case["prior"]).filter(|value| !value.is_null());
        let (report, state) =
            fold_provider_outages(&records, prior, now_s, &OutagePolicy::default());
        same(&report, &case["report"], name);
        same(&state, &case["state"], name);
    }
}

#[test]
fn the_journal_keeps_breaker_epochs_and_a_damaged_one_reads_unknown() {
    let frozen = golden("outage_fold.json");
    let now_s = frozen["now_s"].as_f64().unwrap();
    let records = records_of(&frozen["cases"][0]["records"]);
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("recovery").join("provider-outages.json");
    let policy = OutagePolicy::default();

    let first = measure_and_persist(&records, now_s, Some(&journal), &policy);
    let opened = first["breakers"][0]["opened_at"].clone();
    assert_eq!(opened, json!(now_s));
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&journal).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 1);

    let later = measure_and_persist(&[], now_s + 60.0, Some(&journal), &policy);
    assert_eq!(later["breakers"][0]["opened_at"], opened);
    assert_eq!(later["instrument"], "measured");

    std::fs::write(&journal, "{ damaged").unwrap();
    let damaged = measure_and_persist(&records, now_s, Some(&journal), &policy);
    same(
        &damaged,
        &json!({
            "instrument": "unknown",
            "breakers": [],
            "counts": {"journal_unreadable": 1},
            "refusals": [{"reason": "journal_unreadable", "path": journal.display().to_string()}],
        }),
        "damaged",
    );
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), "{ damaged");
}
