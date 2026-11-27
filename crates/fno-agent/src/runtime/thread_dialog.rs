//! What a parked claude thread's dialog says. A native `claude --bg` worker
//! has no mux pane, so the dialog is read off the attach frame stream: the
//! daemon redraws the screen to a fresh attacher. A numbered picker takes a
//! key. A macOS grant screen (Accessibility, Screen Recording) takes none; a
//! person has to act at the machine, and the answer path must say so instead
//! of typing a key the screen ignores.

use crate::mail_inject::claude_control::read_screen;
use crate::manifest::{load_manifest, AnswerablePrompt};
use crate::readiness::ScreenView;
use std::path::Path;

const GRANTS: [(&str, &str); 2] = [
    ("accessibility", "Accessibility"),
    ("screen recording", "Screen Recording"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dialog {
    /// Numbered options: a response key answers it.
    Picker,
    /// A system grant screen. No key answers it; the named grants are missing.
    Grant(Vec<&'static str>),
    Unknown,
}

pub(crate) fn classify(screen: &str) -> Dialog {
    let lower = screen.to_ascii_lowercase();
    let numbered = |n: u8| {
        screen.lines().any(|l| {
            let t = l.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
            t.strip_prefix(&n.to_string())
                .is_some_and(|rest| rest.starts_with(". ") || rest.starts_with(") "))
        })
    };
    let asks = ["system settings", "not granted"]
        .iter()
        .any(|w| lower.contains(w));
    let missing: Vec<&'static str> = GRANTS
        .iter()
        .filter(|(needle, _)| lower.contains(needle))
        .map(|(_, name)| *name)
        .collect();
    if asks && !missing.is_empty() {
        Dialog::Grant(missing)
    } else if numbered(1) && numbered(2) {
        Dialog::Picker
    } else {
        Dialog::Unknown
    }
}

/// Why a thread prompt was not answered. Only `Unconfirmed` and
/// `StillShowing` follow a typed key; every other variant typed nothing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum Refusal {
    #[error("{}", grant_remedy(.0))]
    Grant(Vec<&'static str>),
    #[error("no permission menu is showing: the bottom of the screen holds no \"Do you want to proceed?\" question over numbered options")]
    NoMenu,
    #[error("unknown action {0:?}")]
    UnknownAction(String),
    #[error("the showing menu has no {action} option ({menu})")]
    NoOption { action: String, menu: String },
    #[error("the showing menu has more than one {action} option ({menu})")]
    Ambiguous { action: String, menu: String },
    #[error("the dialog is unreadable: {0}")]
    Unreadable(String),
    #[error("the screen changed between the read and the key, so nothing was typed")]
    Changed,
    #[error("the control socket refused: {0}")]
    Wire(&'static str),
    #[error("the key was typed but the screen could not be re-read: {0}")]
    Unconfirmed(String),
    #[error("the key was typed and the same prompt still shows, so the answer did not take")]
    StillShowing,
}

impl Refusal {
    /// The exit code `ask --answer` reports: 3 for a grant screen, 2 for a
    /// refusal on a clean read, 12 for a wire, read or confirm failure.
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Refusal::Grant(_) => 3,
            Refusal::NoMenu
            | Refusal::UnknownAction(_)
            | Refusal::NoOption { .. }
            | Refusal::Ambiguous { .. }
            | Refusal::Changed => 2,
            _ => 12,
        }
    }
}

/// The permission menu the bottom of `screen` shows, read by the claude
/// manifest's own `permission_prompt` rule (the reader the pane lane uses):
/// the question, the `1..N` options anchored to the live bottom region, and a
/// fingerprint of that region. Numbered text elsewhere on the screen, in a
/// transcript or in a command, is not this menu.
pub(crate) fn read_menu(screen: &str) -> Option<AnswerablePrompt> {
    let manifest = load_manifest("claude", None)?.ok()?;
    let view = ScreenView {
        visible_text: screen,
        cursor_row: 0,
        cursor_col: 0,
        osc_title: None,
        osc_progress: None,
    };
    let (verdict, menu) = manifest.evaluate_answerable(&view)?;
    let menu = menu?;
    (verdict.state == "blocked"
        && verdict.rule_id == "permission_prompt"
        && menu
            .prompt
            .to_ascii_lowercase()
            .contains("do you want to proceed?"))
    .then_some(menu)
}

/// True when the claude manifest reads the screen as an idle, empty prompt
/// box. A menu, a working turn or an unreadable screen is not idle, so a
/// typed line never lands on one of them.
pub(crate) fn at_idle_prompt(screen: &str) -> bool {
    let Some(Ok(manifest)) = load_manifest("claude", None) else {
        return false;
    };
    let view = ScreenView {
        visible_text: screen,
        cursor_row: 0,
        cursor_col: 0,
        osc_title: None,
        osc_progress: None,
    };
    manifest
        .evaluate_answerable(&view)
        .is_some_and(|(verdict, _)| verdict.state == "idle" && verdict.rule_id == "live_prompt_box")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Once,
    Always,
    Deny,
}

/// What a label means. Only the labels claude prints for these three choices
/// count. An unknown label maps to nothing, so "Yes, allow reading from X
/// during this session" and "Yes, and bypass permissions" never read as a
/// one-time yes.
fn choice_of(label: &str) -> Option<Choice> {
    let l = label.trim().to_ascii_lowercase().replace('\u{2019}', "'");
    if l == "yes" {
        Some(Choice::Once)
    } else if l.starts_with("yes, and don't ask again") || l.starts_with("yes, always allow") {
        Some(Choice::Always)
    } else if l == "no" || l.starts_with("no, ") {
        Some(Choice::Deny)
    } else {
        None
    }
}

fn cut(text: &str, max: usize) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() <= max {
        clean
    } else {
        let head: String = clean.chars().take(max).collect();
        format!("{head}...")
    }
}

/// The options as `1. Yes | 2. No`, each label cut to 60 characters.
pub(crate) fn numbered(menu: &AnswerablePrompt) -> String {
    menu.options
        .iter()
        .map(|o| format!("{}. {}", o.idx, cut(&o.label, 60)))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The key for `action` (`allow_once`, `allow_always`, `deny`) on THIS menu.
/// A menu with no option of that kind, or two, is refused: a fixed digit
/// would press another option.
pub(crate) fn key_for(menu: &AnswerablePrompt, action: &str) -> Result<String, Refusal> {
    let want = match action {
        "allow_once" => Choice::Once,
        "allow_always" => Choice::Always,
        "deny" => Choice::Deny,
        other => return Err(Refusal::UnknownAction(other.to_string())),
    };
    let mut hits = menu
        .options
        .iter()
        .filter(|o| choice_of(&o.label) == Some(want));
    match (hits.next(), hits.next()) {
        (Some(o), None) => Ok(o.idx.clone()),
        (None, _) => Err(Refusal::NoOption {
            action: action.to_string(),
            menu: numbered(menu),
        }),
        _ => Err(Refusal::Ambiguous {
            action: action.to_string(),
            menu: numbered(menu),
        }),
    }
}

/// A token that looks like a secret: a long mixed token, a known key prefix,
/// or the value of a `NAME=value` whose name says credential.
fn redact_token(tok: &str) -> String {
    const PREFIXES: [&str; 5] = ["sk-", "ghp_", "gho_", "xox", "AKIA"];
    const NAMES: [&str; 6] = ["key", "token", "secret", "pass", "auth", "cred"];
    if let Some((name, _)) = tok.split_once('=') {
        let lower = name.to_ascii_lowercase();
        if NAMES.iter().any(|n| lower.contains(n)) {
            return format!("{name}=[redacted]");
        }
    }
    let mixed = tok.chars().any(|c| c.is_ascii_digit()) && tok.chars().any(char::is_alphabetic);
    if PREFIXES.iter().any(|p| tok.starts_with(p)) || (tok.chars().count() >= 24 && mixed) {
        return "[redacted]".to_string();
    }
    tok.to_string()
}

/// The one line of the prompt that names what is asked, for a check-in line:
/// the first line above the question that is not a header, cut to 80
/// characters, secret-looking tokens redacted, in single quotes. The screen
/// belongs to the worker, so it shows as quoted data and never as text.
pub(crate) fn asked_line(menu: &AnswerablePrompt) -> Option<String> {
    let line = menu.prompt.lines().map(str::trim).find(|l| {
        let low = l.to_ascii_lowercase();
        !low.contains("do you want to proceed")
            && !matches!(low.as_str(), "bash command" | "bash" | "command")
            && l.chars().any(char::is_alphanumeric)
    })?;
    // Redact every whole token first, then cut: a secret that straddles the
    // cut would otherwise leave its head behind as a short, unflagged token.
    let clean: String = line.chars().filter(|c| !c.is_control()).collect();
    let redacted = clean
        .split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("'{}'", cut(&redacted, 80).replace('\'', "\"")))
}

/// Answer the permission menu a thread shows: read it, choose the key from
/// the menu's own options, type the key on an attach that re-reads the screen
/// first, then re-read and confirm the prompt closed. Returns the key typed.
pub(crate) fn answer_prompt(
    daemon_dir: &Path,
    short: &str,
    action: &str,
) -> Result<String, Refusal> {
    let screen = read_thread(daemon_dir, short).map_err(Refusal::Unreadable)?;
    if let Dialog::Grant(missing) = classify(&screen) {
        return Err(Refusal::Grant(missing));
    }
    let menu = read_menu(&screen).ok_or(Refusal::NoMenu)?;
    let key = key_for(&menu, action)?;
    crate::mail_inject::type_key_if_screen_unchanged(daemon_dir, short, &key, |now| {
        read_menu(now).is_some_and(|m| m.fingerprint == menu.fingerprint)
    })
    .map_err(|e| match e {
        crate::mail_inject::TypeRefusal::Changed => Refusal::Changed,
        crate::mail_inject::TypeRefusal::Wire(w) => Refusal::Wire(w),
    })?;
    std::thread::sleep(std::time::Duration::from_millis(800));
    // The same fingerprint means the same prompt. A worker that re-asks the
    // identical command right after the key reads as still showing: the
    // answer is then reported unconfirmed, never as answered.
    match read_thread(daemon_dir, short) {
        Err(e) => Err(Refusal::Unconfirmed(e)),
        Ok(after) => match read_menu(&after) {
            Some(m) if m.fingerprint == menu.fingerprint => Err(Refusal::StillShowing),
            _ => Ok(key),
        },
    }
}

/// The remedy a grant dialog needs, in one sentence.
pub(crate) fn grant_remedy(missing: &[&str]) -> String {
    format!(
        "a macOS grant screen is showing ({} not granted). No key answers it: a person must grant it at the machine",
        missing.join(" and ")
    )
}

/// Read the dialog of the thread whose short id the roster names.
pub(crate) fn read_thread(daemon_dir: &Path, short: &str) -> Result<String, String> {
    let roster = crate::claude_roster::ClaudeRoster::load(&daemon_dir.join("roster.json"))
        .map_err(|e| format!("roster unreadable: {e:?}"))?;
    let worker = roster.find(short).ok_or("thread not in the roster")?;
    let sock = worker
        .resolve_control_sock()
        .ok_or("thread has no control socket")?;
    read_screen(
        &sock,
        worker.short_id(),
        crate::claude_roster::read_control_key_in(daemon_dir),
    )
}

/// A stand-in for a claude daemon's `control.sock`, so the answer path runs
/// against bytes on a real socket. It never touches a live session.
#[cfg(test)]
pub(crate) mod fake {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    pub(crate) struct FakeThread {
        pub dir: tempfile::TempDir,
        /// Every byte any attacher typed after its handshake.
        pub typed: Arc<Mutex<Vec<u8>>>,
    }

    impl FakeThread {
        pub(crate) fn typed(&self) -> Vec<u8> {
            self.typed.lock().unwrap().clone()
        }
    }

    /// Serve `screens` one per attach, in order; the last one repeats.
    pub(crate) fn serve(short: &str, screens: &[&str]) -> FakeThread {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pty")).unwrap();
        let roster = serde_json::json!({
            "proto": 1,
            "workers": {short: {
                "sessionId": format!("{short}-0000-0000-0000-000000000000"),
                "ptySock": root.join("pty/w.sock").to_string_lossy(),
                "cwd": "/w",
            }},
        });
        std::fs::write(root.join("roster.json"), roster.to_string()).unwrap();
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        let typed = Arc::new(Mutex::new(Vec::new()));
        let screens: Vec<String> = screens.iter().map(|s| s.replace('\n', "\r\n")).collect();
        let (seen, log) = (Arc::new(AtomicUsize::new(0)), typed.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let screen = screens
                    .get(n)
                    .or(screens.last())
                    .cloned()
                    .unwrap_or_default();
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    let mut hello = String::new();
                    if reader.read_line(&mut hello).unwrap_or(0) == 0 {
                        return;
                    }
                    let _ = writer.write_all(b"{\"ok\":true}\n");
                    let _ = writer.write_all(screen.as_bytes());
                    let mut buf = [0u8; 64];
                    while let Ok(k) = reader.read(&mut buf) {
                        if k == 0 {
                            break;
                        }
                        log.lock()
                            .unwrap()
                            .extend_from_slice(buf.get(..k).unwrap_or_default());
                    }
                });
            }
        });
        FakeThread { dir, typed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "Bash command\n\n  rm -rf build\n  Remove the build directory\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No";
    const THREE: &str = "Bash command\n\n  git push\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for git push commands in /w\n   3. No, and tell Claude what to do differently (esc)";
    const WRAPPED: &str = " Do you want to proceed?\n   1. Yes\n   2. Yes, and don't ask again for\n      Bash(cargo test:*)\n      Bash(git status:*)\n   3. No";

    #[test]
    fn a_menu_or_an_unreadable_screen_is_never_an_idle_prompt() {
        assert!(!at_idle_prompt(TWO));
        assert!(!at_idle_prompt(""));
    }

    #[test]
    fn a_numbered_picker_takes_a_key() {
        assert_eq!(classify(THREE), Dialog::Picker);
    }

    #[test]
    fn a_grant_screen_names_each_missing_grant_and_takes_no_key() {
        let s = "computer-use needs permission.\nGrant Accessibility and Screen Recording in System Settings.";
        let Dialog::Grant(missing) = classify(s) else {
            panic!("expected a grant dialog");
        };
        assert_eq!(missing, vec!["Accessibility", "Screen Recording"]);
        assert!(grant_remedy(&missing).contains("Accessibility and Screen Recording"));
    }

    #[test]
    fn plain_text_is_unknown() {
        assert_eq!(classify("working on it..."), Dialog::Unknown);
    }

    #[test]
    fn a_two_option_menu_maps_deny_to_its_own_number_and_refuses_always() {
        let m = read_menu(TWO).unwrap();
        assert_eq!(key_for(&m, "allow_once").unwrap(), "1");
        assert_eq!(key_for(&m, "deny").unwrap(), "2");
        assert!(matches!(
            key_for(&m, "allow_always"),
            Err(Refusal::NoOption { .. })
        ));
        assert_eq!(asked_line(&m).unwrap(), "'rm -rf build'");
    }

    #[test]
    fn a_three_option_menu_maps_each_action_to_its_number() {
        let m = read_menu(THREE).unwrap();
        assert_eq!(key_for(&m, "allow_once").unwrap(), "1");
        assert_eq!(key_for(&m, "allow_always").unwrap(), "2");
        assert_eq!(key_for(&m, "deny").unwrap(), "3");
    }

    #[test]
    fn a_wrapped_option_does_not_end_the_menu() {
        let m = read_menu(WRAPPED).unwrap();
        assert_eq!(m.options.len(), 3);
        assert_eq!(key_for(&m, "deny").unwrap(), "3");
    }

    #[test]
    fn only_the_plain_yes_is_a_one_time_allow() {
        let s = "Do you want to proceed?\n 1. Yes, allow reading from /x during this session\n 2. Yes, and bypass permissions\n 3. No";
        let m = read_menu(s).unwrap();
        assert!(matches!(
            key_for(&m, "allow_once"),
            Err(Refusal::NoOption { .. })
        ));
        assert_eq!(key_for(&m, "deny").unwrap(), "3");
        let dup = "Do you want to proceed?\n 1. Yes\n 2. Yes\n 3. No";
        assert!(matches!(
            key_for(&read_menu(dup).unwrap(), "allow_once"),
            Err(Refusal::Ambiguous { .. })
        ));
    }

    #[test]
    fn numbered_text_with_no_question_is_not_a_menu() {
        assert!(read_menu("Steps:\n 1. Yes\n 2. No").is_none());
        assert!(read_menu("1. Yes\n 2. No\nDo you want to proceed?").is_none());
        assert!(read_menu("working on it...").is_none());
    }

    #[test]
    fn an_old_menu_in_scrollback_is_not_the_live_menu() {
        let s = format!("{TWO}\n\nreply\nmore\nmore\nmore\nmore\nmore\nmore\nmore\nmore");
        assert!(read_menu(&s).is_none());
    }

    const IDLE: &str = "all done\n\n ❯ ";

    #[test]
    fn deny_on_a_two_option_menu_types_its_own_number_and_nothing_else() {
        let t = fake::serve("a1b2c3d4", &[TWO, TWO, IDLE]);
        let key = answer_prompt(t.dir.path(), "a1b2c3d4", "deny").unwrap();
        assert_eq!(key, "2");
        assert_eq!(t.typed(), b"2");
    }

    #[test]
    fn a_screen_that_changed_before_the_key_types_nothing() {
        let t = fake::serve("a1b2c3d4", &[TWO, THREE]);
        let err = answer_prompt(t.dir.path(), "a1b2c3d4", "allow_once").unwrap_err();
        assert_eq!(err, Refusal::Changed);
        assert!(t.typed().is_empty());
    }

    #[test]
    fn an_action_the_menu_lacks_types_nothing() {
        let t = fake::serve("a1b2c3d4", &[TWO]);
        let err = answer_prompt(t.dir.path(), "a1b2c3d4", "allow_always").unwrap_err();
        assert!(matches!(err, Refusal::NoOption { .. }), "{err}");
        assert!(t.typed().is_empty());
    }

    #[test]
    fn no_menu_and_an_unreadable_dialog_type_nothing() {
        let t = fake::serve("a1b2c3d4", &["working on it..."]);
        assert_eq!(
            answer_prompt(t.dir.path(), "a1b2c3d4", "deny").unwrap_err(),
            Refusal::NoMenu
        );
        assert!(t.typed().is_empty());
        let empty = tempfile::tempdir().unwrap();
        assert!(matches!(
            answer_prompt(empty.path(), "a1b2c3d4", "deny").unwrap_err(),
            Refusal::Unreadable(_)
        ));
    }

    #[test]
    fn a_grant_screen_is_refused_with_its_remedy_and_types_nothing() {
        let t = fake::serve(
            "a1b2c3d4",
            &["Grant Accessibility in System Settings.\n 1. Yes\n 2. No"],
        );
        let err = answer_prompt(t.dir.path(), "a1b2c3d4", "allow_once").unwrap_err();
        assert_eq!(err, Refusal::Grant(vec!["Accessibility"]));
        assert_eq!(err.exit_code(), 3);
        assert!(t.typed().is_empty());
    }

    #[test]
    fn a_prompt_that_still_shows_after_the_key_is_not_reported_answered() {
        let t = fake::serve("a1b2c3d4", &[TWO]);
        let err = answer_prompt(t.dir.path(), "a1b2c3d4", "allow_once").unwrap_err();
        assert_eq!(err, Refusal::StillShowing);
        assert_eq!(t.typed(), b"1");
    }

    #[test]
    fn a_secret_that_straddles_the_cut_leaves_no_head_behind() {
        let secret = "q9Zk3Lm7Xv2Bn8Wc4Rt6Yh1Jd5Fg0";
        let pad = "a".repeat(74);
        let s = format!(
            "Bash command\n  {pad} {secret} tail\n\n Do you want to proceed?\n 1. Yes\n 2. No"
        );
        let line = asked_line(&read_menu(&s).unwrap()).unwrap();
        assert!(!line.contains("q9Zk"), "{line}");
        assert!(line.contains("[reda"), "{line}");
    }

    #[test]
    fn the_asked_line_is_cut_and_redacts_secret_looking_tokens() {
        let s = "Bash command\n  curl TOKEN=abc123 sk-live-9f8e7d6c5b4a39281716\n\n Do you want to proceed?\n 1. Yes\n 2. No";
        let line = asked_line(&read_menu(s).unwrap()).unwrap();
        assert!(line.contains("TOKEN=[redacted]"), "{line}");
        assert!(!line.contains("sk-live"), "{line}");
        let long = format!(
            "Bash command\n  {}\n\n Do you want to proceed?\n 1. Yes\n 2. No",
            "ab ".repeat(60)
        );
        assert!(asked_line(&read_menu(&long).unwrap()).unwrap().len() < 100);
    }
}
