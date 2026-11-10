//! Saved sets: up to nine whole loadouts kept in `composer-sets.json` in the
//! mux dir. A digit with focus off the prompt, a tap on a row in the more
//! fold, or `:name` in the prompt applies one. Every apply goes through the
//! draft's own fields, so a set can never launch what the pickers could not.

use serde::{Deserialize, Serialize};

use super::*;

pub(crate) const MAX_SETS: usize = 9;
const FILE: &str = "composer-sets.json";
const NAME_CAP: usize = 24;

/// One saved loadout. An empty axis means the harness decides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ComposerSet {
    pub name: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub model_row: Option<String>,
    #[serde(default)]
    pub route: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub placement: String,
    #[serde(default)]
    pub worktree: Option<bool>,
}

/// The card's view of the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Sets {
    pub list: Vec<ComposerSet>,
    /// The one line the card shows: an unreadable file, a failed save, or a
    /// harness a set named that is gone.
    pub notice: Option<String>,
    /// The name being typed after the `save as set` row.
    pub naming: Option<String>,
}

fn path() -> std::path::PathBuf {
    crate::proto::mux_dir().join(FILE)
}

fn placement_word(p: Placement) -> &'static str {
    match p {
        Placement::Thread => "thread",
        Placement::ThreadSplitBeside => "split",
        Placement::ThreadNewTab => "tab",
        Placement::PaneActiveTab => "pane",
    }
}

fn placement_from(word: &str) -> Placement {
    match word {
        "split" => Placement::ThreadSplitBeside,
        "tab" => Placement::ThreadNewTab,
        "pane" => Placement::PaneActiveTab,
        _ => Placement::Thread,
    }
}

/// A set name is one word, so `:name` can find it: no whitespace, no leading
/// sigil or dash.
fn clean_name(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .trim_start_matches(['-', '+', ':'])
        .chars()
        .take(NAME_CAP)
        .collect()
}

impl ComposerSet {
    fn from_draft(name: String, d: &LaunchDraft) -> Self {
        Self {
            name,
            harness: d.harness(),
            model: d.model.clone(),
            model_row: d.model_row.clone(),
            route: d.route.clone(),
            provider: d.provider.clone(),
            effort: d.effort.clone(),
            permission: d.permission.clone(),
            host: d.host.clone(),
            placement: placement_word(d.placement).to_string(),
            worktree: d.worktree,
        }
    }

    /// The axes a row shows beside the name.
    pub(crate) fn summary(&self) -> String {
        let model = self
            .model_row
            .as_deref()
            .filter(|m| !m.is_empty())
            .unwrap_or(&self.model);
        [self.harness.as_str(), model, self.effort.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    }
}

impl Sets {
    /// Read the store. A missing file is no sets; an unreadable one is no
    /// sets and a notice naming the file.
    pub(crate) fn load() -> Self {
        let text = match std::fs::read_to_string(path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => return Self::unreadable(&e.to_string()),
        };
        match serde_json::from_str::<Vec<ComposerSet>>(&text) {
            Ok(mut list) => {
                list.retain(|s| !clean_name(&s.name).is_empty());
                list.truncate(MAX_SETS);
                Self {
                    list,
                    ..Self::default()
                }
            }
            Err(e) => Self::unreadable(&e.to_string()),
        }
    }

    fn unreadable(why: &str) -> Self {
        Self {
            notice: Some(format!("{FILE} unreadable, so no sets: {why}")),
            ..Self::default()
        }
    }

    /// Write the list atomically: a temp file in the same dir, then rename.
    fn save(&self) -> std::io::Result<()> {
        let file = path();
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.list)?)?;
        std::fs::rename(&tmp, &file)
    }
}

/// Apply set `i` onto the draft. A harness the catalog no longer lists leaves
/// the model axes alone (they belong to that harness), applies the rest and
/// says which harness is gone.
pub(crate) fn apply(l: &mut Launcher, catalog: &Option<CatalogOutcome>, portal: u8, i: usize) {
    let Some(set) = l.sets.list.get(i).cloned() else {
        return;
    };
    l.sets.notice = None;
    let mut missing = None;
    match l.draft.harnesses.iter().position(|h| *h == set.harness) {
        Some(idx) => {
            l.draft.harness_idx = idx;
            l.draft.model = set.model;
            l.draft.model_row = set.model_row;
            l.draft.route = set.route;
            l.draft.provider = set.provider;
        }
        None if set.harness.is_empty() => {}
        None => missing = Some(set.harness.clone()),
    }
    l.draft.effort = set.effort;
    l.draft.permission = set.permission;
    l.draft.host = set.host;
    l.draft.placement = placement_from(&set.placement);
    l.draft.placement_portal = portal;
    l.draft.worktree = set.worktree;
    l.draft.bump();
    clear_unoffered_pins(&mut l.draft, catalog);
    if let Some(harness) = missing {
        l.sets.notice = Some(format!(
            "set {}: harness {harness} is not installed; the rest applied",
            set.name
        ));
    }
}

