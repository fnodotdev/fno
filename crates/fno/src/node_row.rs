//! One graph row, typed, at the crate's row boundary.
//!
//! Rows reach this crate over the keeper socket (`store_client::rows`), a
//! total fold: a legacy row the strict fno-agents model would reject still
//! rides through. So the parse here never refuses a row. Each known key is
//! read into its typed field when its JSON type matches. A key with no typed
//! field, a key whose value is off-type, and a null-valued key all stay in
//! `extra`, verbatim. Serializing a row gives back the same JSON object, so
//! the rows this crate ships back to the keeper (`board_facts`, `flow`)
//! carry every byte they arrived with.
//!
//! Readers past this boundary use the typed fields: a misspelled field name
//! is a compile error, never a silent `None` (`docs/rust-style.md` R1).

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Take `key` out of `m` when its value is non-null and parses as `T`.
/// Anything else stays in `m`, so the row keeps it for the round trip.
fn take_typed<T: DeserializeOwned>(m: &mut Map<String, Value>, key: &str) -> Option<T> {
    let v = m.get(key).filter(|v| !v.is_null())?;
    let typed = T::deserialize(v).ok()?;
    m.remove(key);
    Some(typed)
}

/// Take a list `key` element by element: each element that parses as `T`
/// is kept, so one off-type element hides only itself. When every element
/// parses the key leaves `m`; otherwise the raw array stays in `m` for the
/// round trip.
fn take_list<T: DeserializeOwned>(m: &mut Map<String, Value>, key: &str) -> Option<Vec<T>> {
    let items = m.get(key)?.as_array()?;
    let typed: Vec<T> = items
        .iter()
        .filter_map(|item| T::deserialize(item).ok())
        .collect();
    if typed.len() == items.len() {
        m.remove(key);
    }
    Some(typed)
}

/// Put a typed field back under its key. `None` writes nothing: the key was
/// absent, null or off-type, and in the last two cases `extra` holds it. A
/// key `extra` still holds is the raw value and wins.
fn put_typed<T: Serialize>(m: &mut Map<String, Value>, key: &str, v: Option<T>) {
    if m.contains_key(key) {
        return;
    }
    if let Some(v) = v {
        if let Ok(v) = serde_json::to_value(v) {
            m.insert(key.to_string(), v);
        }
    }
}

/// The take a field declares (`=> take_list`), else [`take_typed`].
macro_rules! take_field {
    (; $ty:ty, $m:expr, $key:literal) => {
        take_typed::<$ty>($m, $key)
    };
    ($take:ident; $ty:ty, $m:expr, $key:literal) => {
        $take($m, $key)
    };
}

/// A struct whose every field is an optional typed read of one JSON key,
/// plus `extra` for the rest. The `From` pair is the whole parse and the
/// whole write, so the two cannot drift apart.
macro_rules! lenient_object {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            $( $(#[$fmeta:meta])* $field:ident : $ty:ty = $key:literal $(=> $take:ident)?, )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
        #[serde(from = "Map<String, Value>", into = "Map<String, Value>")]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: Option<$ty>, )*
            /// Every key no typed field took, verbatim.
            pub extra: Map<String, Value>,
        }

        impl From<Map<String, Value>> for $name {
            fn from(mut m: Map<String, Value>) -> Self {
                $( let $field = take_field!($($take)?; $ty, &mut m, $key); )*
                Self { $( $field, )* extra: m }
            }
        }

        impl From<$name> for Map<String, Value> {
            fn from(row: $name) -> Self {
                let mut m = row.extra;
                $( put_typed(&mut m, $key, row.$field); )*
                m
            }
        }
    };
}

/// The status vocabulary: the words of `fno_agents::backlog::model::Status`
/// (a test in fno-agents holds the two lists equal). `claimed` reads as
/// [`Status::InProgress`], the store's own migration, so it is the one
/// word that does not round-trip. A word outside the vocabulary reads as
/// [`Status::Other`], so the parse stays total.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    Idea,
    Design,
    Ready,
    InProgress,
    InReview,
    Blocked,
    Deferred,
    Done,
    Superseded,
    Other(String),
}

impl Status {
    /// Every named status, in the store's order.
    pub const KNOWN: [Status; 9] = [
        Status::Idea,
        Status::Design,
        Status::Ready,
        Status::InProgress,
        Status::InReview,
        Status::Blocked,
        Status::Deferred,
        Status::Done,
        Status::Superseded,
    ];

