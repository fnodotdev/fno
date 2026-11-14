//! The Decisions tab: the decisions table over the full terminal, a sibling
//! of the Messages tab. The strip row is the sideline's; the rows below are
//! the table's own render. Retract and overrule take their text on an entry
//! line here, then run the law door from this client with its stdin
//! inherited. Keys reach this view only from the client's own terminal
//! (`stdin_reader`), so the door reads the same real-tty operator proof a
//! pane would give it.

use super::*;
use crate::decisions_overlay::{
    action_argv, load_reviewed, mark_reviewed, peek, read_now, Action, DecisionRow, Filter,
};
use crate::decisions_table::{parse_key, render, store_path, Key, Table};
use crate::view_store::SidelineView;

pub(crate) struct DecisionsBoard {
    table: Table,
    /// A listing read is in flight and the rows may be empty or old.
    loading: bool,
    /// Book mode: the standing-law reading surface over the same rows.
    pub(crate) book: bool,
    /// An open retract or overrule entry line.
    entry: Option<Entry>,
}

/// The text a retract (its reason) or an overrule (the new ruling) waits on.
struct Entry {
    row: DecisionRow,
    retract: bool,
    text: String,
}

/// What a finished law door run sends back on the board channel.
#[derive(serde::Serialize, serde::Deserialize)]
struct DoorResult {
    door: String,
    ok: bool,
}

impl Entry {
    fn label(&self) -> &'static str {
        if self.retract {
            "reason"
        } else {
            "new text"
        }
    }
}

/// Open the board at once on the last listing (empty before the first read)
/// and re-read behind it. The read runs off the input thread and answers on
/// the alerts panel's channel, which `bell::apply` hands to `adopt`.
pub(crate) fn open(view: &mut View) {
    let rows = peek().map(|r| r.to_vec()).unwrap_or_default();
    open_rows(view, rows);
    reread(view);
}

/// Re-read the listing behind the open board, keeping filters and selection.
fn reread(view: &mut View) {
    let Some(tx) = view.bell.tx.clone() else {
        view.set_notice("decisions: the alerts channel is not ready".into());
        return;
    };
    if let Some(b) = view.decisions_board.as_mut() {
        b.loading = true;
    }
    tokio::spawn(async move {
        let read = tokio::task::spawn_blocking(read_now)
            .await
            .map_err(|e| format!("decisions read failed: {e}"))
            .and_then(|r| r);
        let _ = tx.send((
            bell::BOARD_GEN,
            read.and_then(|rows| {
                serde_json::to_value(rows.as_ref()).map_err(|e| format!("decisions: {e}"))
            }),
        ));
    });
}

/// Take a finished read into the open board, keeping the filters and the
/// selection.
pub(crate) fn adopt(view: &mut View, result: Result<serde_json::Value, String>) {
    if let Some(Ok(d)) = result
        .as_ref()
        .ok()
        .filter(|v| v.is_object())
        .map(<DoorResult as serde::Deserialize>::deserialize)
    {
        view.set_notice(d.door);
        if d.ok && view.decisions_board.is_some() {
            reread(view);
        }
        return;
    }
    let Some(b) = view.decisions_board.as_mut() else {
        return;
    };
    b.loading = false;
    match result.and_then(|v| {
        serde_json::from_value::<Vec<DecisionRow>>(v).map_err(|e| format!("decisions: {e}"))
    }) {
        Ok(rows) => {
            b.table.rows = rows;
            b.table.apply(Key::Other);
        }
        Err(e) => view.set_notice(format!("decisions: {e}")),
    }
}

/// Open the tab on `rows`; the snapshot shoots it on a fixed set.
pub(crate) fn open_rows(view: &mut View, rows: Vec<DecisionRow>) {
    view.panel_on = true;
    view.messages_board = None;
    view.backlog_board = None;
    view.org_board = None;
    view.decisions_board = Some(DecisionsBoard {
        table: Table {
            rows,
            reviewed: load_reviewed(&store_path()),
            filter: Filter::default(),
            pick: 0,
        },
        loading: false,
        book: false,
        entry: None,
    });
    view.region_owner = super::region_focus::RegionOwner::Board;
    super::backlog_board::set_sideline_view(view, SidelineView::Decisions);
}

