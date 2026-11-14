//! The `+flag` and `:value` grammar: the merged completion list, its fuzzy
//! rank, the paste parser, and the per-axis recent-use store. Every pick
//! commits through the same [`PickerAction`] a chip picker runs; no second
//! writer of any axis lives here.

use std::collections::BTreeMap;

use super::{
    branch_offered, effort_offered, picker_rows, split_flag_entry, CatalogOutcome, Focus,
    LaunchDraft, Launcher, PickerAction,
};
use crate::popup::PopupRow;

/// The glyph a recently used row wears; the rank reads it back.
pub(crate) const RECENT_GLYPH: &str = "\u{21ba}";
const RECENT_CAP: usize = 20;

/// Recent picks per axis, most recent first.
pub(crate) type Recent = BTreeMap<String, Vec<String>>;

/// The groups a sigil lists, in order: `+` leads with flags, `:` with values.
/// This one function carries the sigil-to-group rule.
pub(crate) fn group_order(sigil: char) -> Vec<Focus> {
    let values = [
        Focus::Harness,
        Focus::Model,
        Focus::Effort,
        Focus::Permission,
        Focus::Project,
        Focus::Branch,
        Focus::Worktree,
        Focus::Where,
    ];
    let mut order = Vec::new();
    if sigil == '+' {
        order.push(Focus::Plus);
        order.extend(values);
    } else {
        order.extend(values);
        order.push(Focus::Plus);
    }
    order
}

fn group_title(field: Focus) -> &'static str {
    match field {
        Focus::Plus => "flags",
        Focus::Harness => "harness",
        Focus::Model => "model",
        Focus::Effort => "effort",
        Focus::Permission => "perms",
        Focus::Project => "folder",
        Focus::Branch => "branch",
        Focus::Worktree => "worktree",
        Focus::Where => "where",
        Focus::Message => "message",
        Focus::WillRun => "command",
    }
}

fn axis_key(field: Focus, harness: &str) -> String {
    match field {
        Focus::Plus => format!("flags:{harness}"),
        other => group_title(other).to_string(),
    }
}

/// What a row is remembered and matched by: the flag name without its value
/// slot, or the label as shown.
fn row_key(field: Focus, label: &str) -> String {
    if field == Focus::Plus {
        split_flag_entry(label).0
    } else {
        label.to_string()
    }
}

/// The word a typed query is matched against: lowercase, no leading dashes,
/// no `<value>` slot.
fn core(label: &str) -> String {
    let lower = label.to_lowercase();
    let head = lower.split(" <").next().unwrap_or("");
    head.trim_start_matches('-').to_string()
}

/// Whether a typed filter names a row exactly, case aside.
pub(crate) fn names(label: &str, filter: &str) -> bool {
    core(label) == filter.to_lowercase()
}

/// The merged completion rows for a sigil: one header per axis, then that
/// axis's rows. Recent rows lead their group and wear [`RECENT_GLYPH`]. Rows
/// without a commit action (greyed notes, drill-down rows) are left out: a
/// completion list only holds what Tab can accept.
pub(crate) fn merged_rows(
    l: &Launcher,
    sigil: char,
    catalog: &Option<CatalogOutcome>,
    backlog: &[crate::proto::BacklogCard],
    recent: &Recent,
) -> (Vec<PopupRow>, Vec<Option<PickerAction>>) {
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let harness = l.draft.harness();
    for field in group_order(sigil) {
        if field == Focus::Effort && !effort_offered(l, catalog) {
            continue;
        }
        if matches!(field, Focus::Branch | Focus::Worktree) && !branch_offered(l, catalog) {
            continue;
        }
        let (src_rows, src_actions) = if field == Focus::Worktree {
            (
                vec![PopupRow::Entry {
                    glyph: "\u{2022}".to_string(),
                    label: "worktree".to_string(),
                    hint: "toggle a new worktree".to_string(),
                    enabled: true,
                }],
                vec![Some(PickerAction::ToggleWorktree)],
            )
        } else {
            picker_rows(l, field, catalog, backlog)
        };
        let seen = recent
            .get(&axis_key(field, &harness))
            .cloned()
            .unwrap_or_default();
        let mut group: Vec<(usize, PopupRow, PickerAction)> = Vec::new();
        for (row, action) in src_rows.into_iter().zip(src_actions) {
            let (
                PopupRow::Entry {
                    label,
                    hint,
                    enabled: true,
                    glyph,
                },
                Some(action),
            ) = (row, action)
            else {
                continue;
            };
            if matches!(
                action,
                PickerAction::OpenMore
                    | PickerAction::ShowSteps { .. }
                    | PickerAction::ShowApprovals(_)
                    | PickerAction::ApplySet(_)
                    | PickerAction::SaveSet
            ) {
                continue;
            }
            let at = seen.iter().position(|k| *k == row_key(field, &label));
            let glyph = if at.is_some() {
                RECENT_GLYPH.to_string()
            } else {
                glyph
            };
            group.push((
                at.unwrap_or(usize::MAX),
                PopupRow::Entry {
                    glyph,
                    label,
                    hint,
                    enabled: true,
                },
                PickerAction::Field(field, Box::new(action)),
            ));
        }
        if group.is_empty() {
            continue;
        }
        group.sort_by_key(|(at, _, _)| *at);
        rows.push(PopupRow::Header(group_title(field).to_string()));
        actions.push(None);
        for (_, row, action) in group {
            rows.push(row);
            actions.push(Some(action));
        }
    }
    super::sets::push_named(l, &mut rows, &mut actions);
    (rows, actions)
}

