use super::*;

#[test]
fn unbacked_self_review_optout_defaults_to_obligation_on() {
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var_os("FNO_CLAIMS_ROOT");
    let root = std::env::temp_dir().join(format!("fno-loop-optout-{}", std::process::id()));
    std::env::set_var("FNO_CLAIMS_ROOT", &root);

    let settings = parse_settings("[review]\nself_review_required = false\n");

    assert_eq!(settings.self_review_required, None);
    match prior {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
}

#[test]
fn parse_manifest_harness_session_id_null_sentinel_is_none() {
    // init writes `harness_session_id: ${_HARNESS_SESSION:-null}`, so an
    // unresolvable session lands as the literal "null" and an empty value as
    // "". Both must read as None or a real attester compared against
    // Some("null") mislabels a self-attestation as other_session.
    for raw in ["null", ""] {
        let content = format!("---\nsession_id: abc\nharness_session_id: {raw}\n---\n");
        let m = parse_manifest(&content).unwrap();
        assert_eq!(
            m.harness_session_id, None,
            "harness_session_id: {raw:?} must parse as None"
        );
    }
    // A real id parses through unchanged.
    let m = parse_manifest(
        "---\nsession_id: abc\nharness_session_id: 3abddea3-ad19-481f-b0c1-af19043c95fe\n---\n",
    )
    .unwrap();
    assert_eq!(
        m.harness_session_id.as_deref(),
        Some("3abddea3-ad19-481f-b0c1-af19043c95fe")
    );
}

#[test]
fn parse_settings_malformed_fails_closed_not_zeroed() {
    // A malformed config.toml must NOT silently zero the gate (the old
    // fail-open); it fails CLOSED with an unsatisfiable sentinel so the ship
    // gate blocks visibly. Here: an unclosed table header.
    let cfg = "[review\nrequired_bots = []\n";
    assert!(
        parse_settings_result(cfg).is_err(),
        "malformed TOML must be a parse error"
    );
    let s = parse_settings(cfg);
    assert_eq!(
        s.required_bots,
        Some(vec![UNPARSEABLE_SETTINGS_SENTINEL.to_string()]),
        "a malformed file must fail closed, not zero the gate"
    );
    // The sentinel can never be satisfied by a real bot login.
    assert!(!login_matches_bot(
        "chatgpt-codex-connector",
        UNPARSEABLE_SETTINGS_SENTINEL
    ));
    // The person-gate vocabulary rides this declaration (G3 is shrink-only):
    // only the three words are kept; a typo stays None so the default
    // (today's behavior) applies, matching posture's lax contract.
    let s = parse_settings("[review]\nhuman = \"always\"\n");
    assert_eq!(s.human, Some(ReviewHuman::Always));
    let s = parse_settings("[review]\nhuman = \"never\"\n");
    assert_eq!(s.human, Some(ReviewHuman::Never));
    let s = parse_settings("[review]\nhuman = \"ui\"\n");
    assert_eq!(s.human, Some(ReviewHuman::Ui));
    let s = parse_settings("[review]\nhuman = \"alwayz\"\n");
    assert_eq!(
        s.human, None,
        "a typo degrades to the default, never a gate"
    );
    let s = parse_settings("[review]\nhuman = \"always extra\"\n");
    assert_eq!(s.human, None, "a padded word is not in the vocabulary");
    let s = parse_settings("");
    assert_eq!(s.human, None, "absent key reads the default");
    // The person gate's resolution contract rides this same declaration
    // (G3 is shrink-only): explicit ui and always owe a person's approval;
    // never and the absent key do not. One predicate serves both read sites
    // (review_inputs and run_done), so these pins cover pr status and the
    // stop gate together.
    assert!(!ReviewHuman::owed(None), "absent key owes no person");
    assert!(!ReviewHuman::owed(Some(ReviewHuman::Never)));
    assert!(ReviewHuman::owed(Some(ReviewHuman::Ui)));
    assert!(ReviewHuman::owed(Some(ReviewHuman::Always)));
}

#[test]
fn parse_settings_reviewers_malformed_mapping_fails_closed() {
    // A `{...}` mapping value must NOT drop to no-gate (Python raises here);
    // Rust stores an unsatisfiable sentinel so the gate stays active but can
    // never clear (codex peer review P1).
    let s = parse_settings("[review]\nreviewers = {a = \"b\"}\n");
    assert_eq!(s.reviewers, vec![MALFORMED_REVIEWERS_SENTINEL.to_string()]);
    let tmp = tempfile::tempdir().unwrap();
    let p = write_events(tmp.path(), &[]);
    assert!(
        !reviewers_all_attested(&p, &s.reviewers, "h"),
        "a malformed-reviewers sentinel must never be satisfiable"
    );
}