/// Paint the style codes `render` emits (reset, bold, dim, reverse) into one
/// row of cells.
fn paint_ansi(row: &mut [Cell], line: &str) {
    let (mut flags, mut col) = (0u8, 0usize);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
                code.push(d);
            }
            flags = match code.as_str() {
                "1" => flags | cell_flags::BOLD,
                "2" => flags | cell_flags::DIM,
                "7" => flags | cell_flags::INVERSE,
                _ => 0,
            };
        } else if let Some(cell) = row.get_mut(col) {
            *cell = Cell {
                c,
                flags,
                ..Cell::default()
            };
            col += 1;
        }
    }
}

pub(crate) fn paint_full(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    cells[..rows * cols].fill(Cell::default());
    view.paint_top_row(cells, cols, cols);
    let Some(b) = &view.decisions_board else {
        return;
    };
    if b.book {
        for (i, line) in book_lines(&b.table.rows, cols, rows.saturating_sub(1))
            .iter()
            .enumerate()
        {
            if let Some(row) = cells.get_mut((i + 1) * cols..(i + 2) * cols) {
                paint_ansi(row, line);
            }
        }
        return;
    }
    for (i, line) in render(&b.table, cols, rows.saturating_sub(1))
        .iter()
        .enumerate()
    {
        if let Some(row) = cells.get_mut((i + 1) * cols..(i + 2) * cols) {
            paint_ansi(row, line);
        }
    }
    if b.loading && b.table.rows.is_empty() {
        if let Some(row) = cells.get_mut(3 * cols..4 * cols) {
            paint_ansi(row, "loading decisions...");
        }
    }
    if let Some(e) = &b.entry {
        let action = if e.retract { "retract" } else { "overrule" };
        let line = format!(
            "\x1b[1m{action} {}\x1b[0m {}: {}_  \x1b[2mEnter runs the law door, Esc cancels\x1b[0m",
            e.row.decision_id,
            e.label(),
            e.text
        );
        let last = rows.saturating_sub(1);
        if let Some(row) = cells.get_mut(last * cols..(last + 1) * cols) {
            row.fill(Cell::default());
            paint_ansi(row, &line);
        }
    }
}

/// The book's first sentence: the ruling text up to its first period or
/// newline, so a face reads in one line.
fn first_sentence(text: &str) -> &str {
    // A period only ends a sentence when whitespace follows; the period
    // inside `v0.5.0` does not.
    let cut = text
        .match_indices('.')
        .find(|(i, _)| text[*i + 1..].starts_with([' ', '\n', '\t']) || text[*i + 1..].is_empty())
        .map(|(i, _)| i + 1)
        .or_else(|| text.find('\n'))
        .unwrap_or(text.len());
    text[..cut].trim()
}

/// The book paint: one header line, one heading per section present, then a
/// two-line topic block (plain-words title with the fold count, the face's
/// first sentence). No decision id appears anywhere in book mode.
fn book_lines(rows: &[DecisionRow], _cols: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let groups = crate::lawbook::book(rows);
    if groups.is_empty() {
        lines.push("the rule book is empty: no standing law rows yet".into());
        return lines;
    }
    lines.push("the rule book - standing law by section, [b] back to the table".into());
    for (section, topics) in groups {
        lines.push(String::new());
        lines.push(section.title().to_uppercase());
        for t in topics {
            let fold = if t.folded.is_empty() {
                String::new()
            } else {
                format!("  (+{} older)", t.folded.len())
            };
            lines.push(format!("  {}{}", t.title, fold));
            lines.push(format!("    {}", first_sentence(&t.face.decision)));
        }
    }
    lines.truncate(max_lines);
    lines
}

pub(crate) async fn route_keys(
    view: &mut View,
    scanner: &mut Scanner,
    bytes: &[u8],
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    scanner.disarm_repeat();
    for event in scanner.scan(bytes, Instant::now()) {
        match event {
            Event::Forward(chunk) => keys(view, &chunk),
            event => match dispatch_event(view, event, sock).await? {
                DispatchFlow::Continue => {}
                DispatchFlow::Break => break,
                DispatchFlow::Detach => return Ok(StdinFlow::Detach),
            },
        }
    }
    Ok(StdinFlow::Continue)
}

