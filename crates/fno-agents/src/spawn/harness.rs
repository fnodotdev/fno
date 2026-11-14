//! The harness an agent runs on, as one closed vocabulary.
//!
//! Every reader that branches on a harness word parses it here once and
//! matches on the variant. A word footnote does not branch on (gemini, pi,
//! a future host, the empty string) lands in [`Harness::Other`] with its
//! spelling kept, so the type stays total over any input. Parsing is exact:
//! the wire words are lowercase, and `"Claude"` is not `claude`.
//!
//! The full supported roster stays `provider::KNOWN_HARNESSES` in
//! fno-agents. This enum names only the harnesses code branches on. A table
//! keyed over the whole roster (gemini, pi, grok and the rest beside these
//! four) stays a string `match` until its words earn variants here. An
//! executable-name check (`argv[0]`, a binary basename) is a different axis
//! and also stays a string compare. The fno crate compiles a generated copy
//! of this file, so both crates share it.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::hash::{Hash, Hasher};

/// Build values with [`Harness::parse`]. Equality and hashing read the wire
/// word, so a hand-built `Other("claude")` still equals `Claude`; a `match`
/// reads the variant and cannot, which is why `parse` is the constructor.
#[derive(Debug, Clone)]
pub enum Harness {
    Claude,
    Codex,
    Opencode,
    Agy,
    Other(String),
}

impl Harness {
    pub fn parse(word: &str) -> Self {
        match word {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "opencode" => Self::Opencode,
            "agy" => Self::Agy,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Agy => "agy",
            Self::Other(word) => word,
        }
    }
}

impl PartialEq for Harness {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Harness {}

impl Hash for Harness {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

/// `word.as_harness()` parses at a compare site. Method auto-deref takes
/// `&str`, `String` and `&String` alike, where a free `parse(&x)` call would
/// need a borrow on some of them and not on others.
pub trait AsHarness {
    fn as_harness(&self) -> Harness;
}

impl AsHarness for str {
    fn as_harness(&self) -> Harness {
        Harness::parse(self)
    }
}

impl From<&str> for Harness {
    fn from(word: &str) -> Self {
        Self::parse(word)
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Harness {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Harness {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(Self::parse(&word))
    }
}

#[cfg(test)]
mod tests {
    use super::Harness;

    #[test]
    fn every_word_round_trips_and_unknown_words_keep_their_spelling() {
        for word in ["claude", "codex", "opencode", "agy", "gemini", "", "Claude"] {
            let harness = Harness::parse(word);
            assert_eq!(harness.as_str(), word);
            let wire = serde_json::to_string(&harness).unwrap();
            assert_eq!(wire, serde_json::to_string(word).unwrap());
            assert_eq!(serde_json::from_str::<Harness>(&wire).unwrap(), harness);
        }
        assert_eq!(Harness::parse("claude"), Harness::Claude);
        assert_ne!(Harness::parse("Claude"), Harness::Claude);
        assert_eq!(Harness::Other("claude".into()), Harness::Claude);
    }
}