fn class(q: &str, label: &str) -> u8 {
    let c = core(label);
    if c.starts_with(q) {
        0
    } else if c.contains(q) {
        1
    } else {
        2
    }
}

/// Rank an already filtered completion list for the query: inside a group,
/// recent rows first, then prefix hits, then substring hits, then shorter
/// labels; groups with the best hit lead. A tie keeps the sigil's order. An
/// empty query ranks nothing.
pub(crate) fn rank_groups(
    rows: Vec<PopupRow>,
    actions: Vec<Option<PickerAction>>,
    q: &str,
) -> (Vec<PopupRow>, Vec<Option<PickerAction>>) {
    if q.is_empty() {
        return (rows, actions);
    }
    let q = q.to_lowercase();
    let mut groups: Vec<Vec<(PopupRow, Option<PickerAction>)>> = Vec::new();
    for pair in rows.into_iter().zip(actions) {
        if matches!(pair.0, PopupRow::Header(_)) || groups.is_empty() {
            groups.push(Vec::new());
        }
        if let Some(group) = groups.last_mut() {
            group.push(pair);
        }
    }
    let key = |pair: &(PopupRow, Option<PickerAction>)| match &pair.0 {
        PopupRow::Entry { glyph, label, .. } => (
            u8::from(glyph != RECENT_GLYPH),
            class(&q, label),
            label.chars().count(),
        ),
        _ => (0, 0, 0),
    };
    for group in &mut groups {
        let head = usize::from(matches!(group.first(), Some((PopupRow::Header(_), _))));
        if let Some(entries) = group.get_mut(head..) {
            entries.sort_by_key(key);
        }
    }
    let best = |group: &Vec<(PopupRow, Option<PickerAction>)>| {
        group
            .iter()
            .filter_map(|p| match &p.0 {
                PopupRow::Entry { label, .. } => Some(class(&q, label)),
                _ => None,
            })
            .min()
            .unwrap_or(2)
    };
    groups.sort_by_key(|g| best(g));
    groups.into_iter().flatten().unzip()
}

/// One vocabulary word and the action that commits it.
#[derive(Debug, Clone)]
pub(crate) struct VocabRow {
    core: String,
    field: Focus,
    action: PickerAction,
}

/// The paste vocabulary: every committed-by-name row of the merged list.
/// A flag that wants a value stays out; the picker captures that value.
pub(crate) fn vocab(
    l: &Launcher,
    catalog: &Option<CatalogOutcome>,
    backlog: &[crate::proto::BacklogCard],
) -> Vec<VocabRow> {
    let (rows, actions) = merged_rows(l, '+', catalog, backlog, &Recent::new());
    let mut out = Vec::new();
    for (row, action) in rows.into_iter().zip(actions) {
        let (PopupRow::Entry { label, .. }, Some(PickerAction::Field(field, inner))) =
            (row, action)
        else {
            continue;
        };
        if matches!(
            *inner,
            PickerAction::AddPill {
                picks_value: true,
                ..
            }
        ) {
            continue;
        }
        out.push(VocabRow {
            core: core(&label),
            field,
            action: *inner,
        });
    }
    out
}

