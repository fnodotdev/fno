//! The text a live lane types into a session. Only this module builds a [`TurnText`]: its header line is `` `@sender · id` `` (user law d-b720d23c), then a real new line, then the body. A lane takes a `&TurnText`, so no caller can hand a lane a bare body that lands with no header.
//!
//! Outside this crate a plain string cannot become a turn:
//!
//! ```compile_fail
//! let _ = fno_mail::turn::TurnText(String::from("hello"));
//! ```
//!
//! ```compile_fail
//! let s: fno_mail::turn::TurnText = String::from("hello").into();
//! ```
//!
//! A [`RawCommand`] is the other thing a raw door may type: one slash command, or one codex dollar verb (user law d-f6570dc9). A message never rides it.

use std::fmt;

/// Which spelling the header's sender takes. One data field per harness row in the capability contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderForm {
    /// `` `@candor · fmail-f7aa93` ``: the default. The `@` sits inside inline code, so mention pickers never fire.
    Mention,
    /// `` `candor · fmail-f7aa93` ``: for a harness whose composer check failed on the `@` form.
    Plain,
}

/// Why a header or a raw command refused to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnError {
    /// The sender is empty.
    EmptySender,
    /// The message id is empty.
    EmptyId,
    /// A field holds a backtick, the ` · ` separator, a new line or another control character, so it could forge a header field.
    Forges { field: &'static str, value: String },
    /// The raw text is not one slash command or one codex dollar verb.
    NotACommand(String),
}

impl fmt::Display for TurnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TurnError::EmptySender => write!(f, "a turn header needs a sender"),
            TurnError::EmptyId => write!(f, "a turn header needs a message id"),
            TurnError::Forges { field, value } => write!(
                f,
                "turn header {field} {value:?} holds a backtick, a separator or a control character; it could forge a header field"
            ),
            TurnError::NotACommand(text) => write!(
                f,
                "raw text {text:?} is not one slash command or one codex dollar verb; send a message through mail, not the raw door"
            ),
        }
    }
}

impl std::error::Error for TurnError {}

fn forges(value: &str) -> bool {
    value.contains('`') || value.contains(" · ") || value.chars().any(char::is_control)
}

fn checked(field: &'static str, value: &str) -> Result<String, TurnError> {
    if forges(value) {
        return Err(TurnError::Forges {
            field,
            value: value.to_string(),
        });
    }
    Ok(value.to_string())
}

/// A checked header line. Its fields cannot forge a second field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnHeader {
    form: HeaderForm,
    sender: String,
    id: String,
    subject: Option<String>,
}

impl TurnHeader {
    /// Check the fields. `subject` is the third field; the law form drops it when a body is present, and the caller decides that.
    pub fn new(
        form: HeaderForm,
        sender: &str,
        id: &str,
        subject: Option<&str>,
    ) -> Result<TurnHeader, TurnError> {
        if sender.trim().is_empty() {
            return Err(TurnError::EmptySender);
        }
        if id.trim().is_empty() {
            return Err(TurnError::EmptyId);
        }
        Ok(TurnHeader {
            form,
            sender: checked("sender", sender)?,
            id: checked("id", id)?,
            subject: subject.map(|s| checked("subject", s)).transpose()?,
        })
    }

    /// The header line, backticks included, no trailing new line.
    pub fn render(&self) -> String {
        let who = match self.form {
            HeaderForm::Mention => format!("@{}", self.sender),
            HeaderForm::Plain => self.sender.clone(),
        };
        match &self.subject {
            Some(subject) => format!("`{who} · {} · {subject}`", self.id),
            None => format!("`{who} · {}`", self.id),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// How a lane keeps the new line between header and body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineRule {
    /// A keystroke lane (a mux pane, a keeper): each new line run is typed as one LF byte, the non-submitting ctrl+j. The submit is a separate CR the lane sends after.
    NonSubmittingKey,
    /// A paneless claude session over control.sock: any new line parks the turn in the harness queue, so every whitespace run becomes one space and the turn absorbs mid-turn (user law d-5641722b).
    OneLine,
    /// A lane that carries the text as data, not keystrokes (codex `turn/start`, the daemon RPC, the switchboard): the text rides unchanged.
    Verbatim,
}

/// The text a lane types: a checked header, a new line, then the body. Built only here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnText(String);

impl TurnText {
    /// The law form: the header line, a real new line, then the body. An empty body gives the header line alone.
    pub fn compose(header: &TurnHeader, body: &str) -> TurnText {
        let line = header.render();
        if body.is_empty() {
            TurnText(line)
        } else {
            TurnText(format!("{line}\n{body}"))
        }
    }

    /// A stored turn read back (a bus row's body, for a retry). It parses only when its first line is a header the checks accept.
    pub fn parse(stored: &str) -> Result<TurnText, TurnError> {
        let (line, body) = stored.split_once('\n').unwrap_or((stored, ""));
        let not_a_turn = || TurnError::Forges {
            field: "header",
            value: line.to_string(),
        };
        let inner = line
            .strip_prefix('`')
            .and_then(|l| l.strip_suffix('`'))
            .ok_or_else(not_a_turn)?;
        let mut fields = inner.splitn(3, " · ");
        let who = fields.next().unwrap_or_default();
        let id = fields.next().ok_or_else(not_a_turn)?;
        let subject = fields.next();
        let (form, sender) = match who.strip_prefix('@') {
            Some(sender) => (HeaderForm::Mention, sender),
            None => (HeaderForm::Plain, who),
        };
        let header = TurnHeader::new(form, sender, id, subject)?;
        Ok(TurnText::compose(&header, body))
    }

    /// The stored form, real new lines kept.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The bytes a lane with `rule` types. A trailing new line run is dropped under every keystroke rule, so a submit lands against the last line.
    pub fn render(&self, rule: NewlineRule) -> String {
        match rule {
            NewlineRule::Verbatim => self.0.clone(),
            NewlineRule::OneLine => one_line(&self.0),
            NewlineRule::NonSubmittingKey => non_submitting(&self.0),
        }
    }
}

impl fmt::Display for TurnText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `NewlineRule::OneLine` keystrokes: every whitespace run becomes one space.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `NewlineRule::NonSubmittingKey` keystrokes: every new line run (`\r\n`, `\n`, `\r`) becomes one LF; a tab becomes a space; every other control character, ESC included, is dropped; a trailing new line run is cut.
pub fn non_submitting(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            other if other.is_control() => {}
            other => out.push(other),
        }
    }
    out
}

