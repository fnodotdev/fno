//! Session ids: the typed address mail names a conversation by. `SessionId::parse` accepts three shapes: a uuid in 8-4-4-4-12 hex form (any case, stored lower-cased), opencode's `ses_` followed by ASCII alphanumerics (stored verbatim), and zcode's `sess_` followed by a uuid (stored lower-cased).
//!
//! An exactly-8-hex string is a display head, never an address: `parse` refuses it, and only `SessionId::short` mints one. The stored forms match the `identity_key` normalization, so an id and its hold-clock key agree.

use std::convert::TryFrom;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// True when `raw` is a uuid in the 8-4-4-4-12 hex form, any case.
pub fn is_uuid_shape(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &byte) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// A session id: the full address a mail recipient is reached at.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

impl SessionId {
    /// Parse one of the three shapes, or refuse a name, a head or a scope.
    pub fn parse(raw: &str) -> Result<SessionId, SessionIdError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(SessionIdError::Empty);
        }
        if is_uuid_shape(raw) {
            return Ok(SessionId(raw.to_ascii_lowercase()));
        }
        if let Some(tail) = raw.strip_prefix("ses_") {
            if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Ok(SessionId(raw.to_string()));
            }
            return Err(SessionIdError::Shape(raw.to_string()));
        }
        if let Some(tail) = raw.strip_prefix("sess_") {
            if is_uuid_shape(tail) {
                return Ok(SessionId(raw.to_ascii_lowercase()));
            }
            return Err(SessionIdError::Shape(raw.to_string()));
        }
        if raw.len() == 8 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SessionIdError::Head(raw.to_string()));
        }
        Err(SessionIdError::Shape(raw.to_string()))
    }

    /// The stored form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 8-character display form. A short form never parses back.
    pub fn short(&self) -> ShortForm {
        if let Some(tail) = self.0.strip_prefix("sess_") {
            let head = tail.get(..8).unwrap_or(tail);
            return ShortForm(format!("sess_{head}"));
        }
        if let Some(tail) = self.0.strip_prefix("ses_") {
            let head = tail.get(..8).unwrap_or(tail);
            return ShortForm(format!("ses_{head}"));
        }
        ShortForm(self.0[..8].to_string())
    }
}

impl FromStr for SessionId {
    type Err = SessionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SessionId::parse(s)
    }
}

impl TryFrom<String> for SessionId {
    type Error = SessionIdError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        SessionId::parse(&raw)
    }
}

impl From<SessionId> for String {
    fn from(id: SessionId) -> String {
        id.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The 8-character display form of a session id.
///
/// A short form never converts back to a full id:
///
/// ```compile_fail
/// use fno_mail::session::{SessionId, ShortForm};
///
/// let id = SessionId::parse("d9daf3e8-1111-2222-3333-444455556666").unwrap();
/// let short: ShortForm = id.short();
/// let _back: SessionId = SessionId::from(short);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShortForm(String);

impl ShortForm {
    /// The display form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ShortForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a session id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionIdError {
    Empty,
    Head(String),
    Shape(String),
}

impl fmt::Display for SessionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionIdError::Empty => f.write_str("a session id cannot be empty"),
            SessionIdError::Head(head) => {
                write!(
                    f,
                    "`{head}` is an 8-hex head: a display form, not an address"
                )
            }
            SessionIdError::Shape(raw) => write!(
                f,
                "`{raw}` is not a session id: expected a uuid, a `ses_` id or a `sess_` id"
            ),
        }
    }
}

impl std::error::Error for SessionIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_three_shapes_and_normalizes() {
        let cases = [
            (
                "D9DAF3E8-1111-2222-3333-444455556666",
                "d9daf3e8-1111-2222-3333-444455556666",
                "d9daf3e8",
            ),
            (
                "ses_e6c7845fdffewdITB5QzCw2jGw",
                "ses_e6c7845fdffewdITB5QzCw2jGw",
                "ses_e6c7845f",
            ),
            (
                "sess_0B1C2D3E-1111-2222-3333-444455556666",
                "sess_0b1c2d3e-1111-2222-3333-444455556666",
                "sess_0b1c2d3e",
            ),
        ];
        for (raw, stored, short) in cases {
            let id = SessionId::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(id.as_str(), stored);
            assert_eq!(id.short().as_str(), short);
            assert_eq!(id.to_string(), stored);
        }
    }

    #[test]
    fn parse_refuses_names_heads_and_scopes() {
        let cases = [
            ("", SessionIdError::Empty),
            ("zenobia", SessionIdError::Shape("zenobia".to_string())),
            (
                "fleet:leads",
                SessionIdError::Shape("fleet:leads".to_string()),
            ),
            ("d9daf3e8", SessionIdError::Head("d9daf3e8".to_string())),
            ("a1b2c3d4", SessionIdError::Head("a1b2c3d4".to_string())),
            (
                "not-a-uuid",
                SessionIdError::Shape("not-a-uuid".to_string()),
            ),
            (
                "g1b2c3d4-1111-2222-3333-444455556666",
                SessionIdError::Shape("g1b2c3d4-1111-2222-3333-444455556666".to_string()),
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(SessionId::parse(raw), Err(expected));
        }
    }
}
