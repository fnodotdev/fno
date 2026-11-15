//! Capture-time leak scan for `pane run --shot`.
//!
//! The needles only the captured machine knows: its scutil names, its `$HOME`,
//! and any uuid-shaped session id in the pixels. The generic shapes a CI
//! runner can judge are the repo gate's job (`scripts/ci/check-image-leaks.sh`).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A bounded child never wedges the shot the way a bare `.output()` can.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Tesseract's own budget. It took 7.3 s on a 1600x1686 capture, so the
/// 5 s read budget killed every full-size shot and the scan never ran.
const OCR_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(25);

/// Spawn, poll `try_wait`, kill at the deadline; drain piped stdout on exit.
fn run_bounded(command: &mut Command, budget: Duration) -> Option<std::process::Output> {
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _read = pipe.read_to_end(&mut stdout);
                }
                return Some(std::process::Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                child.kill().ok();
                child.wait().ok();
                return None;
            }
            Err(_) => return None,
        }
    }
}

/// The needles only this machine can read. Every field is optional: a machine
/// may leave `HostName` unset or lack the reading commands.
pub(crate) struct MachineFacts {
    host_names: Vec<String>,
    home: Option<String>,
}

/// One external read, trimmed; `None` when the command is missing, slow, or fails.
fn read_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = run_bounded(
        Command::new(cmd)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        SPAWN_TIMEOUT,
    )?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Reads the needles. A failed or empty read (no `HostName` set, a non-macOS
/// host without `scutil`) just contributes less.
pub(crate) fn read_machine_facts() -> MachineFacts {
    MachineFacts {
        host_names: ["ComputerName", "LocalHostName", "HostName"]
            .iter()
            .filter_map(|n| read_cmd("scutil", &["--get", n]))
            .collect(),
        home: std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()),
    }
}

/// The uuid-shaped tokens in OCR text: the session-id shape this crate's
/// backlog search already judges (`search_query::is_uuid`). Text splits on
/// non-hex-dash characters and each candidate is judged whole.
fn find_session_ids(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut token = String::new();
    for c in text.chars() {
        if c == '-' || c.is_ascii_hexdigit() {
            token.push(c);
        } else {
            if crate::backlog::search_query::is_uuid(&token) {
                found.push(token.clone());
            }
            token.clear();
        }
    }
    if crate::backlog::search_query::is_uuid(&token) {
        found.push(token);
    }
    found
}

/// The pure decision: which needles appear in OCR text. Host names and the
/// home path match case-insensitively; a needle under four characters is
/// dropped so a two-letter host can never match half a word. Every hit is
/// one formatted line ready for the refusal.
pub(crate) fn scan_text(text: &str, facts: &MachineFacts) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut hits = Vec::new();
    for name in &facts.host_names {
        if name.chars().count() >= 4 && lower.contains(&name.to_lowercase()) {
            hits.push(format!("host name: {name}"));
        }
    }
    if let Some(home) = &facts.home {
        if home.chars().count() >= 6
            && home[1..].contains('/')
            && lower.contains(&home.to_lowercase())
        {
            hits.push(format!("home path: {home}"));
        }
    }
    for id in find_session_ids(text) {
        hits.push(format!("session id: {id}"));
    }
    hits
}

/// What the scan concluded about one capture.
pub(crate) enum ScanVerdict {
    Clean,
    Leaks(Vec<String>),
    Skipped(String),
}