/// One slash command or one codex dollar verb: the only text a raw door types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommand(String);

impl RawCommand {
    /// Accept `/name ...` or `$name ...` on one line, the name opening with an ASCII letter. Surrounding whitespace is trimmed.
    pub fn parse(text: &str) -> Result<RawCommand, TurnError> {
        let trimmed = text.trim();
        let refuse = || TurnError::NotACommand(text.to_string());
        if trimmed.chars().any(char::is_control) {
            return Err(refuse());
        }
        let rest = trimmed
            .strip_prefix('/')
            .or_else(|| trimmed.strip_prefix('$'))
            .ok_or_else(refuse)?;
        if !rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return Err(refuse());
        }
        Ok(RawCommand(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> TurnHeader {
        TurnHeader::new(HeaderForm::Mention, "zenobia", "fmail-1a2b", None).unwrap()
    }

    #[test]
    fn pane_turn_types_ctrl_j_between_header_and_body() {
        let typed =
            TurnText::compose(&header(), "one\r\ntwo\n\n").render(NewlineRule::NonSubmittingKey);
        assert_eq!(typed, "`@zenobia · fmail-1a2b`\none\ntwo");
        assert!(
            !typed.contains('\r'),
            "a pane turn types no return: {typed:?}"
        );
    }

    #[test]
    fn control_sock_turn_is_one_line_and_verbatim_keeps_lines() {
        let turn = TurnText::compose(&header(), "one\n  two");
        assert_eq!(
            turn.render(NewlineRule::OneLine),
            "`@zenobia · fmail-1a2b` one two"
        );
        assert_eq!(turn.render(NewlineRule::Verbatim), turn.as_str());
    }

    #[test]
    fn a_stored_turn_parses_back_and_a_bare_body_does_not() {
        let turn = TurnText::compose(&header(), "line one\nline two");
        assert_eq!(turn.as_str(), "`@zenobia · fmail-1a2b`\nline one\nline two");
        assert_eq!(TurnText::parse(turn.as_str()), Ok(turn));
        let plain =
            TurnHeader::new(HeaderForm::Plain, "zenobia", "fmail-1a2b", Some("ping")).unwrap();
        let header_only = TurnText::compose(&plain, "");
        assert_eq!(header_only.as_str(), "`zenobia · fmail-1a2b · ping`");
        assert_eq!(TurnText::parse(header_only.as_str()), Ok(header_only));
        for stored in [
            "just a body",
            "`@zenobia`\nbody",
            "`@a · b · c · d`\nbody",
            "`@ · fmail-1`",
        ] {
            assert!(
                TurnText::parse(stored).is_err(),
                "{stored:?} must not parse"
            );
        }
    }

    #[test]
    fn header_refuses_fields_that_forge_a_second_field() {
        for (sender, id, subject) in [
            ("a`b", "fmail-1", None),
            ("a · b", "fmail-1", None),
            ("zenobia", "fmail-1\n`@x · y`", None),
            ("zenobia", "fmail-1", Some("hi\nthere")),
        ] {
            assert!(
                matches!(
                    TurnHeader::new(HeaderForm::Mention, sender, id, subject),
                    Err(TurnError::Forges { .. })
                ),
                "{sender:?} {id:?} {subject:?} must refuse"
            );
        }
        assert_eq!(
            TurnHeader::new(HeaderForm::Mention, " ", "fmail-1", None),
            Err(TurnError::EmptySender)
        );
        assert_eq!(
            TurnHeader::new(HeaderForm::Mention, "z", "", None),
            Err(TurnError::EmptyId)
        );
    }

    #[test]
    fn raw_command_takes_only_a_slash_command_or_a_dollar_verb() {
        assert_eq!(
            RawCommand::parse(" /compact ").unwrap().as_str(),
            "/compact"
        );
        assert_eq!(
            RawCommand::parse("$fno:target x-1").unwrap().as_str(),
            "$fno:target x-1"
        );
        for text in [
            "hello there",
            "/",
            "/ compact",
            "$5 please",
            "/compact\nand a message",
            "",
        ] {
            assert!(RawCommand::parse(text).is_err(), "{text:?} must refuse");
        }
    }
}