/// A digit key with focus off the prompt: `1` to `9` apply that set. True
/// when the key was a set key, so a digit with no set behind it still reads
/// as typed-and-dropped rather than as an error.
pub(crate) fn apply_digit(
    l: &mut Launcher,
    catalog: &Option<CatalogOutcome>,
    portal: u8,
    c: char,
) -> bool {
    let Some(n) = c.to_digit(10).filter(|n| *n >= 1) else {
        return false;
    };
    apply(l, catalog, portal, n as usize - 1);
    true
}

/// The more fold's saved sets, each a tappable row. With no sets saved the
/// fold shows nothing here, so the flags keep their place in the short list.
pub(crate) fn push_rows(
    l: &Launcher,
    rows: &mut Vec<PopupRow>,
    actions: &mut Vec<Option<PickerAction>>,
) {
    if l.sets.list.is_empty() {
        return;
    }
    rows.push(PopupRow::Header("saved sets".to_string()));
    actions.push(None);
    for (i, set) in l.sets.list.iter().enumerate() {
        push_entry(
            rows,
            actions,
            &(i + 1).to_string(),
            &set.name,
            &set.summary(),
            true,
            Some(PickerAction::ApplySet(i)),
        );
    }
}

/// The `save as set` row, last in the more fold.
pub(crate) fn push_save(rows: &mut Vec<PopupRow>, actions: &mut Vec<Option<PickerAction>>) {
    push_entry(
        rows,
        actions,
        "\u{2022}",
        "save as set",
        "name the current loadout",
        true,
        Some(PickerAction::SaveSet),
    );
}

/// The `:name` group of the completion list: set names, so the grammar's
/// unique-word rule applies them like any other value.
pub(crate) fn push_named(
    l: &Launcher,
    rows: &mut Vec<PopupRow>,
    actions: &mut Vec<Option<PickerAction>>,
) {
    if l.sets.list.is_empty() {
        return;
    }
    rows.push(PopupRow::Header("set".to_string()));
    actions.push(None);
    for (i, set) in l.sets.list.iter().enumerate() {
        push_entry(
            rows,
            actions,
            "\u{2022}",
            &set.name,
            &set.summary(),
            true,
            Some(PickerAction::Field(
                Focus::Plus,
                Box::new(PickerAction::ApplySet(i)),
            )),
        );
    }
}

/// The `save as set` row: the prompt line asks for a name.
pub(crate) fn begin_naming(l: &mut Launcher) {
    l.sets.notice = None;
    l.sets.naming = Some(String::new());
    l.focus = Focus::Message;
}

/// Keys while the name is typed. Enter saves, Esc drops it; a name with the
/// same spelling as a saved one replaces that set, which is how a set is
/// edited. Returns true when the key was taken.
pub(crate) fn name_key(l: &mut Launcher, key: &LKey) -> bool {
    let Some(name) = l.sets.naming.as_mut() else {
        return false;
    };
    match key {
        LKey::Char(c) if !c.is_whitespace() && name.chars().count() < NAME_CAP => name.push(*c),
        LKey::Backspace => {
            name.pop();
        }
        LKey::Esc => l.sets.naming = None,
        LKey::Enter => commit_name(l),
        _ => {}
    }
    true
}

fn commit_name(l: &mut Launcher) {
    let name = clean_name(&l.sets.naming.take().unwrap_or_default());
    if name.is_empty() {
        return;
    }
    let set = ComposerSet::from_draft(name, &l.draft);
    match l
        .sets
        .list
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(&set.name))
    {
        Some(at) => {
            if let Some(slot) = l.sets.list.get_mut(at) {
                *slot = set;
            }
        }
        None if l.sets.list.len() < MAX_SETS => l.sets.list.push(set),
        None => {
            l.sets.notice = Some(format!(
                "{MAX_SETS} sets saved; save under an existing name to replace one"
            ));
            return;
        }
    }
    if let Err(e) = l.sets.save() {
        l.sets.notice = Some(format!("{FILE} not saved: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(name: &str, harness: &str, model: &str, effort: &str) -> ComposerSet {
        ComposerSet {
            name: name.to_string(),
            harness: harness.to_string(),
            model: model.to_string(),
            model_row: Some(model.to_string()),
            effort: effort.to_string(),
            permission: "plan".to_string(),
            placement: "tab".to_string(),
            worktree: Some(true),
            ..ComposerSet::default()
        }
    }

    #[test]
    fn composer_sets_roundtrip_and_a_corrupt_file_reads_as_no_sets() {
        // AC13-EDGE: a corrupt store opens as no sets and the notice names it.
        std::fs::create_dir_all(crate::proto::mux_dir()).unwrap();
        std::fs::write(path(), "{not json").unwrap();
        let loaded = Sets::load();
        assert!(loaded.list.is_empty());
        assert!(loaded.notice.as_deref().unwrap().contains(FILE));
        // A save replaces the corrupt file atomically and reads back.
        let saved = Sets {
            list: vec![set("daily", "claude", "opus", "high")],
            ..Sets::default()
        };
        saved.save().unwrap();
        assert_eq!(Sets::load().list, saved.list);
        assert!(!path().with_extension("json.tmp").exists());
        // A name is one word, so `:name` can find it.
        assert_eq!(clean_name("  --my  set "), "my-set");
    }
}
