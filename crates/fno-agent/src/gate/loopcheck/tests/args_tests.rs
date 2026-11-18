use super::*;

use crate::loopcheck::budget::AccountCap;

/// One check_budget account-arm case: a manifest and a one-entry ledger, answering the trip.
fn account_cap_case(session: &str, ledger: &str, c: Option<AccountCap>) -> Option<BudgetTrip> {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ledger.json");
    std::fs::write(&path, ledger).unwrap();
    let manifest = Manifest {
        session_id: Some(session.into()),
        ..Default::default()
    };
    let now: DateTime<Utc> = "2026-06-05T01:00:00Z".parse().unwrap();
    check_budget(&manifest, &Settings::default(), &now, &path, c.as_ref())
}

#[test]
fn check_budget_account_cost_cap_trips_per_account() {
    // AC1-HP: the bound account's cap trips when the meter crosses it.
    let c = Some(AccountCap {
        account: "a".into(),
        cap: Some(Ok(5.0)),
    });
    let trip = account_cap_case(
        "acct-s",
        r#"{"entries":[{"session_id":"acct-s","cost_usd":6.0}]}"#,
        c,
    );
    assert_eq!(
        trip,
        Some(BudgetTrip::AccountCost {
            account: "a".into(),
            cap_usd: Some(5.0),
            spent_usd: 6.0
        }),
        "the bound account's cap trips beside the session cap"
    );
}

#[test]
fn check_budget_malformed_account_cap_trips_fail_closed() {
    // A cap that cannot be parsed trips with the number unreadable.
    let c = Some(AccountCap {
        account: "a".into(),
        cap: Some(Err("5.OO".into())),
    });
    let trip = account_cap_case(
        "acct-s",
        r#"{"entries":[{"session_id":"acct-s","cost_usd":0.0}]}"#,
        c,
    );
    assert_eq!(
        trip,
        Some(BudgetTrip::AccountCost {
            account: "a".into(),
            cap_usd: None,
            spent_usd: 0.0
        }),
        "malformed account cap must fail closed"
    );
}
