//! `fno inbox decisions --table`: the Decisions table, one interactive
//! screen. It runs in a mux pane, so its stdin is a real terminal. A retract
//! or an overrule leaves the screen and runs the law door with that terminal
//! inherited, which is the operator proof the door reads (`law_cli`).

use crate::decisions_overlay::{
    action_argv, load, load_reviewed, mark_reviewed, visible, Action, Author, DecisionRow, Filter,
};
use std::collections::HashSet;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Lane,
    State,
    Author,
    Audit,
    Retract,
    Overrule,
    Review,
    Goto,
    Book,
    Quit,
    Other,
}

pub fn parse_key(b: &[u8]) -> Key {
    match b {
        b"\x1b[A" | b"k" => Key::Up,
        b"\x1b[B" | b"j" => Key::Down,
        b"l" => Key::Lane,
        b"s" => Key::State,
        b"a" => Key::Author,
        b"u" => Key::Audit,
        b"r" => Key::Retract,
        b"o" => Key::Overrule,
        b"v" => Key::Review,
        b"g" => Key::Goto,
        b"b" => Key::Book,
        b"q" | b"\x03" | b"\x04" => Key::Quit,
        _ => Key::Other,
    }
}

pub struct Table {
    pub rows: Vec<DecisionRow>,
    pub reviewed: HashSet<String>,
    pub filter: Filter,
    pub pick: usize,
}

impl Table {
    fn shown(&self) -> Vec<&DecisionRow> {
        visible(&self.rows, &self.filter, &self.reviewed)
    }

    pub(crate) fn selected(&self) -> Option<&DecisionRow> {
        self.shown().get(self.pick).copied()
    }

    /// Apply a navigation or filter key; an action key returns to the loop.
    pub fn apply(&mut self, key: Key) {
        match key {
            Key::Up => self.pick = self.pick.saturating_sub(1),
            Key::Down => self.pick = (self.pick + 1).min(self.shown().len().saturating_sub(1)),
            Key::Lane => self.filter.cycle_lane(),
            Key::State => self.filter.cycle_state(),
            Key::Author => self.filter.cycle_author(),
            Key::Audit => self.filter.toggle_audit(),
            _ => {}
        }
        self.pick = self.pick.min(self.shown().len().saturating_sub(1));
    }
}

const AUDIENCE_MIN_W: usize = 14;
const AUDIENCE_MAX_W: usize = 30;
const RULING_MIN_W: usize = 24;

/// The audience cell: the chip word, then the persona jobs that fit whole
/// within `w`. A job that would not fit is dropped, never cut mid-word.
fn audience_cell(r: &DecisionRow, w: usize) -> String {
    let mut cell = r
        .audience
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("audience?")
        .to_string();
    for (i, job) in r.job.iter().enumerate() {
        let sep = if i == 0 { " " } else { "," };
        if cell.chars().count() + sep.len() + job.chars().count() > w {
            break;
        }
        cell.push_str(sep);
        cell.push_str(job);
    }
    cell
}

fn fit(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(n).collect();
    while out.chars().count() < n {
        out.push(' ');
    }
    out
}

