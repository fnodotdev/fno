//! Where a message goes: the four address forms and the host spelling rule.
//! Resolving a handle to a live registry row is not this module's job.

/// A parsed destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// A bare name or session id on this machine.
    Handle(String),
    /// `name@host`, with the host already in `normalize_host` form.
    Remote { name: String, host: String },
    /// `--to-lead <scope>`: whoever holds the lead role for the scope.
    Lead(String),
    /// `--to-project <project>`: the one live peer that answers for it.
    Project(String),
}

impl Address {
    /// Parse the positional target: a handle, or `name@host`.
    pub fn parse_target(target: &str) -> Result<Address, TargetError> {
        if !target.contains('@') {
            return Ok(Address::Handle(target.to_string()));
        }
        let (name, host) = split_target(target)?;
        Ok(Address::Remote {
            name,
            host: normalize_host(&host),
        })
    }
}

/// A cross-host target that is not `<name>@<host>` with one `@` and both
/// sides nonempty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetError {
    pub target: String,
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cross-host target {:?}: the form is <name>@<host>, one @, \
both sides nonempty; a host alias in ssh_config covers user@host spellings",
            self.target
        )
    }
}

impl std::error::Error for TargetError {}

impl From<TargetError> for String {
    fn from(e: TargetError) -> String {
        e.to_string()
    }
}

/// Split `<name>@<host>` at its single `@`. Both sides nonempty; two `@`
/// refuse (an ssh user@host spelling is not carried; an ssh_config Host
/// alias covers those).
pub fn split_target(target: &str) -> Result<(String, String), TargetError> {
    let refused = || TargetError {
        target: target.to_string(),
    };
    if target.matches('@').count() != 1 {
        return Err(refused());
    }
    let (name, host) = target.split_once('@').unwrap_or(("", ""));
    if name.is_empty() || host.is_empty() {
        return Err(refused());
    }
    Ok((name.to_string(), host.to_string()))
}

/// Hosts compare equal across machines that spell themselves differently
/// (`mac2.local` from gethostname, `mac2` from a user typing). One
/// normalizer runs on write and on pull (operator ruling, 2026-10-10).
pub fn normalize_host(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.strip_suffix(".local") {
        Some(base) if !base.is_empty() => base.to_string(),
        _ => lower,
    }
}

/// A mail id's hash form: `fmail-` plus exactly 12 hex digits.
pub fn is_mail_hash(id: &str) -> bool {
    id.strip_prefix("fmail-")
        .is_some_and(|hex| hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A mail id's number form: `fmail-` plus 1 to 7 decimal digits. The hash is
/// always 12 characters, so the two forms never overlap.
pub fn is_mail_number(id: &str) -> bool {
    id.strip_prefix("fmail-")
        .is_some_and(|n| (1..=7).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Either form of a delivered-mail id token.
pub fn is_mail_id(id: &str) -> bool {
    is_mail_hash(id) || is_mail_number(id)
}

#[cfg(test)]
mod mail_id_tests {
    use super::*;

    #[test]
    fn hash_and_number_forms_are_disjoint() {
        assert!(is_mail_hash("fmail-0badc0de1234"));
        assert!(!is_mail_number("fmail-0badc0de1234"));
        assert!(is_mail_number("fmail-87"));
        assert!(!is_mail_hash("fmail-87"));
        assert!(is_mail_id("fmail-87") && is_mail_id("fmail-123456789012"));
        assert!(!is_mail_number("fmail-12345678"));
        assert!(!is_mail_id("fmail-") && !is_mail_id("fmail-8x") && !is_mail_id("msg-87"));
    }
}
