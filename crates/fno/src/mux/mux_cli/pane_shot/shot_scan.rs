//! Capture-time leak scan for `pane run --shot`.
//!
//! The needles only the captured machine knows: its scutil names, its `$HOME`,
//! and any uuid-shaped session id in the pixels. The generic shapes a CI
//! runner can judge are the repo gate's job (`scripts/ci/check-image-leaks.sh`).

use std::path::Path;
use std::process::Command;

/// The needles only this machine can read. Every field is optional: a machine
/// may leave `HostName` unset or lack the reading commands.
pub(crate) struct MachineFacts {
    host_names: Vec<String>,
    home: Option<String>,
}

/// One external read, trimmed; `None` when the command is missing or fails.
fn read_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
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

/// The uuid-shaped tokens in OCR text: `8-4-4-4-12` hex, the shape of a fno
/// session id. Hand-rolled: `regex` is an optional dependency of this crate
/// and a uuid needs none. Text splits on non-hex-dash characters and each
/// candidate is judged whole.
fn find_session_ids(text: &str) -> Vec<String> {
    fn shape_ok(token: &str) -> bool {
        token.len() == 36
            && token.as_bytes()[8] == b'-'
            && token.as_bytes()[13] == b'-'
            && token.as_bytes()[18] == b'-'
            && token.as_bytes()[23] == b'-'
            && token.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
    }
    let mut found = Vec::new();
    let mut token = String::new();
    for c in text.chars() {
        if c == '-' || c.is_ascii_hexdigit() {
            token.push(c);
        } else {
            if shape_ok(&token) {
                found.push(token.clone());
            }
            token.clear();
        }
    }
    if shape_ok(&token) {
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
        if home.chars().count() >= 6 && lower.contains(&home.to_lowercase()) {
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
    let _ = std::fs::remove_file(&txt);
    let out = Command::new("tesseract").arg(path).arg(&base).output();
    let text = match out {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ScanVerdict::Skipped(
                "tesseract is not installed; install it with 'brew install tesseract' so shots are scanned".into(),
            );
        }
        Err(e) => return ScanVerdict::Skipped(format!("tesseract could not start: {e}")),
        Ok(o) => {
            if !o.status.success() {
                return ScanVerdict::Skipped(format!(
                    "tesseract exited {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            match std::fs::read_to_string(&txt) {
                Ok(t) => t,
                Err(e) => {
                    return ScanVerdict::Skipped(format!("tesseract wrote no readable text: {e}"))
                }
            }
        }
    };
    let _ = std::fs::remove_file(&txt);
    let hits = scan_text(&text, &facts);
    if hits.is_empty() {
        ScanVerdict::Clean
    } else {
        ScanVerdict::Leaks(hits)
    }
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
