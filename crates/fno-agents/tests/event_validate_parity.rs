//! parity-stage: characterization
//! parity-oracle: fno.events.validate
//!
//! Characterization tests for the native envelope judge, frozen against
//! goldens captured from the Python judge before that leg was deleted. The
//! goldens under tests/golden/event_validate/ freeze one
//! (exit, stdout, stderr) triple per corpus row (every row in
//! tests/py/events/parity_corpus.jsonl but one: the overflow-literals row is
//! answered by the door's substrate refusal). The native judge in
//! crates/fno-agents/src/runtime/event_store/validate.rs must answer every frozen
//! case identically. No oracle leg is left, so the goldens are the contract.

use common::{assert_golden, Golden};
use std::fs;
use std::path::PathBuf;

mod common;

/// Repo root: crates/fno-agents -> repo.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// (label, event JSON) per corpus row.
fn corpus_rows() -> Vec<(String, String)> {
    let path = repo_root().join("tests/py/events/parity_corpus.jsonl");
    let text = fs::read_to_string(path).expect("parity corpus is present");
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Rows with literals serde_json cannot represent (non-finite
        // overflow numbers) never reach the judge: the door answers them at
        // the substrate layer and the write still never lands. Their
        // Python-side diagnostic contract stays in the Python suite until
        // that leg retires.
        let row: serde_json::Value = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let label = row["reason"]
            .as_str()
            .expect("corpus rows carry reasons")
            .to_string();
        rows.push((
            label,
            serde_json::to_string(&row["event"]).expect("event reserializes"),
        ));
    }
    rows
}

/// The native judge's verdict for one corpus row, as a Golden.
fn run_rust_verdict(event_json: &str) -> Golden {
    match fno_agents::event_store::validate::judge_line(event_json) {
        fno_agents::event_store::validate::Verdict::Valid => Golden {
            exit: Some(0),
            streams: vec![String::new(), String::new()],
        },
        fno_agents::event_store::validate::Verdict::Invalid(msg) => Golden {
            exit: Some(1),
            streams: vec![String::new(), format!("{msg}\n")],
        },
        fno_agents::event_store::validate::Verdict::Substrate(msg) => Golden {
            exit: Some(2),
            streams: vec![String::new(), format!("{msg}\n")],
        },
    }
}

/// Every corpus row, one golden key each: the frozen golden is the contract.
#[test]
fn parity_over_corpus() {
    for (label, event_json) in corpus_rows() {
        let rust = run_rust_verdict(&event_json);
        assert_golden("event_validate", &label, &rust, None);
    }
}

/// Every string literal among an `emit_event(` call's leading arguments in
/// this crate is a schema event type or an envelope source, so a new emit
/// cannot land a type the judge refuses as unknown.
#[test]
fn every_emit_event_literal_is_a_schema_type_or_source() {
    #[derive(serde::Deserialize)]
    struct Projection {
        event_types: std::collections::BTreeMap<String, serde_json::Value>,
        allowed_sources: Vec<String>,
    }
    let schema: Projection = serde_json::from_str(
        &fs::read_to_string(
            repo_root().join("crates/fno-agents/src/runtime/event_store/events_schema.json"),
        )
        .expect("projection is present"),
    )
    .expect("projection parses");
    let known = |w: &str| {
        schema.event_types.contains_key(w) || schema.allowed_sources.iter().any(|s| s == w)
    };
    let mut stack = vec![repo_root().join("crates/fno-agents/src")];
    let mut unknown = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("src dir reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap_or_default();
            for (at, _) in text.match_indices("emit_event(") {
                if text[..at].ends_with("fn ") {
                    continue;
                }
                // The call's own argument span.
                let open = at + "emit_event(".len();
                let mut depth = 1usize;
                let mut end = open;
                for (i, c) in text[open..].char_indices() {
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                let span = &text[open..end];
                // Only an argument that is a bare string literal names a
                // type or a source; a key inside `event["kind"]` names neither.
                for arg in span.split(',').map(str::trim) {
                    let Some(lit) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) else {
                        continue;
                    };
                    let word = lit.chars().all(|c| c.is_ascii_lowercase() || c == '_');
                    if word && !lit.is_empty() && !known(lit) {
                        unknown.push(format!("{}: {lit}", path.display()));
                    }
                }
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "emitted types missing from schema.yaml: {unknown:?}"
    );
}
