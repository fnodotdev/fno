//! Prints the mux wire JSON Schema (draft 2020-12) to stdout. The checked-in
//! copy is `schema/mux-wire.schema.json`; CI regenerates it and fails on a
//! diff, so the file cannot drift from the types.
//!
//! ```text
//! cargo run --features schema --example gen-schema > schema/mux-wire.schema.json
//! ```

use fno_proto::{ClientMsg, ServerMsg, MIN_COMPAT_PROTO, PROTO_VERSION};
use schemars::generate::SchemaSettings;

fn main() {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let client = generator.subschema_for::<ClientMsg>();
    let server = generator.subschema_for::<ServerMsg>();
    let defs = generator.take_definitions(true);
    let doc = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "fno mux wire",
        "proto_version": PROTO_VERSION,
        "min_compat_proto": MIN_COMPAT_PROTO,
        "roots": { "ClientMsg": client, "ServerMsg": server },
        "$defs": defs,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&doc).expect("schema serializes")
    );
}
