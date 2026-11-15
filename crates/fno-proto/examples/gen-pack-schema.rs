//! Prints the pack UI JSON Schema (draft 2020-12) to stdout. The checked-in
//! copy is `schema/pack-ui.schema.json`; CI regenerates it and fails on a
//! diff, so the file cannot drift from the types. This is the schema a pack
//! author who writes no Rust validates their manifest's `mods`, `layouts`,
//! and view files against.
//!
//! ```text
//! cargo run --features schema --example gen-pack-schema > schema/pack-ui.schema.json
//! ```

use fno_proto::pack::{Layout, Mod, View, PACK_UI_SCHEMA_VERSION};
use schemars::generate::SchemaSettings;

fn main() {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let mods = generator.subschema_for::<Mod>();
    let layout = generator.subschema_for::<Layout>();
    let view = generator.subschema_for::<View>();
    let defs = generator.take_definitions(true);
    let doc = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "fno pack UI",
        "description": "The mods and layouts a pack's plugin.yaml declares. While pack_ui_schema_version is 1, changes are additive only; a removal or rename raises it.",
        "pack_ui_schema_version": PACK_UI_SCHEMA_VERSION,
        "roots": { "Mod": mods, "Layout": layout, "View": view },
        "$defs": defs,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&doc).expect("schema serializes")
    );
}
