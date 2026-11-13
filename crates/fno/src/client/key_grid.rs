//! The `?` footer: the keys of the view in focus, laid out as a short grid
//! of key and label cells. One builder serves every view, so a view names
//! its own keys and the grid shape cannot drift between them. The full
//! keys overlay stays one key further, the `*` cell the grid always ends in.

use super::{Cell, StdinFlow, View};
use crate::text_style::TLine;

/// One cell of the grid: a key and what it does.
pub(crate) type KeyHint = (String, String);

/// The view whose keys the footer names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyView {
    Agents,
    Backlog,
    Detail,
    Messages,
    Org,
}

/// The agents footer, in reading order: each cell is a prefix-chord action id
/// and its label. The key comes off the live binding table, so a rebind or a
/// changed default moves the footer with it; a cell whose action holds no
/// chord drops out.
pub(crate) const AGENTS_ACTIONS: &[(&str, &str)] = &[
    ("toggle-composer", "new session"),
    ("new-workspace", "new workspace"),
    ("sideline-menu", "menu"),
    ("cycle-sideline-view", "cycle views"),
    ("open-backlog-board", "backlog"),
    ("open-messages", "messages"),
    ("org-view", "HQ"),
    ("detach", "detach"),
];

fn fixed(rows: &[(&str, &str)]) -> Vec<KeyHint> {
    rows.iter()
        .map(|(k, l)| (k.to_string(), l.to_string()))
        .collect()
}

/// The agents cells for `bindings`, each key written as the prefix then the
/// chord (`^B H`).
fn agents_hints(bindings: &[crate::keys::KeyBinding]) -> Vec<KeyHint> {
    let prefix = match crate::keys::prefix_display().strip_prefix("ctrl+") {
        Some(letter) => format!("^{}", letter.to_ascii_uppercase()),
        None => crate::keys::prefix_display(),
    };
    AGENTS_ACTIONS
        .iter()
        .filter_map(|(action, label)| {
            let kb = bindings.iter().find(|kb| kb.action == *action)?;
            Some((format!("{prefix} {}", kb.disp), label.to_string()))
        })
        .collect()
}

/// The keys of one view, in reading order. The closing `* all keys` cell is
/// the builder's, not the view's.
pub(crate) fn hints(view: KeyView) -> Vec<KeyHint> {
    match view {
        KeyView::Agents => agents_hints(&crate::keys::key_bindings()),
        KeyView::Backlog => fixed(&[
            ("hjkl", "move"),
            ("[ ]", "lane"),
            ("L", "lanes"),
            ("Tab", "list/kanban"),
            ("/", "search"),
            ("f", "filter"),
            ("enter", "details"),
            ("c", "columns"),
            ("b", "blueprint"),
            ("t", "target"),
            ("A", "lead"),
            ("T/K/J", "rank"),
            ("F", "full screen"),
            ("esc", "close"),
        ]),
        KeyView::Detail => fixed(&[
            ("j/k", "scroll"),
            ("tab", "next link"),
            ("enter", "open"),
            ("y", "copy id"),
            ("Y", "copy command"),
            ("b", "blueprint"),
            ("t", "target"),
            ("A", "lead"),
            ("c", "comment"),
            ("esc", "board"),
        ]),
        KeyView::Messages => fixed(&[
            ("h/l", "columns"),
            ("j/k", "move"),
            ("enter", "open"),
            ("esc", "back"),
            ("a/b", "filter"),
            ("s", "sort"),
            ("d", "details"),
            ("r", "retry"),
            ("x", "exchanges"),
            ("m", "id"),
            ("y", "copy"),
            ("q", "close"),
        ]),
        KeyView::Org => fixed(&[
            ("j/k", "move"),
            ("h/l", "tab"),
            ("u", "go up"),
            ("enter", "open or attach"),
            ("space", "peek, stop, portal"),
            ("n", "new session"),
            ("L", "start a lead"),
            ("s", "start a seat"),
            ("r", "review PR"),
            ("W", "HQ web page"),
            ("F", "full screen"),
            ("V", "cycle views"),
            ("esc", "close"),
        ]),
    }
}

/// The grid never grows past this many rows; a narrow pane drops the last
/// view keys before it drops the `* all keys` cell.
const MAX_ROWS: usize = 4;

/// The columns that fit `w`: four where each cell reads whole, fewer on a
/// narrow pane so a label never clips mid-word on a wide-enough terminal.
fn columns(w: usize) -> usize {
    match w {
        0..=39 => 1,
        40..=69 => 2,
        _ => 4,
    }
}