    pub fn parse(word: &str) -> Status {
        match word {
            "idea" => Status::Idea,
            "design" => Status::Design,
            "ready" => Status::Ready,
            "in_progress" | "claimed" => Status::InProgress,
            "in_review" => Status::InReview,
            "blocked" => Status::Blocked,
            "deferred" => Status::Deferred,
            "done" => Status::Done,
            "superseded" => Status::Superseded,
            other => Status::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Status::Idea => "idea",
            Status::Design => "design",
            Status::Ready => "ready",
            Status::InProgress => "in_progress",
            Status::InReview => "in_review",
            Status::Blocked => "blocked",
            Status::Deferred => "deferred",
            Status::Done => "done",
            Status::Superseded => "superseded",
            Status::Other(word) => word,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Status::parse(&String::deserialize(d)?))
    }
}

/// A board column. Derived from a row's state, never stored on it.
/// [`KanbanColumn::OffBoard`] is the card a deferred, superseded or
/// roadmap row carries: no lane renders it, and list queries still reach it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum KanbanColumn {
    InProgress,
    Now,
    Next,
    Later,
    Triage,
    Done,
    /// The default too: a card built with no row renders in no lane.
    #[default]
    OffBoard,
}

impl KanbanColumn {
    /// The board's columns, left to right. A test in fno-agents holds this
    /// list equal to the keeper's projection columns.
    pub const ALL: [KanbanColumn; 6] = [
        KanbanColumn::InProgress,
        KanbanColumn::Now,
        KanbanColumn::Next,
        KanbanColumn::Later,
        KanbanColumn::Triage,
        KanbanColumn::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            KanbanColumn::InProgress => "In Progress",
            KanbanColumn::Now => "Now",
            KanbanColumn::Next => "Next",
            KanbanColumn::Later => "Later",
            KanbanColumn::Triage => "Triage",
            KanbanColumn::Done => "Done",
            KanbanColumn::OffBoard => "Off-board",
        }
    }

    /// The column a display name names, off-board included.
    pub fn parse(name: &str) -> Option<KanbanColumn> {
        Self::ALL
            .into_iter()
            .chain([KanbanColumn::OffBoard])
            .find(|c| c.as_str() == name)
    }
}

impl std::fmt::Display for KanbanColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for KanbanColumn {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// An id edge stored as one id or a list of ids. The shape round-trips:
/// a row that stored a string gets a string back.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Ids {
    One(String),
    Many(Vec<String>),
}

impl Ids {
    /// The non-blank ids, in stored order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        let ids: &[String] = match self {
            Ids::One(id) => std::slice::from_ref(id),
            Ids::Many(ids) => ids,
        };
        ids.iter().map(String::as_str).filter(|id| !id.is_empty())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.iter().any(|have| have == id)
    }

    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
}

lenient_object! {
    /// `sessions[].observed_model` as a record: the model the transcript
    /// showed.
    pub struct ObservedModel {
        model: String = "model",
    }
}

/// `sessions[].observed_model`: a record on current rows, the bare model
/// word on older ones.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ObservedModelField {
    Record(ObservedModel),
    Word(String),
}

lenient_object! {
    /// `sessions[].merge_grant` as a record.
    pub struct MergeGrant {
        approved: bool = "approved",
        source: String = "source",
    }
}

/// `sessions[].merge_grant`: a record, or a bare word on older rows.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MergeGrantField {
    Record(MergeGrant),
    Word(String),
}

lenient_object! {
    /// One `sessions[]` record.
    pub struct SessionRecord {
        session_id: String = "session_id",
        phase: String = "phase",
        harness: String = "harness",
        model: String = "model",
        observed_model: ObservedModelField = "observed_model",
        started_at: String = "started_at",
        ended_at: String = "ended_at",
        ended_by: String = "ended_by",
        at: String = "at",
        claimed_at: String = "claimed_at",
        effort: String = "effort",
        merge_grant: MergeGrantField = "merge_grant",
    }
}

lenient_object! {
    /// One `encounters[]` vote.
    pub struct Encounter {
        voter_key: String = "voter_key",
        session_id: String = "session_id",
        voter_kind: String = "voter_kind",
    }
}

lenient_object! {
    /// One `additional_prs[]` binding.
    pub struct PrRef {
        number: u64 = "number",
        url: String = "url",
        merge_status: String = "merge_status",
    }
}

lenient_object! {
    /// One `progress_notes[]` row: a comment, reply or note.
    pub struct ProgressNote {
        ts: String = "ts",
        text: String = "text",
        kind: String = "kind",
        author: String = "author",
        comment_id: String = "comment_id",
        reply_to: String = "reply_to",
        state: String = "state",
        state_ref: String = "state_ref",
        agent_name: String = "agent_name",
        model: String = "model",
        working_node: String = "working_node",
        source_session_id: String = "source_session_id",
    }
}

