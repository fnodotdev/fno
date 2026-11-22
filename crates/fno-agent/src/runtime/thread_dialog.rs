//! What a parked claude thread's dialog says. A native `claude --bg` worker
//! has no mux pane, so the dialog is read off the attach frame stream: the
//! daemon redraws the screen to a fresh attacher. A numbered picker takes a
//! key. A macOS grant screen (Accessibility, Screen Recording) takes none; a
//! person has to act at the machine, and the answer path must say so instead
//! of typing a key the screen ignores.

use crate::mail_inject::claude_control::read_screen;
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

/// One numbered picker as the screen shows it: the lines above the first
/// option (the command or safety check being asked about) and the options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Menu {
    pub context: Vec<String>,
    pub options: Vec<(u8, String)>,
}

fn numbered_line(line: &str) -> Option<(u8, String)> {
    let t = line.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    let rest = t.get(digits.len()..)?;
    let label = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((digits.parse().ok()?, label.trim().to_string()))
}

/// The bottom-most run of options numbered 1, 2, 3 ... in order. Older
/// numbered text higher on the screen is not this menu.
pub(crate) fn parse_menu(screen: &str) -> Option<Menu> {
    let lines: Vec<&str> = screen.lines().collect();
    let mut found: Option<(usize, Vec<(u8, String)>)> = None;
    let mut i = 0;
    while i < lines.len() {
        if lines.get(i).and_then(|l| numbered_line(l)).map(|(n, _)| n) != Some(1) {
            i += 1;
            continue;
        }
        let mut options = Vec::new();
        let mut j = i;
        while let Some((n, label)) = lines.get(j).and_then(|l| numbered_line(l)) {
            if usize::from(n) != options.len() + 1 {
                break;
            }
            options.push((n, label));
            j += 1;
        }
        if options.len() >= 2 {
            found = Some((i, options));
        }
        i = j.max(i + 1);
    }
    let (start, options) = found?;
    let context = lines
        .get(start.saturating_sub(8)..start)
        .unwrap_or_default()
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Some(Menu { context, options })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Once,
    Always,
    Deny,
}

fn choice_of(label: &str) -> Option<Choice> {
    let l = label.to_ascii_lowercase();
    let first = l.split(|c: char| !c.is_ascii_alphabetic()).next()?;
    match first {
        "no" => Some(Choice::Deny),
        "yes"
            if ["don't ask again", "do not ask again", "always", "allow all"]
                .iter()
                .any(|w| l.contains(w)) =>
        {
            Some(Choice::Always)
        }
        "yes" => Some(Choice::Once),
        _ => None,
    }
}

impl Menu {
    /// The key for `action` (`allow_once`, `allow_always`, `deny`) on THIS
    /// menu, or the reason none applies. A menu with no option of that kind,
    /// or two of them, is refused: a fixed digit would press another option.
    pub(crate) fn key_for(&self, action: &str) -> Result<String, String> {
        let want = match action {
            "allow_once" => Choice::Once,
            "allow_always" => Choice::Always,
            "deny" => Choice::Deny,
            other => return Err(format!("unknown action {other:?}")),
        };
        let mut hits = self
            .options
            .iter()
            .filter(|(_, label)| choice_of(label) == Some(want));
        match (hits.next(), hits.next()) {
            (Some((n, _)), None) => Ok(n.to_string()),
            (None, _) => Err(format!(
                "the showing menu has no {action} option ({})",
                self.numbered()
            )),
            _ => Err(format!(
                "the showing menu has more than one {action} option ({})",
                self.numbered()
            )),
        }
    }

    pub(crate) fn numbered(&self) -> String {
        self.options
            .iter()
            .map(|(n, l)| format!("{n}. {l}"))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// The asked-about text above the options, on one line.
    pub(crate) fn asked(&self) -> String {
        self.context.join(" / ")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_picker_takes_a_key() {
        let s = "Allow this tool?\n ❯ 1. Yes\n   2. Yes, always\n   3. No";
        assert_eq!(classify(s), Dialog::Picker);
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

    const TWO: &str =
        "Bash command\n  rm -rf build\n\nDo you want to proceed?\n ❯ 1. Yes\n   2. No";
    const THREE: &str = "Bash command\n  git push\n\nDo you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for git push commands\n   3. No, and tell Claude what to do differently (esc)";

    #[test]
    fn a_two_option_menu_maps_deny_to_its_own_number_and_refuses_always() {
        let m = parse_menu(TWO).unwrap();
        assert_eq!(m.key_for("allow_once").unwrap(), "1");
        assert_eq!(m.key_for("deny").unwrap(), "2");
        assert!(m
            .key_for("allow_always")
            .unwrap_err()
            .contains("no allow_always"));
        assert!(m.asked().contains("rm -rf build"));
    }

    #[test]
    fn a_three_option_menu_maps_each_action_to_its_number() {
        let m = parse_menu(THREE).unwrap();
        assert_eq!(m.key_for("allow_once").unwrap(), "1");
        assert_eq!(m.key_for("allow_always").unwrap(), "2");
        assert_eq!(m.key_for("deny").unwrap(), "3");
    }

    #[test]
    fn the_bottom_menu_wins_over_older_numbered_text() {
        let s = format!("1. old\n2. older\n{TWO}");
        assert_eq!(parse_menu(&s).unwrap().options.len(), 2);
        assert!(parse_menu("working on it...").is_none());
    }

    #[test]
    fn an_unrecognised_menu_is_refused_not_guessed() {
        let m = parse_menu("Pick\n 1. Retry\n 2. Skip").unwrap();
        assert!(m.key_for("deny").is_err());
        assert!(m.key_for("allow_once").is_err());
    }
}
