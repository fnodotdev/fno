//! Server-instance telemetry answers (the scoreboard's counter read).
//!
//! The human_touch emission-failure counter lives and dies with the server
//! instance, so its answer carries the measurement window instead of posing
//! as an all-time fact. Reading never resets the counter.

use crate::mux_cli::{control_roundtrip, env_server, resolve_session, EXIT_ERROR, EXIT_OK};
use crate::proto::{self, ControlCommand, ServerMsg};

/// Current time as a `YYYY-MM-DDThh:mm:ssZ` UTC stamp (same shape the squad
/// store writes).
fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::squad_store::epoch_to_iso_public(secs)
}

/// Build the [`ServerMsg::ServerStats`] answer from live core state. Fields
/// are `pub(crate)` so this reader stays beside the wire shape it owns.
pub fn answer(touch_emit_failures: u64, started_at: &str) -> ServerMsg {
    ServerMsg::ServerStats {
        touch_emit_failures,
        started_at: started_at.to_string(),
        measured_at: now_iso(),
    }
}

/// The stats snapshot as one JSON string for the native scoreboard handoff
/// (`fno whoami scoreboard` passes it to the fno-agent door as
/// `FNO_MUX_STATS_JSON`, so the door reads the counter without a nested
/// `fno` spawn). None when no live server answers; the door then reports
/// Unknown, never zero.
pub fn snapshot_for_scoreboard() -> Option<String> {
    let session = resolve_session(None, env_server().as_deref());
    let sock = proto::socket_path(&session).ok()?;
    match control_roundtrip(&sock, &session, ControlCommand::ServerStats) {
        Ok(ServerMsg::ServerStats {
            touch_emit_failures,
            started_at,
            measured_at,
        }) => Some(
            serde_json::json!({
                "touch_emit_failures": touch_emit_failures,
                "started_at": started_at,
                "measured_at": measured_at,
            })
            .to_string(),
        ),
        _ => None,
    }
}

/// The window the paint reading covers.
const WINDOW_SECS: u64 = crate::paint_stats::DEFAULT_WINDOW_SECS;

/// The human line for the client paint reading.
fn paint_line(paint: Option<&crate::paint_stats::PaintReading>) -> String {
    match paint {
        Some(p) => format!(
            "frame paint: p50 {:.1} ms, p90 {:.1} ms, max {:.1} ms ({} frames, {} client(s), last {} s)",
            p.p50_ms, p.p90_ms, p.max_ms, p.frames, p.clients, p.window_secs
        ),
        None => format!("frame paint: no client painted in the last {WINDOW_SECS} s"),
    }
}

/// `fno mux diag stats [--json]`: server-instance telemetry over one
/// control roundtrip. Read-only; the counter is never reset by reading it.
pub fn cli(json: bool) -> i32 {
    let session = resolve_session(None, env_server().as_deref());
    let sock = match proto::socket_path(&session) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fno mux diag stats: {e}");
            return EXIT_ERROR;
        }
    };
    match control_roundtrip(&sock, &session, ControlCommand::ServerStats) {
        Ok(ServerMsg::ServerStats {
            touch_emit_failures,
            started_at,
            measured_at,
        }) => {
            let paint = crate::paint_stats::read_window(&session, WINDOW_SECS);
            if json {
                let payload = serde_json::json!({
                    "session": session,
                    "touch_emit_failures": touch_emit_failures,
                    "started_at": started_at,
                    "measured_at": measured_at,
                    "paint": paint,
                });
                println!("{payload}");
                EXIT_OK
            } else {
                println!(
                    "touch emission failures: {touch_emit_failures} (server instance since {started_at}; measured {measured_at})"
                );
                println!("{}", paint_line(paint.as_ref()));
                EXIT_OK
            }
        }
        Ok(other) => {
            eprintln!("fno mux diag stats: unexpected reply {other:?}");
            EXIT_ERROR
        }
        Err(e) => {
            eprintln!("fno mux diag stats: {e}");
            EXIT_ERROR
        }
    }
}

/// One UTC stamp, for the instance start recorded at server boot.
pub fn stamp_now() -> String {
    now_iso()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paint_line_names_p50_and_p90_or_says_none() {
        let reading = crate::paint_stats::PaintReading {
            frames: 12,
            clients: 1,
            window_secs: 300,
            p50_ms: 4.0,
            p90_ms: 9.5,
            max_ms: 31.0,
        };
        assert_eq!(
            paint_line(Some(&reading)),
            "frame paint: p50 4.0 ms, p90 9.5 ms, max 31.0 ms (12 frames, 1 client(s), last 300 s)"
        );
        assert!(paint_line(None).contains("no client painted"));
    }

    #[test]
    fn stamps_are_utc_iso_shape() {
        let stamp = now_iso();
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
        assert_eq!(stamp.as_bytes()[10], b'T');
    }
}
