use super::*;
use crate::store_remote::test_primary;
use serde_json::json;

#[test]
fn a_number_is_the_key_and_an_answer_takes_its_questions_number_once() {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph.json");
    let conn = crate::backlog::open(&graph).unwrap();
    let db = crate::backlog::database_path(&graph);
    conn.execute_batch(super::super::DDL).unwrap();
    // A record that predates the mint keeps its hex key until the migration.
    super::super::mint_local(&conn, "q-0000abcd", "local", "h").unwrap();

    let key = mint_key_at(&db, None, "h").unwrap().unwrap();
    assert_eq!(key, "D2", "one sequence with the hex-keyed rows");
    assert_eq!(
        super::super::resolve_in(Some(&conn), 'q', "d2", "local").as_deref(),
        Some("D2")
    );
    assert_eq!(
        super::super::label_in(&conn, "D2", "local").as_deref(),
        Some("D2")
    );

    let answer = |qid: &str| answer_key_at(&db, qid, None, "h").unwrap().unwrap();
    for (qid, label) in [("D2", "D2"), ("q-0000abcd", "D1")] {
        assert_eq!(answer(qid), label);
        let event = json!({"type": "operator_decision", "source": "test",
            "data": {"decision_id": label, "question_id": qid, "decision": "yes"}});
        crate::backlog::decisions::insert_event(&conn, &event).unwrap();
        // The decision door reads the number as the decision, not the question.
        assert_eq!(
            super::super::resolve_in(Some(&conn), 'd', label, "local").as_deref(),
            Some(label)
        );
        // `event_id` is UNIQUE, so a second answer needs a number of its own.
        assert_ne!(answer(qid), label, "a second answer would be dropped");
    }
}

#[test]
fn an_unreachable_primary_queues_with_no_id_and_the_next_beat_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph.db");
    let down = test_primary::dead();
    assert_eq!(mint_key_at(&db, Some(&down), "h").unwrap(), None);

    let index = dir.path().join("questions.jsonl");
    let ask = |text: &str| Queued::Question {
        event: json!({"ts": "2026-11-21T00:00:00Z", "type": "operator_question", "source": "target",
            "data": {"question": text, "subject": "release", "trace": {"trace_id": "none"}}}),
        journal: dir.path().join("events.jsonl"),
        index: index.clone(),
    };
    enqueue(&db, &ask("ship?")).unwrap();
    // A re-ask on the same subject amends the queued row.
    enqueue(&db, &ask("ship it today?")).unwrap();
    let waiting = queued_questions(&db);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting.first().unwrap().title, "ship it today?");
    assert_eq!(drain_at(&db, Some(&down), "h").unwrap(), 0);
    assert!(crate::question_intake::question_rows(&index).0.is_empty());

    let primary = test_primary::start();
    assert_eq!(drain_at(&db, Some(&primary.remote), "h").unwrap(), 1);
    let (asked, _) = crate::question_intake::question_rows(&index);
    let row = asked.get("D1").expect("recorded under its number");
    assert_eq!(row["data"]["question_id"], json!("D1"));
    assert_eq!(row["data"]["trace"]["span_id"], json!("D1"));
    assert!(queued_questions(&db).is_empty());
    assert_eq!(drain_at(&db, Some(&primary.remote), "h").unwrap(), 0);
}