fn unique<'a>(vocab: &'a [VocabRow], word: &str) -> Option<&'a VocabRow> {
    let word = word.to_lowercase();
    let mut hits = vocab.iter().filter(|v| v.core == word);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// Split text into `(whitespace before, word)` pairs; trailing whitespace
/// rides a final empty word.
fn tokens(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let word_at = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        let (ws, tail) = rest.split_at(word_at);
        let word_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (word, after) = tail.split_at(word_end);
        out.push((ws, word));
        rest = after;
    }
    out
}

/// Parse pasted text. A word-start `+word` or `:word`, or a lone sigil then
/// one space then a word, becomes a token only when the word names exactly
/// one vocabulary row, case aside. Tokens leave the text; everything else
/// stays byte for byte. A trailing colon on a word is never a sigil.
pub(crate) fn parse(text: &str, vocab: &[VocabRow]) -> (String, Vec<(Focus, PickerAction)>) {
    let toks = tokens(text);
    let mut out = String::new();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let Some(&(ws, word)) = toks.get(i) else {
            break;
        };
        let body = word.strip_prefix(['+', ':']);
        let found = match body {
            Some("") => match toks.get(i + 1) {
                Some((" ", next)) if !next.is_empty() => unique(vocab, next).map(|v| (v, 2)),
                _ => None,
            },
            Some(body) => unique(vocab, body).map(|v| (v, 1)),
            None => None,
        };
        if let Some((hit, used)) = found {
            hits.push((hit.field, hit.action.clone()));
            i += used;
            continue;
        }
        out.push_str(ws);
        out.push_str(word);
        i += 1;
    }
    if toks.first().is_some_and(|(_, w)| w.starts_with(['+', ':'])) && !hits.is_empty() {
        out = out.trim_start().to_string();
    }
    (out, hits)
}

// -- recent use ---------------------------------------------------------------

fn recent_path() -> std::path::PathBuf {
    crate::proto::mux_dir().join("composer-recent.json")
}

/// The remembered picks; a missing or unreadable file reads as empty.
pub(crate) fn read_recent() -> Recent {
    std::fs::read_to_string(recent_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn bump(recent: &mut Recent, axis: &str, key: &str) {
    if key.is_empty() {
        return;
    }
    let list = recent.entry(axis.to_string()).or_default();
    list.retain(|k| k != key);
    list.insert(0, key.to_string());
    list.truncate(RECENT_CAP);
}

/// Remember what a launch used: its harness, model, effort, mode and flags.
/// Flags are kept per harness.
pub(crate) fn remember_launch(draft: &LaunchDraft) {
    let mut recent = read_recent();
    let harness = draft.harness();
    bump(&mut recent, "harness", &harness);
    bump(
        &mut recent,
        "model",
        draft.model_row.as_deref().unwrap_or(&draft.model),
    );
    bump(&mut recent, "effort", &draft.effort);
    bump(&mut recent, "perms", &draft.permission);
    for (flag, _) in &draft.pills {
        bump(&mut recent, &format!("flags:{harness}"), flag);
    }
    if let Some(parent) = recent_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(&recent) {
        let _ = std::fs::write(recent_path(), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(core: &str, field: Focus, flag: &str) -> VocabRow {
        VocabRow {
            core: core.to_string(),
            field,
            action: PickerAction::AddPill {
                flag: flag.to_string(),
                picks_value: false,
            },
        }
    }

    #[test]
    fn composer_grammar_parse_takes_only_unique_words() {
        let vocab = vec![
            row("search", Focus::Plus, "--search"),
            row("opus", Focus::Model, "opus"),
            row("opus", Focus::Effort, "opus"),
        ];
        // A word-start sigil and a phone's spaced sigil both become tokens,
        // case aside; the rest of the text stays byte for byte.
        let (text, hits) = parse("fix it +Search", &vocab);
        assert_eq!((text.as_str(), hits.len()), ("fix it", 1));
        let (text, hits) = parse("fix + search now", &vocab);
        assert_eq!((text.as_str(), hits.len()), ("fix now", 1));
        // AC4: prose is untouched: a sum, a trailing colon, a name two rows share.
        for prose in ["count + 1 lines", "note: fix the test", ":opus", "a +zzz"] {
            let (text, hits) = parse(prose, &vocab);
            assert_eq!((text.as_str(), hits.len()), (prose, 0), "{prose}");
        }
    }
}