/// The screen as lines, `h` rows of at most `w` columns. The selected row is
/// reverse video and every other style is bold or dim, so the table reads the
/// same on a light and a dark ground.
pub fn render(t: &Table, w: usize, h: usize) -> Vec<String> {
    let shown = t.shown();
    let f = &t.filter;
    let head = format!(
        "\x1b[1mDecisions\x1b[0m  {} of {}  lane:{} state:{} by:{}{}",
        shown.len(),
        t.rows.len(),
        f.lane.unwrap_or("all"),
        f.state.unwrap_or("all"),
        match f.author {
            None => "all",
            Some(Author::User) => "user",
            Some(Author::Agent) => "agent",
            Some(Author::Unknown) => "?",
        },
        if f.audit_only { "  [audit queue]" } else { "" }
    );
    // The description column shrinks before the ruling vanishes: 80
    // fixed columns plus one gap precede it, and a docked host pane is
    // narrower than the full view's 98-column floor.
    // The audience column grows to the longest chip shown, so a chip like
    // customer:solo-founder is not cut mid-word. It stops growing once the
    // ruling would drop under RULING_MIN_W, so a narrow pane keeps the old
    // 14-column cell.
    let aud_w = shown
        .iter()
        .map(|r| audience_cell(r, usize::MAX).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(AUDIENCE_MIN_W, AUDIENCE_MAX_W)
        .min(
            w.saturating_sub(66 + 1 + 16 + 1 + RULING_MIN_W)
                .max(AUDIENCE_MIN_W),
        );
    let fixed = 66 + aud_w;
    let desc_w = 16.min(w.saturating_sub(fixed + 1));
    let text_w = w.saturating_sub(fixed + 1 + desc_w + 1);
    let columns = table_columns(aud_w, desc_w, text_w);
    let mut out = vec![head, format!("\x1b[1m{columns}\x1b[0m")];
    let body_rows = h.saturating_sub(5);
    let top = t.pick.saturating_sub(body_rows.saturating_sub(1));
    for (i, r) in shown.iter().enumerate().skip(top).take(body_rows) {
        let line = format!(
            "{} {} {} {} {} {} {} {} {} {}",
            fit(&r.lifecycle, 10),
            fit(&r.lane, 12),
            fit(&audience_cell(r, aud_w), aud_w),
            fit(r.ts.get(..10).unwrap_or(&r.ts), 10),
            fit(&r.decider(), 10),
            if r.has_quote() { "Q" } else { " " },
            fit(&r.subject_node, 9),
            fit(&r.subject_pr, 6),
            fit(r.describes(), desc_w),
            fit(&r.decision, text_w),
        );
        out.push(if i == t.pick {
            format!("\x1b[7m{line}\x1b[0m")
        } else {
            line
        });
    }
    while out.len() < h.saturating_sub(2) {
        out.push(String::new());
    }
    // The question the selected decision answered, so the pair reads as one.
    out.push(
        match shown.get(t.pick).and_then(|r| r.question.as_deref()) {
            Some(q) => format!("\x1b[2mquestion: {}\x1b[0m", fit(q, w.saturating_sub(10))),
            None => String::new(),
        },
    );
    // The key hints keep whole pairs: the docked pane is narrower than
    // the full string, and a mid-token cut reads as a broken key.
    let hints = [
        ("j/k", "move"),
        ("l", "lane"),
        ("s", "state"),
        ("a", "author"),
        ("u", "audit queue"),
        ("r", "retract"),
        ("o", "overrule"),
        ("v", "reviewed"),
        ("g", "question"),
        ("q", "quit"),
    ];
    let mut hint = String::new();
    for (k, v) in hints {
        let seg = format!("{k} {v}  ");
        if hint.chars().count() + seg.chars().count() > w.saturating_sub(2) {
            break;
        }
        hint.push_str(&seg);
    }
    out.push(format!("\x1b[2m{}\x1b[0m", hint.trim_end()));
    out
}

/// The column header row: the fixed labels plus the width-aware
/// description and decision labels. A narrow dock shortens the
/// description word rather than cutting it mid-token.
fn table_columns(aud_w: usize, desc_w: usize, text_w: usize) -> String {
    let desc_label = if desc_w >= 11 { "description" } else { "desc" };
    format!(
        "{} {} {} {} {} {} {} {} {} {}",
        fit("state", 10),
        fit("lane", 12),
        fit("audience", aud_w),
        fit("date", 10),
        fit("who", 10),
        "Q",
        fit("node", 9),
        fit("PR", 6),
        fit(desc_label, desc_w),
        fit("decision", text_w),
    )
}
fn prompt(label: &str) -> Option<String> {
    print!("{label}: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    let line = line.trim().to_string();
    // A leading dash would read as a door flag.
    (!line.is_empty() && !line.starts_with('-')).then_some(line)
}

/// Run the law door on `row` with this terminal inherited.
fn run_door(row: &DecisionRow, action: &Action) {
    let status = std::process::Command::new(crate::server::fno_bin())
        .args(action_argv(row, action))
        .status();
    match status {
        Ok(s) if s.success() => println!("done"),
        Ok(s) => println!("the door refused (exit {})", s.code().unwrap_or(-1)),
        Err(e) => println!("cannot run fno: {e}"),
    }
    let _ = prompt("press enter");
}

/// The reviewed-marks sidecar, beside the mux's other sidecars.
pub(crate) fn store_path() -> std::path::PathBuf {
    #[cfg(not(test))]
    return crate::proto::mux_sidecar_path("decisions-reviewed.json");
    #[cfg(test)]
    std::env::temp_dir().join("decisions-reviewed.json")
}

pub fn run() -> i32 {
    let rows = match load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fno inbox decisions --table: {e}");
            return 1;
        }
    };
    let store = store_path();
    let mut t = Table {
        rows,
        reviewed: load_reviewed(&store),
        filter: Filter::default(),
        pick: 0,
    };
    let mut _screen = match crate::screen::Screen::enter() {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("fno inbox decisions --table: {e}");
            return 1;
        }
    };
    let mut buf = [0u8; 16];
    loop {
        let (w, h) = crossterm::terminal::size().unwrap_or((100, 30));
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[H\x1b[2J");
        let _ = out.write_all(render(&t, w as usize, h as usize).join("\r\n").as_bytes());
        let _ = out.flush();
        let n = std::io::stdin().read(&mut buf).unwrap_or(0);
        if n == 0 {
            return 0;
        }
        let key = parse_key(&buf[..n]);
        match key {
            Key::Quit => return 0,
            Key::Review => {
                if let Some(id) = t.selected().map(|r| r.decision_id.clone()) {
                    if mark_reviewed(&store, &id).is_ok() {
                        t.reviewed.insert(id);
                        t.apply(Key::Other);
                    }
                }
            }
            Key::Retract | Key::Overrule => {
                let Some(row) = t.selected().cloned() else {
                    continue;
                };
                _screen = None;
                // The header names the row by its number; the hash is an
                // internal key and never reaches the prompt.
                let head = match row.number.as_deref() {
                    Some(number) => format!("{number} {}: {}", row.subject, row.decision),
                    None => format!("{}: {}", row.subject, row.decision),
                };
                println!("{head}");
                let action = if key == Key::Retract {
                    prompt("reason").map(|reason| Action::Retract { reason })
                } else {
                    prompt("new text").map(|new_text| Action::Overrule { new_text })
                };
                if let Some(a) = action {
                    run_door(&row, &a);
                    if let Ok(rows) = load() {
                        t.rows = rows;
                    }
                }
                _screen = crate::screen::Screen::enter().ok();
            }
            k => t.apply(k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: &str, life: &str, ts: &str) -> DecisionRow {
        DecisionRow {
            decision_id: id.into(),
            lifecycle: life.into(),
            lane: "law".into(),
            ts: ts.into(),
            decision: "ship it".into(),
            question: Some("why ship?".into()),
            authority_source: Some("operator".into()),
            ..Default::default()
        }
    }

    fn table() -> Table {
        Table {
            rows: vec![
                r("a", "live", "2026-01-01"),
                r("b", "retracted", "2026-02-01"),
            ],
            reviewed: HashSet::new(),
            filter: Filter::default(),
            pick: 0,
        }
    }

    #[test]
    fn selection_clamps_when_a_filter_narrows_the_list() {
        assert_eq!(parse_key(b"r"), Key::Retract);
        assert_eq!(parse_key(b"z"), Key::Other);
        let mut t = table();
        t.apply(parse_key(b"\x1b[B"));
        assert_eq!(t.pick, 1);
        t.apply(Key::State);
        assert_eq!(t.pick, 0);
        assert_eq!(t.shown().len(), 1);
    }

    #[test]
    fn a_long_audience_chip_is_not_cut_mid_word() {
        let mut t = table();
        t.rows[0].audience = Some("customer:solo-founder".into());
        let page = render(&t, 140, 12).join("\n");
        assert!(
            page.contains("customer:solo-founder"),
            "chip cut in:\n{page}"
        );
        // A job that does not fit is dropped whole, never cut mid-word.
        t.rows[0].job = vec!["alpha".into(), "beta-long-job-name".into()];
        let jobs = render(&t, 140, 12).join("\n");
        assert!(jobs.contains("customer:solo-founder alpha"), "{jobs}");
        assert!(!jobs.contains("beta"), "job cut in:\n{jobs}");
        t.rows[0].job.clear();
        // A narrow pane keeps the ruling rather than widening the chip.
        let narrow = render(&t, 110, 12).join("\n");
        assert!(narrow.contains("ship it"), "ruling lost in:\n{narrow}");
    }
}
