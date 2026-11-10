//! The blind CPU read's admission contract, tested against `run_gate` itself.
//! Split from spawn_gate.rs to keep the over-budget file shrinking.

use super::*;

/// The fixtures below exercise the agent-fan-out arms of the gate, so they
/// carry the worker identity those arms key on. Removed on drop, before the
/// env lock releases.
struct AgentSelfFixture;
impl AgentSelfFixture {
    fn set() -> Self {
        std::env::set_var("FNO_AGENT_SELF", "gate-fixture-worker");
        AgentSelfFixture
    }
}
impl Drop for AgentSelfFixture {
    fn drop(&mut self) {
        std::env::remove_var("FNO_AGENT_SELF");
    }
}

/// The shared admission fixture's payload for one verdict, as the probe
/// would print it: the one fixture both readers pin, so these tests
/// cannot grow their own reading.
fn fixture_payload(verdict: &str) -> String {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/spawn_gate/spawn_gate_admission.json");
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&fixture_path)
            .expect("the shared fixture must exist beside the Python suite"),
    )
    .expect("fixture is JSON");
    let case = doc["cases"]
        .as_array()
        .expect("fixture carries cases")
        .iter()
        .find(|c| c["payload"]["admission"]["verdict"] == verdict)
        .unwrap_or_else(|| panic!("fixture carries a {verdict} case"));
    serde_json::to_string(&case["payload"]).unwrap()
}

