//! The typed read the store's derivations make of a graph row. `Node` is the
//! strict model a row is stored as; it cannot serve here, for two measured
//! reasons. Its claim block never parses the row's `locked_by` mirror, and
//! the status recompute derives `in_progress` from exactly that mirror. And
//! `Node::from_json` refuses the partial rows a write carries before its
//! defaults run (a new row's `slug` is still null).
//!
//! So a derivation parses a row once into `RowFacts` and reads typed fields.
//! It writes its derived keys back on the row in place, so the wire bytes
//! stay what they were. A missing or mistyped key reads as absent, the same
//! answer the string-key reads gave. The key names are the constants in
//! `key`, and the test below holds every one of them inside the vocabulary
//! `Node::from_json` consumes.

use serde_json::{Map, Value};

/// The graph.json key names the derivations read.
pub mod key {
    pub const ID: &str = "id";
    pub const SLUG: &str = "slug";
    pub const TITLE: &str = "title";
    pub const STATUS: &str = "status";
    pub const PARENT: &str = "parent";
    pub const BLOCKED_BY: &str = "blocked_by";
    pub const COMPLETED_AT: &str = "completed_at";
    pub const SUPERSEDED_BY: &str = "superseded_by";
    pub const SUPERSESSION: &str = "supersession";
    pub const DEFERRED_AT: &str = "deferred_at";
    pub const DEFERRED_KIND: &str = "deferred_kind";
    pub const REOPENED_AT: &str = "reopened_at";
    pub const LOCKED_BY: &str = "locked_by";
    pub const LOCKED_AT: &str = "locked_at";
    pub const SESSION_ID: &str = "session_id";
    pub const PR_NUMBER: &str = "pr_number";
    pub const ADDITIONAL_PRS: &str = "additional_prs";
    pub const PLAN_PATH: &str = "plan_path";
    pub const CREATED_AT: &str = "created_at";
    pub const SESSIONS: &str = "sessions";
    pub const DETAILS: &str = "details";
    pub const TYPE: &str = "type";
    pub const CWD: &str = "cwd";
    pub const VISION_PATH: &str = "vision_path";
    pub const PR_URL: &str = "pr_url";
    pub const MERGE_STATUS: &str = "merge_status";
    /// Legacy and stamp keys the model carries in `extras`, verbatim.
    pub const CLAIMED_AT: &str = "claimed_at";
    pub const PARENTED_AT: &str = "parented_at";

    /// Keys inside one `sessions[]` item.
    pub const PHASE: &str = "phase";
    pub const HARNESS: &str = "harness";
    pub const STARTED_AT: &str = "started_at";
    pub const ENDED_AT: &str = "ended_at";
    /// Key inside one `additional_prs[]` item.
    pub const NUMBER: &str = "number";
    /// Key inside the `supersession` object.
    pub const VERIFIED_AT: &str = "verified_at";
}

/// One key's raw slot: absent, null, or a value. Absent and null differ
/// only where a reader asks `is_absent` (the legacy lock backfill does).
#[derive(Clone, Copy, Debug, Default)]
pub struct Slot<'a>(Option<&'a Value>);

impl<'a> Slot<'a> {
    fn of(object: &'a Map<String, Value>, name: &str) -> Self {
        Self(object.get(name))
    }

    pub fn is_absent(self) -> bool {
        self.0.is_none()
    }

    /// Present and not null.
    pub fn is_set(self) -> bool {
        self.0.is_some_and(|value| !value.is_null())
    }

    pub fn as_str(self) -> Option<&'a str> {
        self.0.and_then(Value::as_str)
    }

    pub fn as_i64(self) -> Option<i64> {
        self.0.and_then(Value::as_i64)
    }

    pub fn as_array(self) -> Option<&'a Vec<Value>> {
        self.0.and_then(Value::as_array)
    }

    pub fn as_object(self) -> Option<&'a Map<String, Value>> {
        self.0.and_then(Value::as_object)
    }

    /// The string items of a list, non-strings skipped.
    pub fn str_items(self) -> impl Iterator<Item = &'a str> {
        self.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
    }

    pub fn value(self) -> Option<&'a Value> {
        self.0
    }
}

