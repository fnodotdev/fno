//! The delivered header: the one reader for a mail turn's first line, moved
//! here from `fno-agent::runtime::mail_header` so every consumer (the agent,
//! the serve mappers, the desktop app) calls the same test instead of
//! carrying its own. The law form is `` `@sender · id` `` (d-b720d23c); the
//! legacy stored form keeps a third ` · subject` field, and a legacy turn
//! may continue on the same line through the " ⏎ " body separator or the
//! `NewlineRule::OneLine` single space ([`fno_mail::turn::one_line`]).
//!
//! Acceptance is deliberately LENIENT relative to [`fno_mail::turn`]:
//! `TurnText::parse` reads turns the store wrote back, while this module
//! reads whatever a transcript carries, including older shapes.

/// The fields a delivered header carries: the sender without its `@`, the
/// message id, and the subject when the legacy three-field form kept one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFields {
    pub sender: String,
    pub id: String,
    pub subject: Option<String>,
}

/// The header's inner span and what follows it: the opening backtick through
/// the first closing backtick whose remainder is the line's end, the
/// transcript's one-line body separator " ⏎ " (a legacy stored turn renders
/// on ONE physical line, header then separator then body), or one space -
/// the `NewlineRule::OneLine` keystrokes (`one_line`), which collapse every
/// whitespace run, the header's new line included, to one space; the law
/// form is two physical lines. `None` when no closer reads.
pub fn split_header_span(trimmed: &str) -> Option<(&str, &str)> {
    let rest = trimmed.strip_prefix('`')?;
    let mut from = 0;
    while let Some(rel) = rest[from..].find('`') {
        let close = from + rel;
        let tail = &rest[close + 1..];
        if tail.is_empty() || tail.starts_with(' ') {
            return Some((&rest[..close], tail));
        }
        from = close + 1;
    }
    None
}

/// The span's fields: sender, id, and an optional subject - the law's
/// two-field form carries none. A three-field span keeps the subject as
/// everything after the second separator, not the third split field: it
/// may itself carry a backtick or a " · " separator.
pub fn header_fields(inner: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut parts = inner.splitn(3, " · ");
    Some((parts.next()?, parts.next()?, parts.next()))
}

/// The line's header fields when the whole line reads as a delivered-mail
/// header: one backticked span of `sender · id` (the law form) or
/// `sender · id · subject` (the legacy stored form), the sender `@name` or
/// `name` with no spaces, the id `fmail-` plus 12 hex (the message-id form
/// the mux-messages group rules on) or a legacy `msg-…` token that still
/// resolves. The span closes at the line's end or before the " ⏎ " body
/// separator; a subject may carry a backtick or " · " and must be nonempty.
/// This is the reader's shape test and the forged-body detector.
pub fn header_of(line: &str) -> Option<HeaderFields> {
    let trimmed = line.trim();
    let (inner, _tail) = split_header_span(trimmed)?;
    let (sender, id, subject) = header_fields(inner)?;
    let bare = sender.strip_prefix('@').unwrap_or(sender);
    if bare.is_empty() || bare.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let id_ok = match id.strip_prefix("fmail-") {
        Some(hex) => hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        None => id.strip_prefix("msg-").is_some_and(|rest| !rest.is_empty()),
    };
    if !id_ok {
        return None;
    }
    if let Some(s) = subject {
        if s.trim().is_empty() {
            return None;
        }
    }
    Some(HeaderFields {
        sender: bare.to_string(),
        id: id.to_string(),
        subject: subject.map(|s| s.to_string()),
    })
}

/// True when the whole line reads as a delivered-mail header.
pub fn is_header_line(line: &str) -> bool {
    header_of(line).is_some()
}

/// True when any line of `body` is shaped like a delivered header. A send
/// carrying one refuses: a body cannot forge a second message's first line.
pub fn body_holds_header_line(body: &str) -> bool {
    body.lines().any(is_header_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn law_and_legacy_forms_parse_and_junk_refuses() {
        let law = header_of("`@candor · fmail-0badc0de1234`").unwrap();
        assert_eq!(law.sender, "candor");
        assert_eq!(law.id, "fmail-0badc0de1234");
        assert_eq!(law.subject, None);
        let plain = header_of("`candor · msg-1 · hi`").unwrap();
        assert_eq!(plain.sender, "candor");
        assert_eq!(plain.id, "msg-1");
        assert_eq!(plain.subject.as_deref(), Some("hi"));
        assert!(header_of("`candor · fmail-0badc0de1234` ⏎ body line").is_some());
        for junk in [
            "just a body",
            "`@candor · not-an-id`",
            "`@candor · msg-`",
            "`@can dor · fmail-0badc0de1234`",
            "`@candor · fmail-0badc0de1234`xtail",
            "`@candor · fmail-0badc0de1234 · `",
        ] {
            assert!(header_of(junk).is_none(), "{junk:?} must refuse");
        }
    }

    #[test]
    fn whitespace_and_midline_tails_hold() {
        assert!(is_header_line("  `@candor · fmail-0badc0de1234`  "));
        assert!(is_header_line("`@candor · fmail-0badc0de1234` tail"));
        assert!(!is_header_line(
            "typed words then `@candor · fmail-0badc0de1234`"
        ));
    }

    #[test]
    fn body_holds_only_when_a_whole_line_is_a_header() {
        assert!(body_holds_header_line(
            "lead in\n`@a · fmail-0badc0de1234`\ntrail"
        ));
        assert!(!body_holds_header_line(
            "lead in\nwords then `@a · fmail-0badc0de1234`\ntrail"
        ));
    }
}
