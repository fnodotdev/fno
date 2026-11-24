//! Where a decision's judge tag lives: `[fnod.judge.tags]` in fno config,
//! one `behavioural` or `state` per decision id. The shadow judge asks fnod
//! only about a behavioural law, one whose obedience the reply text alone
//! can show. A state law (merge, push, install) needs a world read that a
//! transcript judge cannot make. An untagged law is never judged.

use std::path::Path;
use toml::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    Behavioural,
    State,
    Untagged,
}

impl Tag {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Tag::Behavioural => "behavioural",
            Tag::State => "state",
            Tag::Untagged => "untagged",
        }
    }
}

/// The tag table, read once per Stop.
#[derive(Debug, Default)]
pub(crate) struct Tags(Value);

impl Tags {
    /// The `[fnod.judge.tags]` table, merged across the config files. A
    /// missing table tags nothing.
    pub(crate) fn read(cwd: &Path) -> Self {
        Tags(
            crate::agents_config::config_value_deep(cwd, &["fnod", "judge", "tags"])
                .unwrap_or_else(|| Value::Table(Default::default())),
        )
    }

    /// The tag of one decision. A value outside the two words reads
    /// untagged, the same as no entry.
    pub(crate) fn get(&self, decision_id: &str) -> Tag {
        match self.0.get(decision_id).and_then(Value::as_str) {
            Some("behavioural") => Tag::Behavioural,
            Some("state") => Tag::State,
            _ => Tag::Untagged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tagged law reads its tag. A law with no entry, or a value outside
    /// the two words, reads untagged, so nothing is judged by default.
    #[test]
    fn a_law_reads_its_tag_and_nothing_else_is_judged() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        let absent = "d-absent01";
        assert_eq!(Tags::read(dir.path()).get(absent), Tag::Untagged);
        std::fs::write(
            dir.path().join(".fno/config.toml"),
            "[fnod.judge.tags]\nd-03c304bb = \"behavioural\"\nd-2d9ee096 = \"state\"\nd-bad00001 = \"behaviour\"\n",
        )
        .unwrap();
        let tags = Tags::read(dir.path());
        let cases = [
            ("d-03c304bb", Tag::Behavioural),
            ("d-2d9ee096", Tag::State),
            ("d-bad00001", Tag::Untagged),
            ("d-absent01", Tag::Untagged),
        ];
        for (id, tag) in cases {
            assert_eq!(tags.get(id), tag, "{id}");
        }
        assert_eq!(Tag::State.as_str(), "state");
    }
}
