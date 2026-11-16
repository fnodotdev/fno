use super::*;

use crate::loopcheck::budget::{resolve_account_cap_at, AccountCap};

/// AC5-ERR: required flags validated in parse_args, which returns Err.
#[test]
fn parse_args_missing_required_flags_err() {
    let no_state: Vec<String> = vec![
        "loop-check".into(),
        "--transcript".into(),
        "/t".into(),
        "--cwd".into(),
        "/c".into(),
    ];
    assert_eq!(
        parse_args(&no_state).unwrap_err(),
        "--state is required".to_string()
    );

    let no_transcript: Vec<String> = vec!["loop-check".into(), "--state".into(), "/s".into()];
    assert_eq!(
        parse_args(&no_transcript).unwrap_err(),
        "--transcript is required".to_string()
    );

    let no_cwd: Vec<String> = vec![
        "loop-check".into(),
        "--state".into(),
        "/s".into(),
        "--transcript".into(),
        "/t".into(),
    ];
    assert_eq!(
        parse_args(&no_cwd).unwrap_err(),
        "--cwd is required".to_string()
    );
}

#[test]
fn budget_flat_key_enforces_cost_cap_ab41b13d9d() {
    // Prove the flat budget_cap key enforces as cost cap for BOTH attended and
    // unattended - this is the fold-in test.
    let settings_cfg = "budget_cap = 0.10\n";
    let settings = parse_settings(settings_cfg);
    assert_eq!(settings.flat_budget_cap, Some(Ok(0.10)));
    // No nested blocks configured
    assert!(settings.attended_cost_cap_usd.is_none());
    assert!(settings.unattended_cost_cap_usd.is_none());
    // The budget resolver picks flat_budget_cap as cost cap fallback
    // for both attended=true and attended=false (tested in check_budget)

    let manifest_att = Manifest {
        session_id: Some("s1".into()),
        created_at: Some("2026-06-05T00:00:00Z".into()),
        attended: true,
        ..Default::default()
    };
    let manifest_unatt = Manifest {
        session_id: Some("s1".into()),
        created_at: Some("2026-06-05T00:00:00Z".into()),
        attended: false,
        ..Default::default()
    };

    // Ledger with cost > 0.10
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[{"session_id":"s1","cost_usd":0.50}]}"#,
    )
    .unwrap();

    let now: DateTime<Utc> = "2026-06-05T01:00:00Z".parse().unwrap();

    assert_eq!(
        check_budget(&manifest_att, &settings, &now, &ledger, None),
        Some(BudgetTrip::Cost {
            cap_usd: Some(0.10),
            spent_usd: 0.50
        }),
        "flat budget_cap must enforce for attended"
    );
    assert_eq!(
        check_budget(&manifest_unatt, &settings, &now, &ledger, None),
        Some(BudgetTrip::Cost {
            cap_usd: Some(0.10),
            spent_usd: 0.50
        }),
        "flat budget_cap must enforce for unattended"
    );
}

#[test]
fn cost_cap_does_not_trip_when_session_is_unmetered() {
    let manifest = Manifest {
        session_id: Some("unmetered".into()),
        budget_cost_cap_usd: Some(Ok(0.0)),
        ..Default::default()
    };
    let settings = Settings::default();
    let now: DateTime<Utc> = "2026-06-05T01:00:00Z".parse().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[{"session_id":"unmetered","cost_usd":null}]}"#,
    )
    .unwrap();

    assert_eq!(
        check_budget(&manifest, &settings, &now, &ledger, None),
        None,
        "an absent meter is unknown, not $0 against a zero cap"
    );
}

#[test]
fn check_budget_malformed_cost_cap_trips_budget() {
    // Fix 2: malformed cap in manifest -> Budget termination (fail-closed)
    let m = Manifest {
        session_id: Some("s".into()),
        created_at: Some("2026-06-05T00:00:00Z".into()),
        budget_cost_cap_usd: Some(Err("5.OO".into())),
        ..Default::default()
    };
    let s = Settings::default();
    let now: DateTime<Utc> = "2026-06-05T01:00:00Z".parse().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[{"session_id":"s","cost_usd":0.0}]}"#,
    )
    .unwrap();
    assert_eq!(
        check_budget(&m, &s, &now, &ledger, None),
        Some(BudgetTrip::Cost {
            cap_usd: None,
            spent_usd: 0.0
        }),
        "malformed cost cap must fail closed"
    );
}
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
fn check_budget_account_cap_spares_unmetered_session() {
    // AC2-EDGE: an unmetered session is unknown, not $0 against the cap.
    let c = Some(AccountCap {
        account: "a".into(),
        cap: Some(Ok(5.0)),
    });
    let trip = account_cap_case(
        "acct-unm",
        r#"{"entries":[{"session_id":"acct-unm","cost_usd":null}]}"#,
        c,
    );
    assert_eq!(
        trip, None,
        "an absent meter is unknown, not $0 against an account cap"
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

#[test]
fn check_budget_account_without_cap_never_trips() {
    // A record that sets no cap imposes none, even with spend on the meter.
    let c = Some(AccountCap {
        account: "a".into(),
        cap: None,
    });
    let trip = account_cap_case(
        "acct-s",
        r#"{"entries":[{"session_id":"acct-s","cost_usd":6.0}]}"#,
        c,
    );
    assert_eq!(trip, None, "an account record without a cap imposes none");
}

#[test]
fn resolve_account_cap_reads_registry_row_and_record_cap() {
    let temp = tempfile::tempdir().unwrap();
    let home = crate::paths::AgentsHome::at(temp.path().join("agents"));
    home.ensure_root().unwrap();
    crate::state::update_registry(&home.registry_json(), |registry| {
        registry.entries.push(crate::state::RegistryEntry {
            name: "t-worker".into(),
            harness: Some("claude".into()),
            harness_session_id: Some("h-acct".into()),
            account_record_id: Some("a".into()),
            cwd: temp.path().to_string_lossy().into_owned(),
            ..Default::default()
        });
    })
    .unwrap();
    // The config sits at <cwd>/.fno/config.toml, a layer candidates() reads
    // without touching the process-global FNO_CONFIG.
    let config_dir = temp.path().join(".fno");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[[accounts.records]]\nid = \"a\"\nname = \"A\"\nharness = \"claude\"\nauth = \"managed\"\ncost_cap_usd_per_session = 5\n",
    )
    .unwrap();
    let cap = resolve_account_cap_at(&home, temp.path(), Some("h-acct"));

    assert_eq!(
        cap.map(|c| (c.account, c.cap)),
        Some(("a".into(), Some(Ok(5.0)))),
        "the row's bound account names its record's cap"
    );
}
