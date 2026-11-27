//! Where a decision's judge tag lives: a TOML file of one `behavioural` or
//! `state` per decision id, its path in `fnod.judge.tags_file` in fno
//! config. The shadow judge asks fnod only about a behavioural law, one
//! whose obedience the reply text alone can show. A state law (merge, push,
//! install) needs a world read that a transcript judge cannot make. An
//! untagged law is never judged.

use std::path::Path;
use toml::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    Behavioural,
    State,
    Untagged,
}

/// The tag table, read once per Stop.
pub(crate) struct Tags(Value);

impl Tags {
    /// The file `fnod.judge.tags_file` names. The table is a file of its
    /// own, so a few hundred laws add no parse to any other config read. No
    /// key, an unreadable file or a damaged line tags nothing, which fails
    /// toward never judged.
    pub(crate) fn read(cwd: &Path) -> Self {
        let named = crate::agents_config::config_value_deep(cwd, &["fnod", "judge", "tags_file"])
            .and_then(|value| value.as_str().map(str::to_string));
        Tags(
            named
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| toml::from_str::<Value>(&text).ok())
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
        // No file is named: nothing is judged.
        let law = "d-03c304bb";
        assert_eq!(Tags::read(dir.path()).get(law), Tag::Untagged);
        let file = dir.path().join("judge-tags.toml");
        std::fs::write(
            &file,
            "d-03c304bb = \"behavioural\"\nd-2d9ee096 = \"state\"\nd-bad00001 = \"behaviour\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".fno/config.toml"),
            format!("[fnod.judge]\ntags_file = \"{}\"\n", file.display()),
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
        // A damaged file tags nothing, which fails toward never judged.
        std::fs::write(&file, "d-03c304bb = \n").unwrap();
        assert_eq!(Tags::read(dir.path()).get(law), Tag::Untagged);
    }
}
