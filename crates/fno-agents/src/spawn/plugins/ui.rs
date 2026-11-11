//! The UI components a pack declares: mods and layouts.
//!
//! A mod is one panel the mux draws. It declares exactly one render path: a
//! `run` argv (a command mod, which runs the pack's own program), a `feed`
//! (an `fno` read argv whose JSON rows `fno config plugins render` draws as
//! a table, so no pack program runs), or `native` (the id of a view the mux
//! already paints). A layout is a whole-window tree in the mux's anchored
//! layout shape, whose slots bind the anchor pane, a shell, or a mod of the
//! same pack.
//!
//! These parse through typed serde structs that refuse unknown fields, the
//! same strictness the rest of the manifest holds by hand.

use serde::{Deserialize, Serialize};

/// The binaries a feed may call: a feed reads fno, it never runs pack code.
pub const FEED_PROGRAMS: [&str; 2] = ["fno", "fno-agents"];

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    #[default]
    Split,
    Popup,
    Sideline,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Column {
    /// A JSON pointer into one row, such as `/verdict`.
    pub key: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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
}

/// How a mod draws, once the manifest shape is known to hold one path.
pub enum Render<'a> {
    Run(&'a [String]),
    Feed(&'a Feed),
    Native(&'a str),
}

impl Mod {
    /// The one declared render path, or a message naming what is wrong.
    pub fn render(&self) -> Option<Render<'_>> {
        match (&self.run, &self.feed, &self.native) {
            (Some(argv), None, None) if !argv.is_empty() => Some(Render::Run(argv)),
            (None, Some(feed), None) => Some(Render::Feed(feed)),
            (None, None, Some(view)) if !view.is_empty() => Some(Render::Native(view)),
            _ => None,
        }
    }

    /// A command mod runs the pack's own program; it starts off and needs
    /// consent before it runs.
    pub fn runs_code(&self) -> bool {
        self.run.is_some()
    }
}

/// Mirrors the mux's `Axis`, spelled as its serde form.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Tree {
    Slot(String),
    Split { axis: Axis, children: Vec<Child> },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Child {
    pub weight: f32,
    pub tree: Tree,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Anchor,
    Shell,
    /// A mod id declared by the same pack.
    Mod(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub name: String,
    pub binding: Binding,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
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

/// One optional field of a hand-checked object, parsed through its serde type.
pub(crate) fn field<T: serde::de::DeserializeOwned>(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> serde_json::Result<Option<T>> {
    obj.get(key)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
}
