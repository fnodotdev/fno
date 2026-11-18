use super::super::intent::detect_intent_full;
use super::*;

#[test]
fn detect_intent_tool_result_ignored() {
    // Tool result content with promise-like text should not trigger
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let user_line = serde_json::json!({
        "message": {"role": "user", "content": "<promise>fake</promise>"}
    });
    std::fs::write(&path, serde_json::to_string(&user_line).unwrap() + "\n").unwrap();
    assert_eq!(detect_intent_full(&path), Intent::None);
}

#[test]
fn watching_intent_stale_transcript_not_honored() {
    // AC3-EDGE: a watching tag 2 entries back with a tag-less newest entry
    // must NOT resurrect as Watching (payload-or-newest-entry rule).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let mut content = String::new();
    for text in [
        "<watching reason=\"ci\" pr=\"3\">", // oldest
        "still going",
        "moving on to unrelated work", // newest
    ] {
        let line = serde_json::json!({"message": {"role": "assistant", "content": text}});
        content.push_str(&serde_json::to_string(&line).unwrap());
        content.push('\n');
    }
    std::fs::write(&path, content).unwrap();
    assert_eq!(detect_intent_full(&path), Intent::None);
}

#[test]
fn quoted_and_bare_tags_are_prose_not_intent() {
    // Same contract as the distress extractor: a tag quoted in backticks or
    // fenced is a worker talking about the protocol, not emitting it, and a
    // bare attribute-less tag is a mention. Two exceptions are machine
    // signal: the engine emits a bare aborted tag when a kill criterion
    // fires with no reason, and the delivery evaluator accepts a bare
    // promise opener as the promise itself.
    use super::super::intent::detect_intent_from_text;
    assert_eq!(
        detect_intent_from_text("not `<watching>` yet"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("see `<promise>fake</promise>` in the docs"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("```text\n<promise>x</promise>\n```"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("bare <watching> mention"),
        Intent::None
    );
    // The bare aborted tag is machine signal, not prose: the engine emits it
    // deliberately when a kill criterion fires with no reason.
    assert!(matches!(
        detect_intent_from_text("<aborted>"),
        Intent::Aborted { .. }
    ));
    // An attributed tag inside a code span is the emission, not a quotation:
    // every observed idle refusal of this shape traces to the tag wrapped in
    // backticks, the form the teaching text itself displays. A refused
    // emission re-invokes the model on every stop; a misread quotation idles
    // one bounded window and self-heals, so the attributed tag reads as
    // signal wherever it sits. Only an attribute-less tag stays prose.
    assert_eq!(
        detect_intent_from_text("see ``<watching reason=\"ci\" timeout=\"15m\">`` here"),
        Intent::Watching {
            reason: "ci".into(),
            pr: None,
            timeout: Some("15m".into()),
            task_id: None,
        }
    );
    assert_eq!(
        detect_intent_from_text("bare <watching> then <watching reason=\"ci\" timeout=\"15m\">"),
        Intent::Watching {
            reason: "ci".into(),
            pr: None,
            timeout: Some("15m".into()),
            task_id: None,
        }
    );
    assert_eq!(
        detect_intent_from_text("I still owe the <promise>MISSION COMPLETE summary"),
        Intent::Promise
    );
    assert!(matches!(
        detect_intent_from_text("<watching reason=\"ci\" timeout=\"15m\">"),
        Intent::Watching { .. }
    ));
    assert!(matches!(
        detect_intent_from_text("<aborted reason=\"operator stop\">"),
        Intent::Aborted { .. }
    ));
    assert_eq!(
        detect_intent_from_text("<promise>MISSION COMPLETE: shipped</promise>"),
        Intent::Promise
    );
}

#[test]
fn declared_help_hold_needs_a_real_distress() {
    // The hold reads the real extractor, not a raw substring: the specimen
    // prose that once mailed the lead an unclassified help holds nothing.
    use crate::loopcheck::declared_recovery_hold;
    assert_eq!(
        declared_recovery_hold("Not `<help>`-worthy: this is the wait class with a live watcher"),
        None
    );
    assert_eq!(
        declared_recovery_hold(
            "stuck <help reason=\"missing dependency\" evidence=\"plan 4.2\">body</help>"
        ),
        Some("declared_help".into())
    );
}
