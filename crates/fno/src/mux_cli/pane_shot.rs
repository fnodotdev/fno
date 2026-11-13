//! `fno mux pane run --shot <out.png>`: one screen capture taken through a mux pane.
//!
//! macOS grants Screen Recording to the responsible process, so a session whose shell
//! descends from an app without the grant cannot run `screencapture`. A pane on a mux
//! server started from a granted terminal app can. The shot writes that pane's argv
//! itself, sends the ordinary `PaneRun`, and reads the capture's exit status from a
//! file next to the output, because the pane closes when its command exits and the
//! status would otherwise be lost.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{control_roundtrip, ControlError, EXIT_ERROR, EXIT_OK};
use crate::proto::{ControlCommand, PanePlacement, ServerMsg};

const POLL: Duration = Duration::from_millis(200);
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

const REMEDY: &str = "start the mux from a terminal app that holds Screen Recording, or grant it \
in System Settings > Privacy & Security > Screen Recording";

/// What the capture covers: the whole display, a rectangle, or one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Area {
    Display,
    Rect(u32, u32, u32, u32),
    Window(u64),
}

/// One parsed `pane run --shot` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShotSpec {
    pub out: String,
    pub area: Area,
    pub timeout: Duration,
    pub cwd: Option<String>,
    pub placement: PanePlacement,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ShotError {
    #[error("--shot output directory {0} does not exist; create it first")]
    NoOutDir(String),
    #[error("could not resolve --shot path {path:?}: {why}")]
    BadOut { path: String, why: String },
    #[error("{0}")]
    Control(String),
    #[error("{0}")]
    Refused(String),
    #[error(
        "screen capture exited {status} on server {server:?}: it lacks Screen Recording; {REMEDY}"
    )]
    CaptureFailed { status: String, server: String },
    #[error(
        "capture on server {server:?} wrote no PNG within {secs}s (pane {pane} was killed); {REMEDY}"
    )]
    Timeout {
        server: String,
        secs: u64,
        pane: u64,
    },
    #[error("capture reported success on server {server:?} but {path} is not a PNG")]
    NotPng { server: String, path: String },
}

/// `x,y,w,h`: four non-negative integers, w and h above zero.
pub(crate) fn parse_rect(v: &str) -> Option<Area> {
    let parts: Vec<u32> = v
        .split(',')
        .map(|p| p.trim().parse::<u32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match parts[..] {
        [x, y, w, h] if w > 0 && h > 0 => Some(Area::Rect(x, y, w, h)),
        _ => None,
    }
}

/// The pane's shell argv. The out path rides as `$1`, so no quoting of it is needed.
fn capture_argv(area: &Area, out: &str) -> Vec<String> {
    let flag = match area {
        Area::Display => String::new(),
        Area::Rect(x, y, w, h) => format!(" -R{x},{y},{w},{h}"),
        Area::Window(id) => format!(" -l{id}"),
    };
    vec![
        "/bin/sh".into(),
        "-c".into(),
        format!("/usr/sbin/screencapture -x{flag} \"$1\"; echo $? > \"$1.status\""),
        "sh".into(),
        out.into(),
    ]
}

fn absolute_out(out: &str) -> Result<PathBuf, ShotError> {
    let p = Path::new(out);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| ShotError::BadOut {
                path: out.into(),
                why: e.to_string(),
            })?
            .join(p)
    };
    match abs.parent() {
        Some(dir) if dir.is_dir() => Ok(abs),
        Some(dir) => Err(ShotError::NoOutDir(dir.display().to_string())),
        None => Err(ShotError::BadOut {
            path: out.into(),
            why: "no parent directory".into(),
        }),
    }
}

fn status_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".status");
    PathBuf::from(s)
}

fn is_png(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == PNG_MAGIC
}

/// Polls the status file until it holds a number or `timeout` passes. An empty
/// file is a write in flight, so it reads as not yet.
fn await_status(status: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(status) {
            let t = text.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL);
    }
}