/// The row fields a derivation decides on. A view, not a copy: each
/// accessor is one map lookup, so a pass pays only for the fields it reads.
#[derive(Clone, Copy, Debug)]
pub struct RowFacts<'a> {
    object: &'a Map<String, Value>,
}

macro_rules! row_slots {
    ($($field:ident => $key:ident,)*) => {
        impl<'a> RowFacts<'a> {
            $(
                pub fn $field(self) -> Slot<'a> {
                    Slot::of(self.object, key::$key)
                }
            )*
        }
    };
}

row_slots! {
    id => ID,
    slug => SLUG,
    title => TITLE,
    status => STATUS,
    parent => PARENT,
    blocked_by => BLOCKED_BY,
    completed_at => COMPLETED_AT,
    superseded_by => SUPERSEDED_BY,
    supersession => SUPERSESSION,
    deferred_at => DEFERRED_AT,
    deferred_kind => DEFERRED_KIND,
    reopened_at => REOPENED_AT,
    locked_by => LOCKED_BY,
    locked_at => LOCKED_AT,
    claimed_at => CLAIMED_AT,
    session_id => SESSION_ID,
    pr_number => PR_NUMBER,
    additional_prs => ADDITIONAL_PRS,
    plan_path => PLAN_PATH,
    created_at => CREATED_AT,
    parented_at => PARENTED_AT,
    sessions => SESSIONS,
    details => DETAILS,
    kind => TYPE,
    cwd => CWD,
    vision_path => VISION_PATH,
    pr_url => PR_URL,
    merge_status => MERGE_STATUS,
}

impl<'a> RowFacts<'a> {
    /// `None` for a row that is not an object.
    pub fn of(row: &'a Value) -> Option<Self> {
        row.as_object().map(|object| Self { object })
    }

    /// The row's string id, the one fact nearly every reader needs.
    pub fn id_of(row: &'a Value) -> Option<&'a str> {
        row.as_object()
            .and_then(|object| object.get(key::ID))
            .and_then(Value::as_str)
    }

    /// The `sessions[]` items that are objects.
    pub fn session_rows(self) -> impl Iterator<Item = SessionFacts<'a>> {
        self.sessions()
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(SessionFacts::of)
    }
}

/// One `sessions[]` item.
#[derive(Clone, Copy, Debug)]
pub struct SessionFacts<'a> {
    pub phase: Slot<'a>,
    pub harness: Slot<'a>,
    pub session_id: Slot<'a>,
    pub started_at: Slot<'a>,
    pub ended_at: Slot<'a>,
}

impl<'a> SessionFacts<'a> {
    pub fn of(row: &'a Value) -> Option<Self> {
        let object = row.as_object()?;
        let slot = |name| Slot::of(object, name);
        Some(Self {
            phase: slot(key::PHASE),
            harness: slot(key::HARNESS),
            session_id: slot(key::SESSION_ID),
            started_at: slot(key::STARTED_AT),
            ended_at: slot(key::ENDED_AT),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::key;

    /// Every top-level key a derivation reads is one the model consumes or
    /// one it carries verbatim by name. A misspelled constant fails here.
    #[test]
    fn row_keys_stay_inside_the_model_vocabulary() {
        let carried_verbatim = [key::CLAIMED_AT, key::PARENTED_AT];
        for name in [
            key::ID,
            key::SLUG,
            key::TITLE,
            key::STATUS,
            key::PARENT,
            key::BLOCKED_BY,
            key::COMPLETED_AT,
            key::SUPERSEDED_BY,
            key::SUPERSESSION,
            key::DEFERRED_AT,
            key::DEFERRED_KIND,
            key::REOPENED_AT,
            key::LOCKED_BY,
            key::LOCKED_AT,
            key::SESSION_ID,
            key::PR_NUMBER,
            key::ADDITIONAL_PRS,
            key::PLAN_PATH,
            key::CREATED_AT,
            key::SESSIONS,
            key::DETAILS,
            key::TYPE,
            key::CWD,
            key::VISION_PATH,
            key::PR_URL,
            key::MERGE_STATUS,
        ] {
            assert!(
                crate::backlog::model::is_known_key(name),
                "{name} is not a key Node::from_json consumes"
            );
        }
        for name in carried_verbatim {
            assert!(!crate::backlog::model::is_known_key(name), "{name}");
        }
    }
}