lenient_object! {
    /// One graph node row.
    pub struct NodeRow {
        id: String = "id",
        slug: String = "slug",
        title: String = "title",
        /// The row's `type` field.
        kind: String = "type",
        status: Status = "status",
        /// The pre-rename status key; read only when `status` is absent.
        legacy_status: Status = "_status",
        priority: String = "priority",
        /// A `Number`, not an `f64`, so an integer rank round-trips as one.
        rank: serde_json::Number = "rank",
        size: String = "size",
        difficulty: String = "difficulty",
        domain: String = "domain",
        project: String = "project",
        parent: String = "parent",
        contained_in: String = "contained_in",
        blocked_by: Ids = "blocked_by",
        related: Ids = "related",
        tags: Vec<String> = "tags",
        details: String = "details",
        current_state: Value = "current_state",
        plan_path: String = "plan_path",
        /// The store-minted short number (`x-5555`), served as `_number`.
        number_label: String = "_number",
        cwd: String = "cwd",
        created_at: String = "created_at",
        completed_at: String = "completed_at",
        touched_at: String = "touched_at",
        queued_at: String = "queued_at",
        sessions: Vec<SessionRecord> = "sessions" => take_list,
        session_id: String = "session_id",
        source_kind: String = "source_kind",
        request_origin: String = "request_origin",
        origin_evidence: String = "origin_evidence",
        source_session_id: String = "source_session_id",
        source_parent_session: String = "source_parent_session",
        source_harness: String = "source_harness",
        source_cwd: String = "source_cwd",
        source_model: String = "source_model",
        source_effort: String = "source_effort",
        spawned_by_session: String = "spawned_by_session",
        spawned_by_harness: String = "spawned_by_harness",
        locked_by_harness: String = "locked_by_harness",
        locked_by_harness_session: String = "locked_by_harness_session",
        encounters: Vec<Encounter> = "encounters" => take_list,
        pr_number: u64 = "pr_number",
        pr_url: String = "pr_url",
        merge_status: String = "merge_status",
        additional_prs: Vec<PrRef> = "additional_prs" => take_list,
        progress_notes: Vec<ProgressNote> = "progress_notes" => take_list,
        decisions: Vec<Value> = "decisions",
        decision_ids: Vec<Value> = "decision_ids",
        cost_usd: serde_json::Number = "cost_usd",
        deferred_kind: String = "deferred_kind",
        superseded_by: String = "superseded_by",
        /// The owner's publish flag; absent reads as public.
        public: bool = "public",
    }
}

/// The id-valued fields that link one row to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Parent,
    ContainedIn,
    BlockedBy,
    Related,
}

impl Edge {
    /// The stored field name, for messages that name the field.
    pub fn field(self) -> &'static str {
        match self {
            Edge::Parent => "parent",
            Edge::ContainedIn => "contained_in",
            Edge::BlockedBy => "blocked_by",
            Edge::Related => "related",
        }
    }
}

