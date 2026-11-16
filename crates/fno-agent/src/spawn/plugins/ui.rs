//! The UI components a pack declares: mods and layouts.
//!
//! A mod is one panel the mux draws. It declares exactly one render path: a
//! `run` argv (a command mod, which runs the pack's own program), a `feed`
//! (an `fno` read argv whose JSON rows `fno config plugins open` draws as
//! a table, so no pack program runs), or `native` (the id of a view the mux
//! already paints). A layout is a whole-window tree in the mux's anchored
//! layout shape, whose slots bind the anchor pane, a shell, or a mod of the
//! same pack.
//!
//! The declaration types themselves live in fno-proto (`fno_proto::pack`),
//! where the published pack-UI schema regenerates from them and where their
//! serde shape stays one spelling for the pack digest. This module
//! re-exports the same names, so every call site keeps its spelling, and
//! keeps the behavior: the feed allowlist, the hand-checked `field`
//! reader, and `shell_words`.

pub use fno_proto::pack::{
    Axis, Binding, Child, Column, Feed, Layout, Mod, Place, Render, Slot, Tree,
};

/// The reads a feed may run. A feed mod starts on and does not count as
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
    &["fno-agent", "review-summary"],
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

/// One optional field of a hand-checked object, parsed through its serde type.
pub(crate) fn field<T: serde::de::DeserializeOwned>(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> serde_json::Result<Option<T>> {
    obj.get(key)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
}

/// An argv as one line a person can paste into a shell: plain words stay
/// bare, anything else goes in single quotes.
pub fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
