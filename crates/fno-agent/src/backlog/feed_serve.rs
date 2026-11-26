//! `fno-agent feed --serve`: the activity panel's held reader. It holds the
//! whole history's rows and answers one JSON request per stdin line with one
//! JSON answer per stdout line (`fno_proto::feed::serve`). A child read of
//! this binary projects the rows, so the projection's working set (every
//! store it joins) is the child's and is gone when it exits. A thread reads
//! again every 15 seconds. The old rows drop when the new rows start to
//! stream, so two copies never coexist, and an answer waits only for that
//! parse. The reader exits when its stdin closes, so it
//! dies with the panel.

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

/// The `--limit` that reads every row, the panel's one-shot read's own.
const ALL_ROWS: &str = "10000000";

/// A running child read, past its projection: its first byte has arrived.
struct Read {
    child: std::process::Child,
    out: std::io::BufReader<std::process::ChildStdout>,
}

/// Start the printed read, `fno-agent feed --json`, in a child and wait for
/// its first byte. The child prints only after it has projected every row,
/// so the wait is the projection and the rows are still to be parsed.
/// `None` when the child fails first, so a refresh keeps the rows it has.
fn start_read(home: &AgentsHome) -> Option<Read> {
    use std::io::BufRead;
    let exe = std::env::current_exe().ok()?;
    let mut child = std::process::Command::new(exe)
        .args(["feed", "--json", "--limit", ALL_ROWS])
        .env(crate::paths::HOME_ENV, home.root())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = std::io::BufReader::new(child.stdout.take()?);
    if out.fill_buf().map(<[u8]>::is_empty).unwrap_or(true) {
        let _ = child.wait();
        return None;
    }
    Some(Read { child, out })
}

/// Parse the child's rows as they stream: the reader never holds the
/// projection or the JSON text, only the rows.
fn finish_read(read: Read) -> Option<Vec<FeedItem>> {
    let Read { mut child, out } = read;
    let rows = serde_json::from_reader::<_, Vec<FeedItem>>(out).ok();
    match child.wait() {
        Ok(status) if status.success() => rows,
        _ => None,
    }
}

/// Replace the held rows with `parse`'s. The old rows drop before `parse`
/// runs, so two copies never coexist, and the write lock spans the parse, so
/// an answer waits on the lock and then reads the new rows: it never reads
/// an empty history. Only the parse is under the lock, not the projection.
fn swap_in(held: &RwLock<Arc<Held>>, stamp: u64, parse: impl FnOnce() -> Option<Vec<FeedItem>>) {
    let mut slot = match held.write() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Arc::new(Held {
        items: Vec::new(),
        stamp,
    });
    let items = parse().unwrap_or_default();
    *slot = Arc::new(Held { items, stamp });
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
    let items = start_read(home).and_then(finish_read).unwrap_or_default();
    let held = Arc::new(RwLock::new(Arc::new(Held { items, stamp: 1 })));
    {
        let held = Arc::clone(&held);
        let home = home.clone();
        std::thread::spawn(move || {
            let mut stamp = 1;
            loop {
                std::thread::sleep(REFRESH_EVERY);
                let Some(read) = start_read(&home) else {
                    continue;
                };
                stamp += 1;
                swap_in(&held, stamp, || finish_read(read));
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

    /// A swap drops the old rows before it parses, and an answer that lands
    /// mid-swap waits on the lock: it reads the old rows or the new ones,
    /// never an empty history.
    #[test]
    fn an_answer_during_a_swap_never_reads_an_empty_history() {
        let rows = |n: usize| {
            (0..n)
                .map(|i| FeedItem {
                    ts: format!("2026-09-01T10:00:{i:02}Z"),
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        let held = Arc::new(RwLock::new(Arc::new(Held {
            items: rows(3),
            stamp: 1,
        })));
        let writer = {
            let held = Arc::clone(&held);
            std::thread::spawn(move || {
                swap_in(&held, 2, || {
                    std::thread::sleep(Duration::from_millis(300));
                    Some(rows(5))
                });
            })
        };
        let mut seen = std::collections::BTreeSet::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(600) {
            let cur = match held.read() {
                Ok(slot) => Arc::clone(&slot),
                Err(poisoned) => Arc::clone(&poisoned.into_inner()),
            };
            seen.insert(cur.items.len());
            std::thread::sleep(Duration::from_millis(5));
        }
        writer.join().expect("the swap thread finishes");
        assert!(!seen.contains(&0), "an empty history was read: {seen:?}");
        assert!(seen.contains(&5), "the new rows landed: {seen:?}");
    }

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