/// A non-empty string field, the Python board's truthiness rule: null and
/// `""` both read as unset.
fn filled(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

impl NodeRow {
    /// One row from a JSON value. A non-object value has no fields, so it
    /// reads as an empty row (no id, so no reader renders it).
    pub fn from_value(v: Value) -> NodeRow {
        match v {
            Value::Object(m) => NodeRow::from(m),
            _ => NodeRow::default(),
        }
    }

    /// The row as the JSON object it arrived as.
    pub fn to_value(&self) -> Value {
        Value::Object(Map::from(self.clone()))
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn rank(&self) -> Option<f64> {
        self.rank.as_ref().and_then(serde_json::Number::as_f64)
    }

    pub fn cost_usd(&self) -> Option<f64> {
        self.cost_usd.as_ref().and_then(serde_json::Number::as_f64)
    }

    /// The persisted status, falling back to the pre-rename key.
    pub fn status(&self) -> Option<&Status> {
        self.status.as_ref().or(self.legacy_status.as_ref())
    }

    pub fn is_roadmap(&self) -> bool {
        self.kind.as_deref() == Some("roadmap")
    }

    /// `completed_at` carries a stamp.
    pub fn is_completed(&self) -> bool {
        filled(&self.completed_at).is_some()
    }

    /// `queued_at` carries a stamp.
    pub fn is_queued(&self) -> bool {
        filled(&self.queued_at).is_some()
    }

    /// The parent id, blanks dropped.
    pub fn parent(&self) -> Option<&str> {
        filled(&self.parent)
    }

    /// The containing feature id, blanks dropped.
    pub fn contained_in(&self) -> Option<&str> {
        filled(&self.contained_in)
    }

    /// The project, trimmed, blanks dropped.
    pub fn project(&self) -> Option<&str> {
        self.project
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }

    pub fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }

    pub fn sessions(&self) -> &[SessionRecord] {
        self.sessions.as_deref().unwrap_or_default()
    }

    pub fn encounters(&self) -> &[Encounter] {
        self.encounters.as_deref().unwrap_or_default()
    }

    pub fn additional_prs(&self) -> &[PrRef] {
        self.additional_prs.as_deref().unwrap_or_default()
    }

    pub fn progress_notes(&self) -> &[ProgressNote] {
        self.progress_notes.as_deref().unwrap_or_default()
    }

    /// The `blocked_by` ids, blanks dropped.
    pub fn blocked_by(&self) -> impl Iterator<Item = &str> {
        self.blocked_by.iter().flat_map(Ids::iter)
    }

    /// The `related` ids, blanks dropped.
    pub fn related(&self) -> impl Iterator<Item = &str> {
        self.related.iter().flat_map(Ids::iter)
    }

    /// The ids one edge field names, blanks dropped.
    pub fn edge_ids(&self, edge: Edge) -> Vec<&str> {
        match edge {
            Edge::Parent => self.parent().into_iter().collect(),
            Edge::ContainedIn => self.contained_in().into_iter().collect(),
            Edge::BlockedBy => self.blocked_by().collect(),
            Edge::Related => self.related().collect(),
        }
    }
}

impl SessionRecord {
    /// The transcript's observed model, else the recorded one.
    pub fn model(&self) -> Option<&str> {
        self.observed_record_model().or(self.model.as_deref())
    }

    /// The model an `observed_model` record names.
    pub fn observed_record_model(&self) -> Option<&str> {
        match &self.observed_model {
            Some(ObservedModelField::Record(m)) => m.model.as_deref(),
            _ => None,
        }
    }

    /// The bare model word an older row stores as `observed_model`.
    pub fn observed_model_word(&self) -> Option<&str> {
        match &self.observed_model {
            Some(ObservedModelField::Word(w)) => Some(w),
            _ => None,
        }
    }
}

/// Rows from a keeper or snapshot array. A non-object element has no id and
/// no reader renders it, so it is dropped here.
pub fn rows_from_values(values: Vec<Value>) -> Vec<NodeRow> {
    values
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(m) => Some(NodeRow::from(m)),
            _ => None,
        })
        .collect()
}

/// The rows as the JSON array the keeper reads.
pub fn rows_to_values(rows: &[NodeRow]) -> Vec<Value> {
    rows.iter().map(NodeRow::to_value).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_off_type_value_stays_in_extra_and_the_row_round_trips() {
        let raw = json!({
            "id": "x-1",
            "status": "shipped",
            "parent": ["x-0"],
            "rank": "high",
            "blocked_by": "x-2",
            "plan_path": null,
            "sessions": [{"session_id": "s1", "observed_model": {"model": "m"}, "ok": true}],
            "unknown_key": {"nested": [1, 2]},
        });
        let row: NodeRow = serde_json::from_value(raw.clone()).expect("the parse is total");
        assert_eq!(row.status(), Some(&Status::Other("shipped".into())));
        assert_eq!(row.parent(), None, "an array parent is off-type");
        assert_eq!(row.extra.get("parent"), Some(&json!(["x-0"])));
        assert_eq!(row.rank, None);
        assert_eq!(row.blocked_by().collect::<Vec<_>>(), ["x-2"]);
        assert_eq!(
            row.sessions().first().and_then(SessionRecord::model),
            Some("m")
        );
        assert_eq!(row.to_value(), raw, "every key, null and off-type included");
    }

    #[test]
    fn a_bad_list_element_hides_only_itself_and_the_row_round_trips() {
        let raw = json!({"id": "x-1", "sessions": ["oops", {"session_id": "s1"}]});
        let row = NodeRow::from_value(raw.clone());
        let sids: Vec<_> = row
            .sessions()
            .iter()
            .map(|s| s.session_id.as_deref())
            .collect();
        assert_eq!(sids, [Some("s1")]);
        assert_eq!(row.to_value(), raw);
    }

    #[test]
    fn claimed_reads_as_in_progress_and_the_legacy_key_answers_last() {
        let row = NodeRow::from_value(json!({"_status": "claimed"}));
        assert_eq!(row.status(), Some(&Status::InProgress));
        let both = NodeRow::from_value(json!({"status": "done", "_status": "ready"}));
        assert_eq!(both.status(), Some(&Status::Done));
    }

    #[test]
    fn every_column_name_parses_back_to_its_column() {
        for col in KanbanColumn::ALL
            .into_iter()
            .chain([KanbanColumn::OffBoard])
        {
            assert_eq!(KanbanColumn::parse(col.as_str()), Some(col));
        }
        assert_eq!(
            serde_json::to_value(KanbanColumn::InProgress).unwrap(),
            json!("In Progress")
        );
    }
}
