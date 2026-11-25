//! `fno-agent fleet feed --serve`: the activity panel's held reader. It
//! projects the whole history once, holds it, and answers one JSON request
//! per stdin line with one JSON answer per stdout line
//! (`fno_proto::feed::serve`). A thread re-projects every 15 seconds and
//! swaps the held rows in one step, so an answer never waits on a refresh.
//! The reader exits when its stdin closes, so it dies with the panel.

use super::feed::serve_rows;
use crate::paths::AgentsHome;
use fno_proto::feed::row::FeedItem;
use fno_proto::feed::serve;
use std::io::{BufRead, Write};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// The client's own refresh cadence: a fresher hold than this is never read.
const REFRESH_EVERY: Duration = Duration::from_secs(15);

/// One projection, under the stamp an answer carries.
struct Held {
    items: Vec<FeedItem>,
    stamp: u64,
}

/// Project the history and read it as the panel's rows. The rows cross the
/// same JSON shape the printed read emits, so the reader and the printed
/// read can never disagree on a field.
fn load(home: &AgentsHome, stamp: u64) -> Held {
    let rows = serve_rows(home);
    let items = serde_json::to_string(&rows)
        .ok()
        .and_then(|json| serde_json::from_str::<Vec<FeedItem>>(&json).ok())
        .unwrap_or_default();
    Held { items, stamp }
}

/// The reader's width: one column per char. The capped columns hold ids,
/// kinds and names, which are ASCII in practice.
fn char_width(s: &str) -> usize {
    s.chars().count()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Serve until stdin closes. Exit 0 on a clean close, 1 when stdout breaks.
pub(crate) fn run(home: &AgentsHome) -> i32 {
    fno_proto::feed::grammar::set_sequential_tail(super::node_ref::is_sequential_tail);
    let held = Arc::new(RwLock::new(Arc::new(load(home, 1))));
    {
        let held = Arc::clone(&held);
        let home = home.clone();
        std::thread::spawn(move || {
            let mut stamp = 1;
            loop {
                std::thread::sleep(REFRESH_EVERY);
                stamp += 1;
                let fresh = Arc::new(load(&home, stamp));
                match held.write() {
                    Ok(mut slot) => *slot = fresh,
                    Err(poisoned) => *poisoned.into_inner() = fresh,
                }
            }
        });
    }
    serve_lines(std::io::stdin().lock(), std::io::stdout().lock(), &held)
}

/// Answer each request line from `input` on `out` until `input` ends.
/// Exit 0 at the end of input, 1 when `out` breaks.
fn serve_lines(input: impl BufRead, mut out: impl Write, held: &RwLock<Arc<Held>>) -> i32 {
    for line in input.lines() {
        let Ok(line) = line else {
            break;
        };
        let answer = match serde_json::from_str::<serve::Request>(&line) {
            Ok(req) => {
                let cur = match held.read() {
                    Ok(slot) => Arc::clone(&slot),
                    Err(poisoned) => Arc::clone(&poisoned.into_inner()),
                };
                serve::answer(&cur.items, cur.stamp, &req, now_secs(), char_width)
            }
            Err(e) => serve::Answer {
                error: Some(format!("bad request: {e}")),
                ..Default::default()
            },
        };
        let written = serde_json::to_writer(&mut out, &answer).is_ok()
            && writeln!(out).is_ok()
            && out.flush().is_ok();
        if !written {
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request in, one answer out carrying its id, and the end of
    /// input ends the reader: the panel's close is the reader's exit.
    #[test]
    fn serve_answers_each_line_and_exits_when_input_ends() {
        let item = |ts: &str, kind: &str| FeedItem {
            ts: ts.into(),
            kind: kind.into(),
            ..Default::default()
        };
        let held = RwLock::new(Arc::new(Held {
            items: vec![
                item("2026-09-01T10:00:00Z", "node_created"),
                item("2026-09-01T11:00:00Z", "question_asked"),
            ],
            stamp: 7,
        }));
        let mut view = serve::ViewSpec::of(&fno_proto::feed::table::TableView::default());
        view.filters
            .insert("kind".into(), vec!["question_asked".into()]);
        let req = serve::Request {
            id: 3,
            view,
            slots: true,
            rows: 10,
            ..Default::default()
        };
        let input = format!(
            "{}\n",
            serde_json::to_string(&req).expect("a request serializes")
        );
        let mut out = Vec::new();
        let code = serve_lines(input.as_bytes(), &mut out, &held);
        assert_eq!(code, 0, "the end of input is a clean exit");
        let text = String::from_utf8(out).expect("answers are utf-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "one answer per request: {text}");
        let answer: serve::Answer =
            serde_json::from_str(lines.first().copied().unwrap_or_default())
                .expect("an answer parses");
        assert_eq!((answer.id, answer.stamp, answer.total), (3, 7, 2));
        assert_eq!(
            answer.matched, 1,
            "the kind filter keeps the question row only"
        );
        assert_eq!(
            answer.items.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![1],
            "the window's rows ride with the slots"
        );
    }
}
