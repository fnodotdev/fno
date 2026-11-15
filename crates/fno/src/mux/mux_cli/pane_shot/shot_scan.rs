//! Capture-time leak scan for `pane run --shot`.
//!
//! The needles only the captured machine knows: its scutil names, its `$HOME`,
//! and any uuid-shaped session id in the pixels. The generic shapes a CI
//! runner can judge are the repo gate's job (`scripts/ci/check-image-leaks.sh`).

use std::path::Path;
use std::process::Command;

/// One leak finding: which needle class and the matched text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeakHit {
    pub(crate) kind: LeakKind,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeakKind {
    HostName,
    HomePath,
    SessionId,
}

impl std::fmt::Display for LeakHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self.kind {
            LeakKind::HostName => "host name",
            LeakKind::HomePath => "home path",
            LeakKind::SessionId => "session id",
        };
        write!(f, "{label}: {}", self.value)
    }
}

/// The needles only this machine can read. Every field is optional: a machine
/// may leave `HostName` unset or lack the reading commands.
pub(crate) struct MachineFacts {
    pub(crate) host_names: Vec<String>,
    pub(crate) home: Option<String>,
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
    let mut host_names: Vec<String> = Vec::new();
    for (cmd, args) in [
        ("scutil", vec!["--get", "ComputerName"]),
        ("scutil", vec!["--get", "LocalHostName"]),
        ("scutil", vec!["--get", "HostName"]),
        ("hostname", vec![]),
    ] {
        if let Some(v) = read_cmd(cmd, &args) {
            host_names.push(v);
        }
    }
    host_names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    MachineFacts {
        host_names,
        home: std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()),
    }
}

/// True for `8-4-4-4-12` hex, the shape of a fno session id. Hand-rolled:
/// `regex` is an optional dependency of this crate and a uuid needs none.
pub(crate) fn is_uuid_shape(token: &str) -> bool {
    if token.len() != 36 {
        return false;
    }
    let bytes = token.as_bytes();
    let dash = |i: usize| bytes[i] == b'-';
    if !(dash(8) && dash(13) && dash(18) && dash(23)) {
        return false;
    }
    token.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

/// The uuid-shaped tokens in OCR text. OCR noise can weld tokens together, so
/// the token read stays tolerant: surrounding text splits on non-hex-dash
/// characters and each candidate is judged whole.
pub(crate) fn find_session_ids(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut token = String::new();
    for c in text.chars() {
        if c == '-' || c.is_ascii_hexdigit() {
            token.push(c);
        } else {
            if is_uuid_shape(&token) {
                found.push(token.clone());
            }
            token.clear();
        }
    }
    if is_uuid_shape(&token) {
        found.push(token);
    }
    found
}

/// The pure decision: which needles appear in OCR text. Host names and the
/// home path match case-insensitively; a needle under four characters is
/// dropped so a two-letter host can never match half a word.
pub(crate) fn scan_text(text: &str, facts: &MachineFacts) -> Vec<LeakHit> {
    let lower = text.to_lowercase();
    let mut hits = Vec::new();
    for name in &facts.host_names {
        if name.chars().count() >= 4 && lower.contains(&name.to_lowercase()) {
            hits.push(LeakHit {
                kind: LeakKind::HostName,
                value: name.clone(),
            });
        }
    }
    if let Some(home) = &facts.home {
        if home.chars().count() >= 6 && lower.contains(&home.to_lowercase()) {
            hits.push(LeakHit {
                kind: LeakKind::HomePath,
                value: home.clone(),
            });
        }
    }
    for id in find_session_ids(text) {
        hits.push(LeakHit {
            kind: LeakKind::SessionId,
            value: id,
        });
    }
    hits
}

/// What the scan concluded about one capture.
pub(crate) enum ScanVerdict {
    Clean,
    Leaks(Vec<LeakHit>),
    Skipped(String),
}

/// OCRs the PNG with tesseract and judges it. A missing or failing tesseract
/// degrades visibly: the shot still succeeds and one stderr line says the
/// scan did not run, because the CI gate is the fail-closed layer and a
/// capture tool must not go down with its checker.
pub(crate) fn scan_shot(path: &Path) -> ScanVerdict {
    let facts = read_machine_facts();
    let base = std::env::temp_dir().join(format!("fno-shot-scan-{}", std::process::id()));
    let _ = std::fs::remove_file(format!("{}.txt", base.display()));
    let out = Command::new("tesseract").arg(path).arg(&base).output();
    match out {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ScanVerdict::Skipped("tesseract is not installed; install it with 'brew install tesseract' so shots are scanned".into())
        }
        Err(e) => ScanVerdict::Skipped(format!("tesseract could not start: {e}")),
        Ok(o) if !o.status.success() => ScanVerdict::Skipped(format!(
            "tesseract exited {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Ok(_) => {
            let txt_path = format!("{}.txt", base.display());
            match std::fs::read_to_string(&txt_path) {
                Ok(text) => {
                    let _ = std::fs::remove_file(&txt_path);
                    let hits = scan_text(&text, &facts);
                    if hits.is_empty() {
                        ScanVerdict::Clean
                    } else {
                        ScanVerdict::Leaks(hits)
                    }
                }
                Err(e) => ScanVerdict::Skipped(format!(
                    "tesseract wrote no readable text: {e}"
                )),
            }
        }
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
        let has = |k: LeakKind, v: &str| hits.iter().any(|h| h.kind == k && h.value == v);
        assert!(has(LeakKind::HostName, "iMac2D2"));
        assert!(has(LeakKind::HomePath, "/Users/demo-user"));
        assert!(has(
            LeakKind::SessionId,
            "852b322d-634f-422c-bd70-c6932a0919cd"
        ));
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
        assert!(!is_uuid_shape("1579aa56"));
        assert!(!is_uuid_shape("3c9a1e7"));
        assert!(!is_uuid_shape("852b322d-634f-422c-bd70-c6932a0919c")); // 35 chars
        assert!(!is_uuid_shape("3c9a1e73c9a1e73c9a1e73c9a1e73c9a1e73c9a1")); // a sha, no dashes
        assert!(is_uuid_shape("852b322d-634f-422c-bd70-c6932a0919cd"));
        assert!(is_uuid_shape("8F3C48BE-0366-40FB-A3C1-303E22DCBC0E")); // OCR case drift
    }

    #[test]
    fn short_or_empty_facts_never_match() {
        let f = facts(&["bb", ""], Some("/Users"));
        let text = "bb16 builds /Users/bb16 in one session";
        assert!(scan_text(text, &f).is_empty());
    }
}
