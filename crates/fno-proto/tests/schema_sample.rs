//! The checked-in schema accepts what `encode` writes. Runs only with the
//! `schema` feature, which CI turns on for the drift step.
#![cfg(feature = "schema")]

use fno_proto::{encode, ClientMsg, Command, Dir};

#[test]
fn encoded_command_validates_against_the_published_schema() {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../schema/mux-wire.schema.json"))
            .expect("schema parses");
    let mut root = doc["roots"]["ClientMsg"].clone();
    root["$defs"] = doc["$defs"].clone();
    let validator = jsonschema::validator_for(&root).expect("schema compiles");

    let bytes = encode(&ClientMsg::Command(Command::SplitDir(Dir::Right))).expect("encodes");
    let instance: serde_json::Value = serde_json::from_slice(&bytes[4..]).expect("body is JSON");
    assert!(validator.is_valid(&instance), "{instance}");
}
