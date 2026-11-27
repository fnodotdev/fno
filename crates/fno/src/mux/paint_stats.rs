//! Client frame paint timing, read back by `fno mux diag stats`.
//!
//! The server never sees a paint, so each attached client keeps a ring of its
//! recent frame times and writes it to `<mux dir>/<session>.paint.<pid>.json`
//! (at most once per `FLUSH_EVERY_MS`). `diag stats` merges the live clients'
//! files over a window. A sidecar needs no wire-version bump, so a stale
//! server keeps working beside a new client.
//!
//! A frame's time runs from the start of `View::compose` to the end of the
//! stdout flush in `Compositor::draw`: it includes the row diff and any block
//! on a slow terminal, which is where lag shows up.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Samples kept per client; at 60 fps this is about 17 s of history, and the
/// window read takes whatever the ring still holds.
const RING: usize = 1024;
const FLUSH_EVERY_MS: u64 = 2000;
/// A compose stamp older than this belongs to a frame that never drew.
const STALE_COMPOSE: std::time::Duration = std::time::Duration::from_secs(2);
pub const DEFAULT_WINDOW_SECS: u64 = 300;

struct Ring {
    session: Option<String>,
    samples: VecDeque<(u64, u64)>,
    last_flush_ms: u64,
}

static RING_STATE: Mutex<Ring> = Mutex::new(Ring {
    session: None,
    samples: VecDeque::new(),
    last_flush_ms: 0,
});

thread_local! {
    static COMPOSE_START: Cell<Option<Instant>> = const { Cell::new(None) };
}

#[derive(Serialize, Deserialize)]
struct PaintFile {
    pid: u32,
    /// `(unix_ms, micros)` per painted frame.
    samples: Vec<(u64, u64)>,
}

/// One merged reading over the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaintReading {
    pub frames: usize,
    pub clients: usize,
    pub window_secs: u64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub max_ms: f64,
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn file_path(dir: &Path, session: &str, pid: u32) -> PathBuf {
    dir.join(format!("{session}.paint.{pid}.json"))
}

/// Name this client's session and drop files left by dead clients of it.
pub fn init(session: &str) {
    if session.is_empty() || session.contains('/') {
        return;
    }
    if let Ok(mut ring) = RING_STATE.lock() {
        ring.session = Some(session.to_string());
    }
    prune_dead(&crate::proto::mux_dir(), session);
}

/// Stamp the start of a frame; the next `record_draw` closes it.
pub fn mark_compose_start() {
    COMPOSE_START.with(|c| c.set(Some(Instant::now())));
}

/// Close one frame that began at the compose stamp (else at `draw_started`).
pub fn record_draw(draw_started: Instant) {
    let start = COMPOSE_START
        .with(Cell::take)
        .filter(|s| draw_started.saturating_duration_since(*s) < STALE_COMPOSE)
        .unwrap_or(draw_started);
    let micros = start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    let now = unix_ms();
    let Ok(mut ring) = RING_STATE.lock() else {
        return;
    };
    if ring.samples.len() == RING {
        ring.samples.pop_front();
    }
    ring.samples.push_back((now, micros));
    let Some(session) = ring.session.clone() else {
        return;
    };
    if now.saturating_sub(ring.last_flush_ms) < FLUSH_EVERY_MS {
        return;
    }
    ring.last_flush_ms = now;
    let file = PaintFile {
        pid: std::process::id(),
        samples: ring.samples.iter().copied().collect(),
    };
    drop(ring);
    write_file(&crate::proto::mux_dir(), &session, &file);
}

fn write_file(dir: &Path, session: &str, file: &PaintFile) {
    let Ok(body) = serde_json::to_vec(file) else {
        return;
    };
    let path = file_path(dir, session, file.pid);
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        std::fs::remove_file(&tmp).ok();
    }
}

/// The `(pid, path)` of every paint file for `session` in `dir`.
fn session_files(dir: &Path, session: &str) -> Vec<(u32, PathBuf)> {
    let prefix = format!("{session}.paint.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid = name
                .strip_prefix(&prefix)?
                .strip_suffix(".json")?
                .parse::<u32>()
                .ok()?;
            Some((pid, e.path()))
        })
        .collect()
}

fn prune_dead(dir: &Path, session: &str) {
    for (pid, path) in session_files(dir, session) {
        if i32::try_from(pid).is_ok_and(crate::proto::pid_confirmed_dead) {
            std::fs::remove_file(path).ok();
        }
    }
}

/// Nearest-rank percentile of an ascending slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).or(sorted.last()).copied().unwrap_or(0)
}

fn summarize(dir: &Path, session: &str, now_ms: u64, window_secs: u64) -> Option<PaintReading> {
    let floor = now_ms.saturating_sub(window_secs * 1000);
    let mut micros = Vec::new();
    let mut clients = 0;
    for (pid, path) in session_files(dir, session) {
        if i32::try_from(pid).is_ok_and(crate::proto::pid_confirmed_dead) {
            std::fs::remove_file(&path).ok();
            continue;
        }
        let Some(file) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<PaintFile>(&b).ok())
        else {
            continue;
        };
        let before = micros.len();
        micros.extend(file.samples.iter().filter(|s| s.0 >= floor).map(|s| s.1));
        clients += usize::from(micros.len() > before);
    }
    if micros.is_empty() {
        return None;
    }
    micros.sort_unstable();
    let ms = |us: u64| us as f64 / 1000.0;
    Some(PaintReading {
        frames: micros.len(),
        clients,
        window_secs,
        p50_ms: ms(percentile(&micros, 50)),
        p90_ms: ms(percentile(&micros, 90)),
        max_ms: ms(micros.last().copied().unwrap_or(0)),
    })
}

/// The merged paint reading for `session`'s live clients over the last
/// `window_secs`; `None` when no client painted in that window.
pub fn read_window(session: &str, window_secs: u64) -> Option<PaintReading> {
    summarize(&crate::proto::mux_dir(), session, unix_ms(), window_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(dir: &Path, session: &str, pid: u32, samples: Vec<(u64, u64)>) {
        write_file(dir, session, &PaintFile { pid, samples });
    }

    #[test]
    fn the_reading_merges_live_clients_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        // 1..=10 ms in the window; a dead client, another session and a
        // 900 ms sample older than the window must all stay out.
        // The stale sample sits below the window floor, so it stays out
        // whether or not a pid next to ours is alive on the host.
        let fresh: Vec<_> = (1..=10).map(|i| (900_000, i * 1000)).collect();
        put(dir.path(), "main", me, fresh);
        put(dir.path(), "main", me + 1_000_000, vec![(900_000, 5_000)]);
        put(dir.path(), "other", me, vec![(900_000, 777_000)]);
        let stale = vec![(1_000, 900_000)];
        put(dir.path(), "main", me + 1, stale);
        let got = summarize(dir.path(), "main", 1_000_000, 300).unwrap();
        assert_eq!(got.p50_ms, 5.0);
        assert_eq!(got.p90_ms, 9.0);
        assert_eq!(got.max_ms, 10.0);
        assert_eq!(got.window_secs, 300);
        assert_eq!((got.frames, got.clients), (10, 1));
    }

    #[test]
    fn a_window_with_no_frames_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "main", std::process::id(), vec![(1_000, 4_000)]);
        assert_eq!(summarize(dir.path(), "main", 900_000, 60), None);
    }
}