/// The grid rows for `view` at width `w`: cells read left to right, then
/// top to bottom, the key bold and the label plain. The last cell is
/// `* all keys`. Every row is padded to the same column width, and a cell
/// wider than its column is cut at the column edge.
pub(crate) fn grid_rows(view: KeyView, w: usize) -> Vec<TLine> {
    let cols = columns(w);
    let mut cells: Vec<KeyHint> = hints(view);
    cells.truncate(cols * MAX_ROWS - 1);
    cells.push(("*".to_string(), "all keys".to_string()));
    let col_w = (w / cols).max(1);
    cells
        .chunks(cols)
        .map(|chunk| {
            let mut row = TLine::plain(String::new());
            for (key, label) in chunk {
                let mut cell = TLine::head(key.as_str());
                cell.push_line(TLine::plain(format!(" {label}")));
                row.push_line(cell.trunc(col_w.saturating_sub(1)).pad_to(col_w));
            }
            row.trim_end()
        })
        .collect()
}

/// Paint `grid` over the bottom rows of a `width`-wide surface, blanking
/// them first so the grid never lands on the view's own rows.
pub(super) fn paint_bottom(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    width: usize,
    grid: &[TLine],
) {
    let h = grid.len().min(rows.saturating_sub(1));
    let top = rows - h;
    for r in top..rows {
        for c in 0..width.min(cols) {
            cells[r * cols + c] = Cell::default();
        }
    }
    super::backlog_style::paint_panel(
        cells,
        rows,
        cols,
        top,
        width.min(cols),
        h,
        grid,
        None,
        &view.theme,
    );
}

/// Smallest terminal that has room for the footer grid; below it `^B ?`
/// opens the full key table modal instead.
const MIN_ROWS: u16 = 16;
const MIN_COLS: u16 = 40;

/// Every footer flag, the boards first and the agents list last.
fn flags(view: &mut View) -> [Option<&mut bool>; 4] {
    [
        view.backlog_board.as_mut().map(|b| &mut b.keys_footer),
        view.org_board.as_mut().map(|b| &mut b.keys_footer),
        view.messages_board.as_mut().map(|b| &mut b.keys_footer),
        Some(&mut view.keys_footer),
    ]
}

fn footer_is_open(view: &mut View) -> bool {
    flags(view).iter().flatten().any(|f| **f)
}

fn close_footers(view: &mut View) {
    flags(view).into_iter().flatten().for_each(|f| *f = false);
}

/// `^B ?`: the footer grid of the view in focus when the terminal has room,
/// else the full key table modal.
pub(super) fn toggle(view: &mut View) {
    if view.term.0 < MIN_ROWS || view.term.1 < MIN_COLS {
        view.open_keys_modal();
        return;
    }
    if footer_is_open(view) {
        close_footers(view);
    } else if let Some(open) = flags(view).into_iter().flatten().next() {
        *open = true;
    }
}

/// The agents-list footer, full width along the bottom, shown only while no
/// board covers the list.
pub(super) fn paint_agents(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    if view.keys_footer {
        paint_bottom(
            view,
            cells,
            rows,
            cols,
            cols,
            &grid_rows(KeyView::Agents, cols),
        );
    }
}

/// Keys while a footer is open: `^B ?`, `?` and Esc close it, `*` opens the
/// full key table, anything else closes it and goes on to its normal owner.
pub(super) fn footer_keys(view: &mut View, bytes: &[u8]) -> Option<Result<StdinFlow, String>> {
    if bytes.is_empty() || !footer_is_open(view) {
        return None;
    }
    close_footers(view);
    match bytes {
        [0x02, b'?'] | [b'?'] | [0x1b] => Some(Ok(StdinFlow::Continue)),
        [b'*'] => {
            view.open_keys_modal();
            Some(Ok(StdinFlow::Continue))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agents_footer_action_holds_a_chord() {
        let bindings = crate::keys::default_bindings();
        for (action, _) in AGENTS_ACTIONS {
            let kb = bindings
                .iter()
                .find(|kb| kb.action == *action)
                .unwrap_or_else(|| panic!("footer action {action:?} is not in the key table"));
            assert_ne!(kb.key, 0, "footer action {action:?} has no default chord");
        }
        assert_eq!(agents_hints(&bindings).len(), AGENTS_ACTIONS.len());
    }

    #[test]
    fn agents_footer_names_the_live_keys() {
        let mut bindings = crate::keys::default_bindings();
        let cells = agents_hints(&bindings);
        let cell = |label: &str| {
            cells
                .iter()
                .find(|(_, l)| l == label)
                .map(|(k, _)| k.clone())
        };
        assert_eq!(cell("backlog").as_deref(), Some("^B B"));
        assert_eq!(cell("HQ").as_deref(), Some("^B H"));
        bindings
            .iter_mut()
            .find(|kb| kb.action == "org-view")
            .unwrap()
            .disp = "Z".into();
        let cells = agents_hints(&bindings);
        assert!(cells.iter().any(|(k, l)| k == "^B Z" && l == "HQ"));
    }

    #[test]
    fn grid_is_four_wide_and_ends_in_the_full_overlay_cell() {
        let rows = grid_rows(KeyView::Messages, 100);
        assert_eq!(rows.len(), 4, "12 keys + the closing cell = 13 cells");
        assert!(rows[0].text.starts_with("h/l columns"));
        assert!(rows
            .last()
            .is_some_and(|r| r.text.trim_end().ends_with("* all keys")));
    }
}