fn shoot(session: &str, sock: &Path, spec: ShotSpec) -> Result<PathBuf, ShotError> {
    let out = absolute_out(&spec.out)?;
    let status = status_path(&out);
    // A leftover file from an earlier run must never read as this capture's result.
    std::fs::remove_file(&out).ok();
    std::fs::remove_file(&status).ok();
    let out_str = out.to_string_lossy().into_owned();
    let cwd = super::pane_args::resolve_run_cwd(spec.cwd, std::env::current_dir().ok());
    let run = ControlCommand::PaneRun {
        cwd,
        argv: capture_argv(&spec.area, &out_str),
        cols: None,
        rows: None,
        claim: false,
        placement: spec.placement,
        worker: None,
        fno_id: None,
    };
    let pane = match control_roundtrip(sock, session, run) {
        Ok(ServerMsg::PaneSpawned { pane_id, .. }) => pane_id,
        Ok(ServerMsg::Err { msg, .. }) => return Err(ShotError::Refused(msg)),
        Ok(other) => {
            return Err(ShotError::Control(format!(
                "unexpected reply to the capture pane: {other:?}"
            )))
        }
        Err(
            ControlError::Fatal(e)
            | ControlError::Unanswered(e)
            | ControlError::FatalCode { msg: e, .. },
        ) => {
            return Err(ShotError::Control(format!(
                "no mux server answered on {session:?} ({e}); a shot never starts one"
            )))
        }
    };
    let Some(code) = await_status(&status, spec.timeout) else {
        control_roundtrip(
            sock,
            session,
            ControlCommand::PaneKill {
                pane,
                hand_off_to: None,
            },
        )
        .ok();
        std::fs::remove_file(&status).ok();
        return Err(ShotError::Timeout {
            server: session.into(),
            secs: spec.timeout.as_secs(),
            pane,
        });
    };
    std::fs::remove_file(&status).ok();
    if code != "0" {
        std::fs::remove_file(&out).ok();
        return Err(ShotError::CaptureFailed {
            status: code,
            server: session.into(),
        });
    }
    if !is_png(&out) {
        return Err(ShotError::NotPng {
            server: session.into(),
            path: out_str,
        });
    }
    Ok(out)
}

/// Runs one shot and returns the process exit code. Stdout is the absolute path
/// (or `{"path":..}` under `--json`); a refusal is one stderr line.
pub(crate) fn run(session: &str, sock: &Path, json: bool, spec: ShotSpec) -> i32 {
    match shoot(session, sock, spec) {
        Ok(path) => {
            if json {
                println!("{}", serde_json::json!({ "path": path }));
            } else {
                println!("{}", path.display());
            }
            EXIT_OK
        }
        Err(e) => {
            eprintln!("fno mux pane run --shot: {e}");
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_parses_four_integers_and_refuses_the_rest() {
        assert_eq!(
            parse_rect("600,320,1360,832"),
            Some(Area::Rect(600, 320, 1360, 832))
        );
        assert!(parse_rect("1,2,3").is_none());
        assert!(parse_rect("1,2,0,4").is_none());
        assert!(parse_rect("a,b,c,d").is_none());
    }

    #[test]
    fn argv_carries_the_out_path_as_a_positional() {
        let argv = capture_argv(&Area::Rect(1, 2, 3, 4), "/tmp/a b.png");
        let script = argv.get(2).map(String::as_str).unwrap_or_default();
        assert_eq!(argv.get(4).map(String::as_str), Some("/tmp/a b.png"));
        assert!(script.contains("-R1,2,3,4"));
        assert!(script.contains("echo $? > \"$1.status\""));
    }

    #[test]
    fn status_wait_reads_a_written_status_and_times_out_without_one() {
        let dir = std::env::temp_dir().join(format!("pane-shot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let st = dir.join("s.status");
        assert_eq!(await_status(&st, Duration::from_millis(250)), None);
        std::fs::write(&st, "1\n").unwrap();
        assert_eq!(
            await_status(&st, Duration::from_millis(250)),
            Some("1".into())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