/// OCRs the PNG with tesseract and judges it. A missing or failing tesseract
/// degrades visibly: the shot still succeeds and one stderr line says the
/// scan did not run, because the CI gate is the fail-closed layer and a
/// capture tool must not go down with its checker.
pub(crate) fn scan_shot(path: &Path) -> ScanVerdict {
    let facts = read_machine_facts();
    let base = std::env::temp_dir().join(format!("fno-shot-scan-{}", std::process::id()));
    let txt = format!("{}.txt", base.display());
    std::fs::remove_file(&txt).ok();
    let started = Instant::now();
    let out = run_bounded(
        Command::new("tesseract")
            .arg(path)
            .arg(&base)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        OCR_TIMEOUT,
    );
    let text = match out {
        None if started.elapsed() >= OCR_TIMEOUT => {
            return ScanVerdict::Skipped(over_budget(path));
        }
        None => {
            return ScanVerdict::Skipped(
                "tesseract is not installed ('brew install tesseract'); the scan did not run"
                    .into(),
            );
        }
        Some(o) if !o.status.success() => {
            return ScanVerdict::Skipped(format!(
                "tesseract exited {}: the scan did not run",
                o.status
            ))
        }
        Some(_) => match std::fs::read_to_string(&txt) {
            Ok(t) => t,
            Err(e) => {
                return ScanVerdict::Skipped(format!("tesseract wrote no readable text: {e}"))
            }
        },
    };
    std::fs::remove_file(&txt).ok();
    let hits = scan_text(&text, &facts);
    if hits.is_empty() {
        ScanVerdict::Clean
    } else {
        ScanVerdict::Leaks(hits)
    }
}

/// The skip line for a tesseract that outran its budget: the budget and the
/// capture's pixel size, so the reader can tell a slow OCR from a wedge.
fn over_budget(path: &Path) -> String {
    let size = png_size(path)
        .map(|(w, h)| format!("{w}x{h}"))
        .unwrap_or_else(|| "unknown-size".into());
    format!(
        "tesseract ran past its {}s budget on a {size} capture; the scan did not run",
        OCR_TIMEOUT.as_secs()
    )
}

/// Width and height from a PNG's IHDR chunk (bytes 16..24, big-endian).
fn png_size(path: &Path) -> Option<(u32, u32)> {
    let mut head = [0u8; 24];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(head[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(head[20..24].try_into().ok()?);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(hosts: &[&str], home: Option<&str>) -> MachineFacts {
        MachineFacts {
            host_names: hosts.iter().map(|s| (*s).to_string()).collect(),
            home: home.map(|s| s.to_string()),
        }
    }

    #[test]
    fn over_budget_line_names_the_budget_and_the_pixel_size() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        let mut head = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        head.extend_from_slice(&1600u32.to_be_bytes());
        head.extend_from_slice(&1681u32.to_be_bytes());
        std::fs::write(&png, head).unwrap();
        let line = over_budget(&png);
        assert!(line.contains("30s budget"), "{line}");
        assert!(line.contains("1600x1681"), "{line}");
    }

    #[test]
    fn scan_flags_host_home_and_session_id_hits() {
        let f = facts(&["iMac2D2"], Some("/Users/demo-user"));
        let text = "The machine row says iMac2D2\n\
                    cwd /Users/demo-user/code/x\n\
                    session 852b322d-634f-422c-bd70-c6932a0919cd copy";
        let hits = scan_text(text, &f);
        assert!(hits.iter().any(|h| h == "host name: iMac2D2"));
        assert!(hits.iter().any(|h| h == "home path: /Users/demo-user"));
        assert!(hits
            .iter()
            .any(|h| h == "session id: 852b322d-634f-422c-bd70-c6932a0919cd"));
    }

    #[test]
    fn clean_demo_text_passes() {
        let f = facts(&["iMac2D2"], Some("/Users/demo-user"));
        let text = "demo-host  Idle  main\n\
                    cwd /tmp/fno-demo.Qw7/checkout\n\
                    12 nodes ready  3 panes";
        assert!(scan_text(text, &f).is_empty());
    }

    #[test]
    fn uuid_shape_rejects_shas_and_short_ids() {
        assert!(find_session_ids("1579aa12").is_empty());
        assert!(find_session_ids("3c9a1e7").is_empty());
        assert!(find_session_ids("852b322d-634f-422c-bd70-c6932a0919c").is_empty());
        assert!(find_session_ids("3c9a1e73c9a1e73c9a1e73c9a1e73c9a1e73c9a1").is_empty());
        assert_eq!(
            find_session_ids("8F3C48BE-0366-40FB-A3C1-303E22DCBC0E"),
            vec!["8F3C48BE-0366-40FB-A3C1-303E22DCBC0E"]
        );
    }

    #[test]
    fn short_or_empty_facts_never_match() {
        let f = facts(&["bb", ""], Some("/Users"));
        let text = "bb16 builds /Users/bb16 in one session";
        assert!(scan_text(text, &f).is_empty());
    }
}