/// AC1-HP: an undecidable band admits at once with a warning - the gate
/// never refuses on it and never re-reads: one probe pass, one warning,
/// one admission.
#[test]
fn an_undecidable_band_admits_with_a_warning() {
    let _g = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _id = AgentSelfFixture::set();
    let dir = std::env::temp_dir().join(format!("fno-gate-blind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prior_claims_root = std::env::var_os("FNO_CLAIMS_ROOT");
    let root = dir.join("claims-root");
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("FNO_CLAIMS_ROOT", &root);
    let prior_spawn_gate = std::env::var_os("FNO_SPAWN_GATE");
    std::env::remove_var("FNO_SPAWN_GATE");
    let prior_config = std::env::var_os("FNO_CONFIG");
    std::env::remove_var("FNO_CONFIG");
    let prior_payload = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD");
    std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD");
    let prior_payload_seq = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ");
    let seq = dir.join("payload-seq.txt");
    let blind = format!("{}\n", fixture_payload("undecidable"));
    std::fs::write(&seq, format!("{blind}{blind}{blind}")).unwrap();
    std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ", &seq);
    let fnodir = dir.join(".fno");
    std::fs::create_dir_all(&fnodir).unwrap();
    std::fs::write(
        fnodir.join("config.toml"),
        "[agents]\nmax_live = 999\nmin_free_gb = 0\nmax_swap_pct = 0\n",
    )
    .unwrap();
    // An empty registry: the only thing this run must survive past the CPU
    // axis is the slot census, and zero rows against 999 passes it.
    let reg = dir.join("registry.json");
    std::fs::write(
        &reg,
        format!(
            r#"{{"schema_version":{},"entries":[]}}"#,
            crate::state::REGISTRY_SCHEMA_VERSION
        ),
    )
    .unwrap();

    let got = run_gate(
        &dir,
        &reg,
        GateInput {
            name: "w1".into(),
            substrate: "bg".into(),
            flags: GateFlags {
                force: false,
                no_wait: false,
            },
            ..Default::default()
        },
    );

    match prior_claims_root {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
    std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ");
    match prior_payload_seq {
        Some(value) => std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ", value),
        None => std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ"),
    }
    match prior_spawn_gate {
        Some(value) => std::env::set_var("FNO_SPAWN_GATE", value),
        None => std::env::remove_var("FNO_SPAWN_GATE"),
    }
    match prior_config {
        Some(value) => std::env::set_var("FNO_CONFIG", value),
        None => std::env::remove_var("FNO_CONFIG"),
    }
    match prior_payload {
        Some(value) => std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD", value),
        None => std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD"),
    }

    let guard = got.expect("an undecidable band admits with a warning");
    let _ = guard;
    let left = std::fs::read_to_string(&seq).unwrap();
    assert_eq!(
        left.matches("\"verdict\":\"undecidable\"").count(),
        2,
        "one pass consumed the first reading only: {left}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC1-EDGE: a blind read admits at once with its warning; the readable
/// answer behind it is never read.
#[test]
fn a_blind_read_then_an_admit_admits() {
    let _g = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _id = AgentSelfFixture::set();
    let dir = std::env::temp_dir().join(format!("fno-gate-admit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prior_claims_root = std::env::var_os("FNO_CLAIMS_ROOT");
    let root = dir.join("claims-root");
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("FNO_CLAIMS_ROOT", &root);
    let prior_spawn_gate = std::env::var_os("FNO_SPAWN_GATE");
    std::env::remove_var("FNO_SPAWN_GATE");
    let prior_config = std::env::var_os("FNO_CONFIG");
    std::env::remove_var("FNO_CONFIG");
    let prior_payload = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD");
    std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD");
    let prior_payload_seq = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ");
    let seq = dir.join("payload-seq.txt");
    let admit = r#"{"admission":{"verdict":"admit","axis":"fleet_cpu_share","reason":"fixture","bound":"exact","ceiling":0.5}}"#;
    std::fs::write(
        &seq,
        format!("ERR footprint unavailable: worker root liveness unavailable\n{admit}\n"),
    )
    .unwrap();
    std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ", &seq);
    let fnodir = dir.join(".fno");
    std::fs::create_dir_all(&fnodir).unwrap();
    std::fs::write(
        fnodir.join("config.toml"),
        "[agents]\nmax_live = 999\nmin_free_gb = 0\nmax_swap_pct = 0\n",
    )
    .unwrap();
    // An empty registry: the only thing this run must survive past the CPU
    // axis is the slot census, and zero rows against 999 passes it.
    let reg = dir.join("registry.json");
    std::fs::write(
        &reg,
        format!(
            r#"{{"schema_version":{},"entries":[]}}"#,
            crate::state::REGISTRY_SCHEMA_VERSION
        ),
    )
    .unwrap();

    let got = run_gate(
        &dir,
        &reg,
        GateInput {
            name: "w1".into(),
            substrate: "bg".into(),
            flags: GateFlags {
                force: false,
                no_wait: false,
            },
            ..Default::default()
        },
    );

    match prior_claims_root {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
    std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ");
    match prior_payload_seq {
        Some(value) => std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ", value),
        None => std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ"),
    }
    match prior_spawn_gate {
        Some(value) => std::env::set_var("FNO_SPAWN_GATE", value),
        None => std::env::remove_var("FNO_SPAWN_GATE"),
    }
    match prior_config {
        Some(value) => std::env::set_var("FNO_CONFIG", value),
        None => std::env::remove_var("FNO_CONFIG"),
    }
    match prior_payload {
        Some(value) => std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD", value),
        None => std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD"),
    }

    assert!(
        got.is_ok(),
        "a readable admit after a blind read admits: {got:?}"
    );
    // Positive control: the blind line was consumed, the readable answer
    // behind it was never read.
    let left = std::fs::read_to_string(&seq).unwrap();
    assert!(left.contains("\"verdict\":\"admit\""), "{left}");
    assert!(!left.contains("ERR "), "{left}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC1-ERR: an instrument that stays unreadable through the whole budget
/// admits: probe failure is not overload (guard diet, law d-9d4e95e5).
#[test]
fn an_unreadable_instrument_admits() {
    let _g = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _id = AgentSelfFixture::set();
    let dir = std::env::temp_dir().join(format!("fno-gate-dead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prior_claims_root = std::env::var_os("FNO_CLAIMS_ROOT");
    let root = dir.join("claims-root");
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("FNO_CLAIMS_ROOT", &root);
    let prior_spawn_gate = std::env::var_os("FNO_SPAWN_GATE");
    std::env::remove_var("FNO_SPAWN_GATE");
    let prior_config = std::env::var_os("FNO_CONFIG");
    std::env::remove_var("FNO_CONFIG");
    let prior_payload = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD");
    std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD");
    let prior_payload_seq = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ");
    let seq = dir.join("payload-seq.txt");
    let err_line = "ERR footprint unavailable: worker root liveness unavailable\n".repeat(3);
    std::fs::write(&seq, err_line).unwrap();
    std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ", &seq);
    let fnodir = dir.join(".fno");
    std::fs::create_dir_all(&fnodir).unwrap();
    std::fs::write(
        fnodir.join("config.toml"),
        "[agents]\nmax_live = 999\nmin_free_gb = 0\nmax_swap_pct = 0\n",
    )
    .unwrap();
    // An empty registry: the only thing this run must survive past the CPU
    // axis is the slot census, and zero rows against 999 passes it.
    let reg = dir.join("registry.json");
    std::fs::write(
        &reg,
        format!(
            r#"{{"schema_version":{},"entries":[]}}"#,
            crate::state::REGISTRY_SCHEMA_VERSION
        ),
    )
    .unwrap();

    let got = run_gate(
        &dir,
        &reg,
        GateInput {
            name: "w1".into(),
            substrate: "bg".into(),
            flags: GateFlags {
                force: false,
                no_wait: false,
            },
            ..Default::default()
        },
    );

    match prior_claims_root {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
    std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ");
    match prior_payload_seq {
        Some(value) => std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ", value),
        None => std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD_SEQ"),
    }
    match prior_spawn_gate {
        Some(value) => std::env::set_var("FNO_SPAWN_GATE", value),
        None => std::env::remove_var("FNO_SPAWN_GATE"),
    }
    match prior_config {
        Some(value) => std::env::set_var("FNO_CONFIG", value),
        None => std::env::remove_var("FNO_CONFIG"),
    }
    match prior_payload {
        Some(value) => std::env::set_var("FNO_TEST_FOOTPRINT_PAYLOAD", value),
        None => std::env::remove_var("FNO_TEST_FOOTPRINT_PAYLOAD"),
    }

    let guard = got.expect("an unreadable probe admits");
    let _ = guard;
    let left = std::fs::read_to_string(&seq).unwrap();
    assert_eq!(
        left.matches("ERR footprint unavailable").count(),
        2,
        "one pass consumed the first ERR line only: {left}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
