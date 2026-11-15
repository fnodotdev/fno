//! The UI components a pack declares: mods, layouts, and views.
//!
//! A mod is one panel a host draws. It declares exactly one render path: a
//! `run` argv (a command mod, which runs the pack's own program), a `feed`
//! (an `fno` read argv whose JSON rows `fno config plugins open` draws as
//! a table, so no pack program runs), `native` (the id of a view the host
//! already paints), or `view` (a pack-relative path to a JSON file of
//! blocks the desktop app draws; nothing runs). A layout is a whole-window
//! tree in the mux's anchored layout shape, whose slots bind the anchor
//! pane, a shell, or a mod of the same pack.
//!
//! These parse through typed serde structs that refuse unknown fields, the
//! same strictness the rest of the manifest holds by hand.
//!
//! Two facts make their serde shape load-bearing. The pack digest hashes
//! it, so a pack without mods keeps its digest and an installed pack never
//! goes stale on a type move. And the published schema
//! (`schema/pack-ui.schema.json`) regenerates from these types through the
//! `gen-pack-schema` example, so a non-Rust author reads the same contract
//! the binary parses.

use serde::{Deserialize, Serialize};

/// The published pack-UI schema's version, emitted as `pack_ui_schema_version` in `schema/pack-ui.schema.json`. The promise `1` carries: while
/// it is 1, changes to these types' serde shape are additive only (a new
/// optional field, a new block or variant). A removal or a rename raises
/// the version, because a pack written against the old file stops parsing.
pub const PACK_UI_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Place {
    #[default]
    Split,
    Popup,
    Sideline,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Column {
    /// A JSON pointer into one row, such as `/verdict`.
    pub key: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub argv: Vec<String>,
    /// A JSON pointer to the row array in the feed's output; empty means the
    /// output itself is the array.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rows: String,
    pub columns: Vec<Column>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Mod {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub preview: String,
    pub recipe: String,
    #[serde(default)]
    pub place: Place,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed: Option<Feed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    /// A pack-relative path to the view JSON the desktop app draws. Like
    /// `preview` and `recipe`, it joins the pack digest only when declared,
    /// so packs written before views existed keep their digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

/// How a mod draws, once the manifest shape is known to hold one path.
/// Computed, never serialized: this rides no pack file, so it carries no
/// schema.
pub enum Render<'a> {
    Run(&'a [String]),
    Feed(&'a Feed),
    Native(&'a str),
    View(&'a str),
}

impl Mod {
    /// The one declared render path, or a message naming what is wrong.
    pub fn render(&self) -> Option<Render<'_>> {
        match (&self.run, &self.feed, &self.native, &self.view) {
            (Some(argv), None, None, None) if !argv.is_empty() => Some(Render::Run(argv)),
            (None, Some(feed), None, None) => Some(Render::Feed(feed)),
            (None, None, Some(view), None) if !view.is_empty() => Some(Render::Native(view)),
            (None, None, None, Some(path)) if !path.is_empty() => Some(Render::View(path)),
            _ => None,
        }
    }

    /// A command mod runs the pack's own program; it starts off and needs
    /// consent before it runs. A view mod is data and never runs anything.
    pub fn runs_code(&self) -> bool {
        self.run.is_some()
    }
}

/// One named field of a [`Block::KeyValue`] row.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub key: String,
    pub value: String,
}

/// One piece of a declared view. Tagged by `kind` on the wire; an unknown
/// kind refuses to parse and names itself in the error.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Heading {
        text: String,
    },
    Text {
        text: String,
    },
    /// A bullet list. With `live`, each row of the read is one item, the
    /// text at the `key` pointer; the declared `items` show when the read
    /// fails.
    List {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live: Option<Live>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        key: String,
    },
    /// A table the host draws from the declared columns. Each column's
    /// `key` is a JSON pointer to a field of the row, the same spelling a
    /// feed's columns use; a declared row is a flat map of strings keyed by
    /// the pointer's one name. With `live`, the rows come from the read.
    Table {
        columns: Vec<Column>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rows: Vec<std::collections::BTreeMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live: Option<Live>,
    },
    KeyValue {
        entries: Vec<Entry>,
    },
}

/// The reads a feed or a view's live block may run. A feed mod starts on and does not count as
/// code, so its argv must be a known read: an allowlist of command prefixes,
/// never "any fno command", which would let a data-only mod write state on
/// every redraw.
pub const FEED_READS: [&[&str]; 8] = [
    &["fno", "backlog", "board"],
    &["fno", "backlog", "list"],
    &["fno", "backlog", "get"],
    &["fno", "backlog", "find"],
    &["fno", "do", "pr", "status"],
    &["fno", "config", "plugins", "ls"],
    &["fno", "config", "plugins", "gallery"],
    &["fno-agents", "review-summary"],
];

/// Flags that turn a listed read into a write.
const FEED_WRITE_FLAGS: [&str; 2] = ["--output", "--fix"];

/// The argv starts with a listed read and carries no flag that writes.
pub fn feed_is_read(argv: &[String]) -> bool {
    let listed = FEED_READS
        .iter()
        .any(|read| argv.len() >= read.len() && argv.iter().zip(read.iter()).all(|(a, r)| a == r));
    listed
        && !argv.iter().any(|a| {
            FEED_WRITE_FLAGS
                .iter()
                .any(|f| a == f || a.starts_with(&format!("{f}=")))
        })
}

/// A block's live rows: one `fno` read argv, held to the same known reads a
/// feed mod may name, and a JSON pointer to the row array in its output
/// (empty means the output itself is the array). The host runs the read
/// when it shows the view; nothing the pack ships runs.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Live {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rows: String,
}

/// One cell of a live row: the text at `pointer`, a string as is, anything
/// else as its JSON, a missing field as empty.
fn cell(row: &serde_json::Value, pointer: &str) -> String {
    match row.pointer(pointer) {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// The view a [`Mod::view`] path points at: the blocks a host draws, in
/// order. A view is data all the way down; declaring one never runs code.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct View {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

impl View {
    /// Fill every live block's rows from its read. `read` runs one argv and
    /// answers its JSON output, or `None` when the read failed. A failed
    /// read keeps the block's declared rows; its argv comes back in the
    /// list so the host can say which read failed.
    pub fn fill_live(
        &mut self,
        read: &dyn Fn(&[String]) -> Option<serde_json::Value>,
    ) -> Vec<String> {
        let mut failed = Vec::new();
        for block in &mut self.blocks {
            let (live, rows_out) = match block {
                Block::Table {
                    live: Some(live), ..
                }
                | Block::List {
                    live: Some(live), ..
                } => (live.clone(), read(&live.argv)),
                _ => continue,
            };
            let rows = rows_out.as_ref().and_then(|out| {
                if live.rows.is_empty() {
                    out.as_array()
                } else {
                    out.pointer(&live.rows)
                        .and_then(serde_json::Value::as_array)
                }
            });
            let Some(rows) = rows else {
                failed.push(live.argv.join(" "));
                continue;
            };
            match block {
                Block::Table {
                    columns, rows: out, ..
                } => {
                    *out = rows
                        .iter()
                        .map(|row| {
                            columns
                                .iter()
                                .map(|c| {
                                    (c.key.trim_start_matches('/').to_string(), cell(row, &c.key))
                                })
                                .collect()
                        })
                        .collect();
                }
                Block::List { items, key, .. } => {
                    *items = rows.iter().map(|row| cell(row, key)).collect();
                }
                _ => {}
            }
        }
        failed
    }
}

/// Mirrors the mux's `Axis`, spelled as its serde form.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Tree {
    Slot(String),
    Split { axis: Axis, children: Vec<Child> },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Child {
    pub weight: f32,
    pub tree: Tree,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Anchor,
    Shell,
    /// A mod id declared by the same pack.
    Mod(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub name: String,
    pub binding: Binding,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub preview: String,
    pub recipe: String,
    pub tree: Tree,
    pub slots: Vec<Slot>,
}

impl Tree {
    pub fn leaves<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Tree::Slot(name) => out.push(name),
            Tree::Split { children, .. } => {
                for child in children {
                    child.tree.leaves(out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mod whose every field but the render paths is set.
    fn base_mod() -> serde_json::Value {
        serde_json::json!({
            "id": "m", "title": "T", "summary": "s",
            "preview": "p.png", "recipe": "R.md"
        })
    }

    fn mod_with(key: &str, value: serde_json::Value) -> Mod {
        let mut m = base_mod();
        m.as_object_mut().unwrap().insert(key.into(), value);
        serde_json::from_value::<Mod>(m).expect("a one-path mod parses")
    }

    #[test]
    fn a_mod_with_one_declared_path_renders_that_path() {
        assert!(matches!(
            mod_with("view", serde_json::json!("m/v.json")).render(),
            Some(Render::View(_))
        ));
        assert!(
            !mod_with("view", serde_json::json!("m/v.json")).runs_code(),
            "a view mod is data and never runs code"
        );
    }

    #[test]
    fn a_mod_declaring_two_paths_renders_nothing() {
        let mut two = base_mod();
        let obj = two.as_object_mut().unwrap();
        obj.insert("run".into(), serde_json::json!(["./x"]));
        obj.insert("view".into(), serde_json::json!("m/v.json"));
        let m: Mod = serde_json::from_value(two).expect("parse admits the shapes");
        assert!(m.render().is_none(), "two declared paths render nothing");
    }

    #[test]
    fn a_view_with_every_block_kind_parses_and_an_unknown_kind_names_itself() {
        let view: View = serde_json::from_str(
            r#"{
                "title": "T",
                "blocks": [
                    {"kind": "heading", "text": "h"},
                    {"kind": "text", "text": "t"},
                    {"kind": "list", "items": ["a"]},
                    {"kind": "table", "columns": [{"key": "/k", "title": "K"}],
                     "rows": [{"k": "v"}]},
                    {"kind": "key_value", "entries": [{"key": "a", "value": "b"}]}
                ]
            }"#,
        )
        .expect("every block kind parses");
        assert_eq!(view.blocks.len(), 5);
        let err = serde_json::from_str::<View>(r#"{"blocks": [{"kind": "chart", "text": "x"}]}"#)
            .expect_err("an unknown block kind refuses");
        assert!(err.to_string().contains("chart"), "{err}");
    }

    #[test]
    fn live_blocks_fill_from_their_read_and_keep_declared_rows_on_failure() {
        let mut view: View = serde_json::from_str(
            r#"{"blocks": [
                {"kind": "table", "columns": [{"key": "/id", "title": "id"}],
                 "rows": [{"id": "declared"}], "live": {"argv": ["fno", "backlog", "list"]}},
                {"kind": "list", "key": "/title",
                 "live": {"argv": ["fno", "backlog", "board"], "rows": "/rows"}}
            ]}"#,
        )
        .expect("live blocks parse");
        let failed = view.fill_live(&|argv| match argv.get(2).map(String::as_str) {
            Some("list") => Some(serde_json::json!([{"id": "x-1"}, {"id": 7}])),
            _ => None,
        });
        assert_eq!(failed, ["fno backlog board"]);
        let Block::Table { rows, .. } = &view.blocks[0] else {
            panic!("table first")
        };
        assert_eq!(rows[0]["id"], "x-1");
        assert_eq!(rows[1]["id"], "7");
        let Block::List { items, .. } = &view.blocks[1] else {
            panic!("list second")
        };
        assert!(items.is_empty(), "a failed read keeps the declared items");
    }
}