pub(crate) fn keys(view: &mut View, bytes: &[u8]) {
    if view
        .decisions_board
        .as_ref()
        .is_some_and(|b| b.entry.is_some())
    {
        entry_keys(view, bytes);
        return;
    }
    let key = if bytes == b"\x1b" {
        Key::Quit
    } else {
        parse_key(bytes)
    };
    let Some(b) = view.decisions_board.as_mut() else {
        return;
    };
    match key {
        Key::Quit => {
            view.decisions_board = None;
            super::backlog_board::set_sideline_view(view, SidelineView::Agents);
        }
        Key::Review => {
            if let Some(id) = b.table.selected().map(|r| r.decision_id.clone()) {
                match mark_reviewed(&store_path(), &id) {
                    Ok(()) => {
                        b.table.reviewed.insert(id);
                        b.table.apply(Key::Other);
                    }
                    Err(e) => view.set_notice(format!("review mark not saved: {e}")),
                }
            }
        }
        Key::Goto => {
            if let Some(q) = b.table.selected().and_then(|r| r.question_id.clone()) {
                view.open_detail_on(&q);
            }
        }
        Key::Retract | Key::Overrule => {
            if let Some(row) = b.table.selected().cloned() {
                b.entry = Some(Entry {
                    row,
                    retract: key == Key::Retract,
                    text: String::new(),
                });
            }
        }
        Key::Book => b.book = !b.book,
        k => b.table.apply(k),
    }
}

/// One keystroke into the open entry line: Esc cancels, Backspace erases,
/// Enter runs the door, and printable text appends.
fn entry_keys(view: &mut View, bytes: &[u8]) {
    let Some(b) = view.decisions_board.as_mut() else {
        return;
    };
    let Some(e) = b.entry.as_mut() else {
        return;
    };
    match bytes {
        b"\x1b" => b.entry = None,
        b"\x7f" | b"\x08" => {
            e.text.pop();
        }
        b"\r" | b"\n" => {
            let text = e.text.trim().to_string();
            // A leading dash would read as a door flag.
            if text.is_empty() || text.starts_with('-') {
                let label = e.label();
                view.set_notice(format!("{label}: type the text first, not starting with -"));
                return;
            }
            let Some(e) = b.entry.take() else {
                return;
            };
            let action = if e.retract {
                Action::Retract { reason: text }
            } else {
                Action::Overrule { new_text: text }
            };
            run_door(view, action_argv(&e.row, &action));
        }
        // An arrow or other escape sequence edits nothing.
        other if other.starts_with(b"\x1b") => {}
        other => {
            if let Ok(s) = std::str::from_utf8(other) {
                e.text.extend(s.chars().filter(|c| !c.is_control()));
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static LAST_DOOR: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// The argv the last Enter handed the door, taken once (tests only).
#[cfg(test)]
pub(crate) fn last_door_argv() -> Option<Vec<String>> {
    LAST_DOOR.with(|d| d.borrow_mut().take())
}

/// Run the law door off the input loop with this client's terminal as its
/// stdin, the operator proof. Its output never paints over the screen: the
/// exit lands as a notice, and a re-read follows a success.
fn run_door(view: &mut View, argv: Vec<String>) {
    #[cfg(test)]
    {
        LAST_DOOR.with(|d| *d.borrow_mut() = Some(argv));
        view.set_notice("law door: running".into());
    }
    #[cfg(not(test))]
    {
        let Some(tx) = view.bell.tx.clone() else {
            view.set_notice("decisions: the alerts channel is not ready".into());
            return;
        };
        view.set_notice(format!(
            "law door: running fno {}",
            argv.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
        ));
        let mut command = crate::process_admission::tokio_command(crate::server::fno_bin());
        command
            .args(&argv)
            .stdin(std::process::Stdio::inherit())
            .kill_on_drop(true);
        tokio::spawn(async move {
            let out = crate::process_admission::tokio_output(&mut command).await;
            let ok = out.as_ref().is_ok_and(|o| o.status.success());
            let notice = match out {
                Ok(o) if o.status.success() => "law door: done".to_string(),
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    let line = err
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("");
                    format!(
                        "law door refused (exit {}): {line}",
                        o.status.code().unwrap_or(-1)
                    )
                }
                Err(e) => format!("law door: cannot run fno: {e}"),
            };
            let _ = tx.send((
                bell::BOARD_GEN,
                serde_json::to_value(DoorResult { door: notice, ok }).map_err(|e| e.to_string()),
            ));
        });
    }
}

pub(crate) async fn mouse(
    view: &mut View,
    rep: crate::mouse::MouseReport,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if rep.row != 0
        || !matches!(
            rep.kind,
            crate::proto::MouseKind::Press(crate::proto::MouseButton::Left)
        )
    {
        return Ok(());
    }
    if bell::button_at(view, rep.row, rep.col) {
        return apply_hit(view, ChromeHit::Bell(bell::Hit::Toggle), sock).await;
    }
    for (start, w, v) in view.top_row_spans() {
        if (rep.col as usize) >= start && (rep.col as usize) < start + w {
            apply_hit(view, ChromeHit::TopRow(v), sock).await?;
        }
    }
    Ok(())
}
